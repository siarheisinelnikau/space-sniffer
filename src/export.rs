//! CSV inventory export for a selected subtree.
//!
//! Every entry is included, including zero-byte and filtered-out entries. The
//! `bytes` column contains the original scan size and `visible_bytes` contains
//! the current filter's size. Directory values aggregate their descendants, so
//! summing all rows would count descendant bytes more than once. Paths use UTF-8
//! (with replacement characters for paths the platform cannot represent as
//! UTF-8), and potentially executable spreadsheet cells receive a leading `'`.

use crate::model::{EntryKind, NodeId, ScanTree};
use std::{
    fs::File,
    io::{self, BufWriter, Write},
    path::Path,
};

/// Write the selected root and its entire subtree in depth-first order.
/// Invalid roots, weight lengths, or malformed subtree links return
/// `InvalidInput` before the destination file is created or overwritten.
pub fn write_csv(path: &Path, tree: &ScanTree, root: NodeId, weights: &[u64]) -> io::Result<()> {
    if root >= tree.nodes.len() {
        return Err(invalid_input(
            "The report root is outside the scanned tree.",
        ));
    }
    if weights.len() != tree.nodes.len() {
        return Err(invalid_input(
            "The report needs one visible byte count for every scanned entry.",
        ));
    }

    let mut order = Vec::new();
    let mut stack = vec![root];
    let mut visited = vec![false; tree.nodes.len()];
    while let Some(index) = stack.pop() {
        if index >= tree.nodes.len() {
            return Err(invalid_input(
                "The subtree contains an invalid child index.",
            ));
        }
        if visited[index] {
            return Err(invalid_input(
                "The subtree contains a cycle or an entry with multiple parents.",
            ));
        }
        visited[index] = true;
        order.push(index);
        stack.extend(tree.nodes[index].children.iter().rev().copied());
    }

    let mut writer = BufWriter::new(File::create(path)?);
    writer.write_all(b"path,kind,bytes,visible_bytes\r\n")?;
    for index in order {
        let node = &tree.nodes[index];
        write_path_cell(&mut writer, &node.path.to_string_lossy())?;
        let kind = match node.kind {
            EntryKind::Directory => "directory",
            EntryKind::File => "file",
            EntryKind::Symlink => "symlink",
        };
        write!(writer, ",{kind},{},{}\r\n", node.size, weights[index])?;
    }
    writer.flush()
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn write_path_cell(writer: &mut impl Write, value: &str) -> io::Result<()> {
    // Check the start after spaces too, since spreadsheet importers may discard
    // leading whitespace before interpreting a formula.
    let dangerous_start = |text: &str| {
        matches!(
            text.chars().next(),
            Some('=' | '+' | '-' | '@' | '\t' | '\r' | '\n')
        )
    };
    let protect = dangerous_start(value) || dangerous_start(value.trim_start_matches(' '));
    let quote = value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\r' | b'\n'));
    if quote {
        writer.write_all(b"\"")?;
    }
    if protect {
        writer.write_all(b"'")?;
    }
    if quote {
        let mut pieces = value.split('"');
        if let Some(first) = pieces.next() {
            writer.write_all(first.as_bytes())?;
        }
        for piece in pieces {
            writer.write_all(b"\"\"")?;
            writer.write_all(piece.as_bytes())?;
        }
        writer.write_all(b"\"")?;
    } else {
        writer.write_all(value.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, ScanStats};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    struct TestFile(PathBuf);

    impl TestFile {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                "space-sniffer-report-{}-{}.csv",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn node(path: &str, size: u64, kind: EntryKind, parent: Option<NodeId>) -> Node {
        Node {
            name: path.into(),
            path: PathBuf::from(path),
            kind,
            size,
            children: Vec::new(),
            parent,
            modified: None,
        }
    }

    fn tree() -> ScanTree {
        let mut nodes = vec![
            node("root", 15, EntryKind::Directory, None),
            node("root/nested", 10, EntryKind::Directory, Some(0)),
            node("root/nested/данные.txt", 10, EntryKind::File, Some(1)),
            node("root/hidden.txt", 5, EntryKind::File, Some(0)),
            node("root/empty.txt", 0, EntryKind::File, Some(0)),
        ];
        nodes[0].children = vec![1, 3, 4];
        nodes[1].children = vec![2];
        ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: 3,
                directories: 2,
                skipped: 0,
                symlinks: 0,
                elapsed: Duration::ZERO,
            },
        }
    }

    #[test]
    fn export_keeps_filtered_and_zero_byte_entries_in_subtree_order() {
        let output = TestFile::new();
        write_csv(&output.0, &tree(), 0, &[10, 10, 10, 0, 0]).unwrap();
        assert_eq!(
            fs::read_to_string(&output.0).unwrap(),
            concat!(
                "path,kind,bytes,visible_bytes\r\n",
                "root,directory,15,10\r\n",
                "root/nested,directory,10,10\r\n",
                "root/nested/данные.txt,file,10,10\r\n",
                "root/hidden.txt,file,5,0\r\n",
                "root/empty.txt,file,0,0\r\n"
            )
        );
        write_csv(&output.0, &tree(), 1, &[10, 10, 10, 0, 0]).unwrap();
        assert_eq!(
            fs::read_to_string(&output.0).unwrap(),
            concat!(
                "path,kind,bytes,visible_bytes\r\n",
                "root/nested,directory,10,10\r\n",
                "root/nested/данные.txt,file,10,10\r\n"
            )
        );
    }

    #[test]
    fn csv_cells_escape_quotes_commas_and_embedded_newlines() {
        let mut output = Vec::new();
        write_path_cell(&mut output, "folder/hello,\"world\"\r\nfile.txt").unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\"folder/hello,\"\"world\"\"\r\nfile.txt\""
        );
    }

    #[test]
    fn spreadsheet_formulas_are_prefixed_before_csv_escaping() {
        for (input, expected) in [
            ("=1+1", "'=1+1"),
            ("+SUM(A1)", "'+SUM(A1)"),
            ("-1", "'-1"),
            ("@command", "'@command"),
            ("\tcommand", "'\tcommand"),
            ("\rcommand", "\"'\rcommand\""),
            (" =SUM(1,2)", "\"' =SUM(1,2)\""),
            ("ordinary.txt", "ordinary.txt"),
        ] {
            let mut output = Vec::new();
            write_path_cell(&mut output, input).unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
    }

    #[test]
    fn invalid_input_does_not_overwrite_an_existing_destination() {
        let output = TestFile::new();
        fs::write(&output.0, "keep me").unwrap();
        let mut tree = tree();
        for result in [
            write_csv(&output.0, &tree, 99, &[0; 5]),
            write_csv(&output.0, &tree, 0, &[]),
        ] {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
        tree.nodes[1].children.push(99);
        assert_eq!(
            write_csv(&output.0, &tree, 0, &[0; 5]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        tree.nodes[1].children = vec![0];
        assert_eq!(
            write_csv(&output.0, &tree, 0, &[0; 5]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read_to_string(&output.0).unwrap(), "keep me");
    }
}
