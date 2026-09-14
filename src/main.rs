mod app;
mod demo;

use std::{path::PathBuf, sync::atomic::AtomicBool};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!(
            "Space Sniffer\n\n  space-sniffer [PATH]        Open the app, optionally scanning PATH\n  space-sniffer --scan PATH   Print a disk usage summary\n  space-sniffer --help        Show this help\n\nWithout PATH the app displays synthetic demo data. Files are never deleted."
        );
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--scan") {
        if args.len() != 2 {
            return Err("Usage: space-sniffer --scan PATH".into());
        }
        let tree =
            space_sniffer::scanner::scan_path(&PathBuf::from(&args[1]), &AtomicBool::new(false))?;
        println!(
            "{}\n{} · {} files · {} folders · {} skipped · {} links",
            tree.nodes[tree.root].path.display(),
            space_sniffer::format_bytes(tree.nodes[tree.root].size),
            tree.stats.files,
            tree.stats.directories,
            tree.stats.skipped,
            tree.stats.symlinks
        );
        for &id in tree.nodes[tree.root].children.iter().take(20) {
            println!(
                "{:>12}  {}",
                space_sniffer::format_bytes(tree.nodes[id].size),
                tree.nodes[id].name
            );
        }
        return Ok(());
    }
    if args.len() > 1
        || args
            .first()
            .is_some_and(|arg| arg.to_string_lossy().starts_with('-'))
    {
        return Err("Usage: space-sniffer [PATH] or space-sniffer --scan PATH".into());
    }
    let path = args.first().map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Space Sniffer")
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([960.0, 640.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Space Sniffer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path)))),
    )?;
    Ok(())
}

fn app_icon() -> eframe::egui::IconData {
    let mut rgba = vec![0; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            let color = if (7..35).contains(&x) && (7..57).contains(&y) {
                [100, 207, 187, 255]
            } else if (39..57).contains(&x) && (7..34).contains(&y) {
                [64, 111, 161, 255]
            } else if (39..57).contains(&x) && (38..57).contains(&y) {
                [167, 120, 62, 255]
            } else {
                [17, 22, 32, 255]
            };
            rgba[(y * 64 + x) * 4..(y * 64 + x + 1) * 4].copy_from_slice(&color);
        }
    }
    eframe::egui::IconData {
        rgba,
        width: 64,
        height: 64,
    }
}
