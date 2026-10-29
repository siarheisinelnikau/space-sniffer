use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/")];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if !roots.contains(&home) {
            roots.push(home);
        }
    }
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        let mut volumes = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        volumes.sort();
        roots.extend(volumes);
    }
    roots
}

pub(super) fn reveal_command(path: &Path, _is_directory: bool) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.arg("-R").arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_selection_keeps_a_filename_as_a_literal_argument() {
        let path = Path::new("/Volumes/Test Disk/a;$(literal).txt");
        for is_directory in [false, true] {
            let command = reveal_command(path, is_directory);
            assert_eq!(command.get_program(), "/usr/bin/open");
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [std::ffi::OsStr::new("-R"), path.as_os_str()]
            );
        }
    }
}
