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
    roots
}

pub(super) fn reveal_command(path: &Path, is_directory: bool) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(if is_directory {
        path
    } else {
        path.parent().unwrap_or(path)
    });
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_opener_receives_the_containing_directory_without_a_shell() {
        let path = Path::new("/sample folder/a;$(literal).txt");
        let command = reveal_command(path, false);
        assert_eq!(command.get_program(), "xdg-open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [path.parent().unwrap().as_os_str()]
        );
        let command = reveal_command(path.parent().unwrap(), true);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [path.parent().unwrap().as_os_str()]
        );
    }
}
