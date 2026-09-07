//! The in-memory result of a filesystem scan.
//!
//! Sizes are logical file lengths (`Metadata::len`), not allocated disk space.
//! Each hard-link path is counted independently. Directory sizes are the sum of
//! scanned regular files below them; links and reparse points contribute zero.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub type NodeId = usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub children: Vec<NodeId>,
    pub parent: Option<NodeId>,
    pub modified: Option<SystemTime>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

#[derive(Clone, Default, Debug)]
pub struct ScanStats {
    /// Regular-file paths visited, including separate paths to hard-linked files.
    pub files: u64,
    /// Directories found, including the root and any unreadable directories.
    pub directories: u64,
    /// Entries that could not be read, or unsupported special-file entries.
    pub skipped: u64,
    /// Symbolic links and Windows reparse points; these are never followed.
    pub symlinks: u64,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
pub struct ScanTree {
    /// An arena: children and parents refer to indices in this vector.
    pub nodes: Vec<Node>,
    pub root: NodeId,
    pub stats: ScanStats,
}

impl ScanTree {
    pub fn total_size(&self) -> u64 {
        self.nodes[self.root].size
    }
}
