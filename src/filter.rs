//! Filename and size filters for the treemap.
//!
//! Semicolon-separated clauses are combined with AND. Plain text is a
//! case-insensitive substring; text containing `*` or `?` is a glob matching the
//! entire basename. A leading `|` excludes a filename pattern. `>` and `<`
//! compare a file's size strictly, using decimal units (`MB`) or IEC units
//! (`MiB`). Fractional quantities are accepted when they resolve to whole bytes.

use crate::model::{Node, ScanTree};

#[derive(Clone, Debug, Default)]
pub struct Filter {
    clauses: Vec<Clause>,
}

#[derive(Clone, Debug)]
enum Clause {
    Name {
        pattern: String,
        glob: Option<Vec<char>>,
        exclude: bool,
    },
    LargerThan(u64),
    SmallerThan(u64),
}

impl Filter {
    /// Parse an empty query, filename patterns, exclusions, and size clauses.
    pub fn parse(input: &str) -> Result<Self, String> {
        if input.trim().is_empty() {
            return Ok(Self::default());
        }

        let mut clauses = Vec::new();
        for (index, raw) in input.split(';').enumerate() {
            let term = raw.trim();
            let fail = |message: &str| format!("Filter clause {}: {message}", index + 1);
            if term.is_empty() {
                return Err(fail(
                    "expected a pattern or size; empty clauses are not allowed.",
                ));
            }

            if let Some(quantity) = term.strip_prefix('>') {
                let bytes = parse_size(quantity).map_err(|message| fail(&message))?;
                clauses.push(Clause::LargerThan(bytes));
            } else if let Some(quantity) = term.strip_prefix('<') {
                let bytes = parse_size(quantity).map_err(|message| fail(&message))?;
                clauses.push(Clause::SmallerThan(bytes));
            } else {
                let (pattern, exclude) = match term.strip_prefix('|') {
                    Some(pattern) => (pattern.trim(), true),
                    None => (term, false),
                };
                if pattern.is_empty() {
                    return Err(fail("an exclusion needs a filename pattern after '|'."));
                }
                let pattern = pattern.to_lowercase();
                let glob = pattern
                    .chars()
                    .any(|character| matches!(character, '*' | '?'))
                    .then(|| pattern.chars().collect());
                clauses.push(Clause::Name {
                    pattern,
                    glob,
                    exclude,
                });
            }
        }
        Ok(Self { clauses })
    }

    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// Whether a file or symlink passes every clause. Directories do not match:
    /// their visible size comes from their matching descendants instead.
    pub fn matches(&self, node: &Node) -> bool {
        if node.is_dir() {
            return false;
        }
        let mut lowercase_name = None;
        let mut name_characters = None;
        self.clauses.iter().all(|clause| match clause {
            Clause::LargerThan(bytes) => node.size > *bytes,
            Clause::SmallerThan(bytes) => node.size < *bytes,
            Clause::Name {
                pattern,
                glob,
                exclude,
            } => {
                let name = lowercase_name.get_or_insert_with(|| node.name.to_lowercase());
                let matched = match glob {
                    Some(pattern) => {
                        let characters =
                            name_characters.get_or_insert_with(|| name.chars().collect::<Vec<_>>());
                        glob_matches(pattern, characters)
                    }
                    None => name.contains(pattern.as_str()),
                };
                matched != *exclude
            }
        })
    }

    /// Compute visible bytes for every node, aggregating matching leaves into
    /// their ancestors. The arena must place each parent before its children.
    /// An empty filter preserves the scan's recorded sizes exactly.
    pub fn weights(&self, tree: &ScanTree) -> Vec<u64> {
        if self.is_empty() {
            return tree.nodes.iter().map(|node| node.size).collect();
        }

        let mut weights = vec![0_u64; tree.nodes.len()];
        for (index, node) in tree.nodes.iter().enumerate().rev() {
            if self.matches(node) {
                weights[index] = node.size;
            }
            if let Some(parent) = node.parent {
                weights[parent] = weights[parent].saturating_add(weights[index]);
            }
        }
        weights
    }
}

/// Match Unicode scalar values, so `?` consumes one character rather than one
/// byte. The most recent `*` supplies a retry position without recursion.
fn glob_matches(pattern: &[char], name: &[char]) -> bool {
    let (mut pattern_index, mut name_index) = (0, 0);
    let (mut star, mut retry_name) = (None, 0);
    while name_index < name.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == name[name_index])
        {
            pattern_index += 1;
            name_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            pattern_index += 1;
            retry_name = name_index;
        } else if let Some(star_index) = star {
            retry_name += 1;
            name_index = retry_name;
            pattern_index = star_index + 1;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn parse_size(input: &str) -> Result<u64, String> {
    let input = input.trim();
    let number_end = input
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(input.len());
    let (number, suffix) = input.split_at(number_end);
    if number.is_empty() || !number.chars().any(|character| character.is_ascii_digit()) {
        return Err("expected a nonnegative size, for example >10MB or <1.5GiB.".into());
    }
    let multiplier: u128 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "p" | "pb" => 1_000_000_000_000_000,
        "e" | "eb" => 1_000_000_000_000_000_000,
        "ki" | "kib" => 1 << 10,
        "mi" | "mib" => 1 << 20,
        "gi" | "gib" => 1 << 30,
        "ti" | "tib" => 1 << 40,
        "pi" | "pib" => 1 << 50,
        "ei" | "eib" => 1 << 60,
        _ => {
            return Err(
                "unknown size unit; use B, KB, MB, GB, TB, PB, EB, or IEC units such as MiB."
                    .into(),
            );
        }
    };

    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if fraction.contains('.') {
        return Err("a size can contain only one decimal point.".into());
    }
    let too_large = || "size is too large; the maximum is 18446744073709551615 bytes.".to_string();
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u128>().map_err(|_| too_large())?
    };
    let mut bytes = whole.checked_mul(multiplier).ok_or_else(too_large)?;
    // Removing trailing zeros accepts harmless extra precision without growing
    // the denominator. Reduce before multiplying to avoid intermediate overflow.
    let fraction = fraction.trim_end_matches('0');
    if !fraction.is_empty() {
        let numerator = fraction
            .parse::<u128>()
            .map_err(|_| "size has too many decimal places.".to_string())?;
        let exponent = u32::try_from(fraction.len())
            .map_err(|_| "size has too many decimal places.".to_string())?;
        let denominator = 10_u128
            .checked_pow(exponent)
            .ok_or_else(|| "size has too many decimal places.".to_string())?;
        let divisor = gcd(multiplier, denominator);
        let reduced_denominator = denominator / divisor;
        if numerator % reduced_denominator != 0 {
            return Err("size must resolve to a whole number of bytes.".into());
        }
        let fractional_bytes = (numerator / reduced_denominator) * (multiplier / divisor);
        bytes = bytes.checked_add(fractional_bytes).ok_or_else(too_large)?;
    }
    u64::try_from(bytes).map_err(|_| too_large())
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryKind, ScanStats};
    use std::{path::PathBuf, time::Duration};

    fn node(name: &str, size: u64, kind: EntryKind) -> Node {
        Node {
            name: name.into(),
            path: PathBuf::from(name),
            kind,
            size,
            children: Vec::new(),
            parent: None,
            modified: None,
        }
    }

    fn tree() -> ScanTree {
        let mut nodes = vec![
            node("root", 75, EntryKind::Directory),
            node("nested", 40, EntryKind::Directory),
            node("small.txt", 10, EntryKind::File),
            node("skip.bin", 30, EntryKind::File),
            node("KEEP.TXT", 30, EntryKind::File),
            node("shortcut", 5, EntryKind::Symlink),
        ];
        nodes[0].children = vec![1, 4, 5];
        nodes[1].children = vec![2, 3];
        for index in [1, 4, 5] {
            nodes[index].parent = Some(0);
        }
        for index in [2, 3] {
            nodes[index].parent = Some(1);
        }
        ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: 3,
                directories: 2,
                skipped: 0,
                symlinks: 1,
                elapsed: Duration::ZERO,
            },
        }
    }

    #[test]
    fn empty_filter_preserves_recorded_sizes() {
        let mut tree = tree();
        tree.nodes[0].size = 999;
        let filter = Filter::parse(" \t ").unwrap();
        assert!(filter.is_empty());
        assert_eq!(filter.weights(&tree), vec![999, 40, 10, 30, 30, 5]);
        assert!(filter.matches(&tree.nodes[2]));
        assert!(!filter.matches(&tree.nodes[0]));
    }

    #[test]
    fn substrings_and_exclusions_are_case_insensitive_and_combined() {
        let filter = Filter::parse("REPORT; .pdf; |draft").unwrap();
        assert!(filter.matches(&node("Annual Report.PDF", 1, EntryKind::File)));
        assert!(!filter.matches(&node("report draft.pdf", 1, EntryKind::File)));
        assert!(!filter.matches(&node("report.txt", 1, EntryKind::File)));
    }

    #[test]
    fn globs_match_whole_names_and_unicode_characters() {
        let filter = Filter::parse("?.TXT").unwrap();
        assert!(filter.matches(&node("🦀.txt", 1, EntryKind::File)));
        assert!(filter.matches(&node("Я.txt", 1, EntryKind::File)));
        assert!(!filter.matches(&node("ab.txt", 1, EntryKind::File)));
        let filter = Filter::parse("ФОТО*?.JPG").unwrap();
        assert!(filter.matches(&node("фото отпуск.jpg", 1, EntryKind::File)));
        assert!(!filter.matches(&node("myфото1.jpg", 1, EntryKind::File)));
        assert!(
            Filter::parse("a*b*c")
                .unwrap()
                .matches(&node("aaabbbc", 1, EntryKind::File))
        );
    }

    #[test]
    fn sizes_are_strict_and_distinguish_decimal_and_iec_units() {
        assert_eq!(parse_size("1.5 MB").unwrap(), 1_500_000);
        assert_eq!(parse_size("1.5MiB").unwrap(), 1_572_864);
        assert_eq!(parse_size(".5 KiB").unwrap(), 512);
        assert_eq!(parse_size("18446744073709551615B").unwrap(), u64::MAX);
        let filter = Filter::parse(">1kb;<2KiB").unwrap();
        assert!(!filter.matches(&node("file", 1_000, EntryKind::File)));
        assert!(filter.matches(&node("file", 1_001, EntryKind::File)));
        assert!(!filter.matches(&node("file", 2_048, EntryKind::File)));
    }

    #[test]
    fn invalid_queries_return_actionable_errors() {
        for query in [
            ";foo",
            "foo;",
            "foo;;bar",
            "|",
            ">",
            "<wat",
            ">=10mb",
            ">-1b",
            ">1.2.3MB",
            ">1XB",
            ">0.1B",
            ">18446744073709551616B",
        ] {
            let error = Filter::parse(query).unwrap_err();
            assert!(error.starts_with("Filter clause "), "{query}: {error}");
        }
    }

    #[test]
    fn matching_file_sizes_aggregate_through_every_ancestor() {
        let tree = tree();
        assert_eq!(
            Filter::parse("*.txt").unwrap().weights(&tree),
            vec![40, 10, 10, 0, 30, 0]
        );
        assert_eq!(
            Filter::parse("*.txt;>15b").unwrap().weights(&tree),
            vec![30, 0, 0, 0, 30, 0]
        );
        assert_eq!(Filter::parse("nothing").unwrap().weights(&tree), vec![0; 6]);
    }
}
