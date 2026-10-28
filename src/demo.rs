use space_sniffer::model::{EntryKind, Node, ScanStats, ScanTree};
use std::{path::PathBuf, time::Duration};

/// Synthetic data; the welcome screen never scans a user's disk automatically.
pub fn tree() -> ScanTree {
    let mut tree = ScanTree {
        nodes: Vec::new(),
        root: 0,
        stats: ScanStats::default(),
    };
    let root = add(&mut tree, None, "Demo workspace", 0, true);
    let projects = add(&mut tree, Some(root), "Projects", 0, true);
    let atlas = add(&mut tree, Some(projects), "atlas-studio", 0, true);
    files(
        &mut tree,
        atlas,
        &[
            ("target", 10_600, true),
            ("node_modules", 3_200, true),
            ("assets", 1_900, true),
            ("src", 230, true),
            ("design.fig", 410, false),
        ],
    );
    let archive = add(&mut tree, Some(projects), "Archived projects", 0, true);
    files(
        &mut tree,
        archive,
        &[
            ("website-backup.zip", 4_100, false),
            ("prototypes.zip", 2_300, false),
            ("old-builds.tar", 1_400, false),
        ],
    );
    let media = add(&mut tree, Some(root), "Media", 0, true);
    let video = add(&mut tree, Some(media), "Video", 0, true);
    files(
        &mut tree,
        video,
        &[
            ("iceland-4k.mp4", 8_200, false),
            ("studio-session.mov", 4_700, false),
            ("showreel.mp4", 2_600, false),
        ],
    );
    let photos = add(&mut tree, Some(media), "Photography", 0, true);
    files(
        &mut tree,
        photos,
        &[
            ("summer.raw", 2_800, false),
            ("portraits.raw", 1_700, false),
            ("exports.jpg", 900, false),
        ],
    );
    let downloads = add(&mut tree, Some(root), "Downloads", 0, true);
    files(
        &mut tree,
        downloads,
        &[
            ("linux-desktop.iso", 4_900, false),
            ("textures.zip", 2_200, false),
            ("setup.exe", 810, false),
            ("reference.pdf", 190, false),
        ],
    );
    let apps = add(&mut tree, Some(root), "Applications", 0, true);
    files(
        &mut tree,
        apps,
        &[
            ("Blender", 2_100, true),
            ("Rust toolchain", 1_700, true),
            ("Utilities", 740, true),
        ],
    );
    let docs = add(&mut tree, Some(root), "Documents", 0, true);
    files(
        &mut tree,
        docs,
        &[
            ("research.pdf", 380, false),
            ("presentation.pptx", 240, false),
            ("notes.md", 24, false),
        ],
    );
    for id in (0..tree.nodes.len()).rev() {
        if let Some(parent) = tree.nodes[id].parent {
            tree.nodes[parent].size += tree.nodes[id].size;
        }
    }
    let sizes: Vec<_> = tree.nodes.iter().map(|n| n.size).collect();
    for node in &mut tree.nodes {
        node.children
            .sort_by_key(|&id| std::cmp::Reverse(sizes[id]));
    }
    tree.stats.files = tree.nodes.iter().filter(|n| !n.is_dir()).count() as u64;
    tree.stats.directories = tree.nodes.iter().filter(|n| n.is_dir()).count() as u64;
    tree.stats.elapsed = Duration::from_millis(420);
    tree
}

fn add(tree: &mut ScanTree, parent: Option<usize>, name: &str, size: u64, dir: bool) -> usize {
    let id = tree.nodes.len();
    let path = parent.map_or_else(|| PathBuf::from(name), |p| tree.nodes[p].path.join(name));
    tree.nodes.push(Node {
        name: name.into(),
        path,
        kind: if dir {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size,
        children: Vec::new(),
        parent,
        modified: None,
    });
    if let Some(parent) = parent {
        tree.nodes[parent].children.push(id);
    }
    id
}

fn files(tree: &mut ScanTree, parent: usize, entries: &[(&str, u64, bool)]) {
    for &(name, mib, directory) in entries {
        if directory {
            let dir = add(tree, Some(parent), name, 0, true);
            for (index, ratio) in [47, 27, 16, 10].into_iter().enumerate() {
                add(
                    tree,
                    Some(dir),
                    &format!("bundle-{}.bin", index + 1),
                    mib * 1_048_576 * ratio / 100,
                    false,
                );
            }
        } else {
            add(tree, Some(parent), name, mib * 1_048_576, false);
        }
    }
}
