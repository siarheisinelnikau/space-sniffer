use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn roots() -> Vec<PathBuf> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    // SAFETY: GetLogicalDrives takes no pointers and returns a drive mask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", char::from(b'A' + bit as u8))))
        .collect()
}

pub(super) fn reveal_command(path: &Path, is_directory: bool) -> Command {
    let mut command = Command::new("explorer.exe");
    if is_directory {
        command.arg(path);
    } else {
        let mut selection = std::ffi::OsString::from("/select,");
        selection.push(path);
        command.arg(selection);
    }
    command
}

pub(super) fn attach_parent_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // SAFETY: ATTACH_PARENT_PROCESS is a documented sentinel; there are no
    // pointer arguments. Failure needs no recovery for a GUI executable.
    unsafe {
        AttachConsole(u32::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_keeps_paths_with_spaces_and_commas_in_one_literal_argument() {
        let path = Path::new(r"C:\sample folder\a,b.txt");
        let command = reveal_command(path, false);
        assert_eq!(command.get_program(), "explorer.exe");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new(r"/select,C:\sample folder\a,b.txt")]
        );
        let command = reveal_command(path.parent().unwrap(), true);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [path.parent().unwrap().as_os_str()]
        );
    }
}
