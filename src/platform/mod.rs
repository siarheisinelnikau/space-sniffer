//! Native desktop integration. The scanner, model, filters, and geometry do
//! not depend on these helpers, so a platform port can change its adapter.

use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod native;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod native;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
#[path = "unix.rs"]
mod native;

pub const OPEN_SHORTCUT_LABEL: &str = if cfg!(target_os = "macos") {
    "Cmd+O"
} else {
    "Ctrl+O"
};

pub fn roots() -> Vec<PathBuf> {
    native::roots()
}

pub fn reveal(path: &Path, is_directory: bool) -> Result<(), String> {
    native::reveal_command(path, is_directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not reveal entry: {error}"))
}

pub fn attach_parent_console() {
    #[cfg(target_os = "windows")]
    native::attach_parent_console();
}
