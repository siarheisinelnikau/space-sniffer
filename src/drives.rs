use std::path::PathBuf;

/// Enumerate roots without querying volume capacities or waking slow drives.
pub fn roots() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
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
    #[cfg(not(target_os = "windows"))]
    {
        let mut roots = vec![PathBuf::from("/")];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home));
        }
        #[cfg(target_os = "macos")]
        if let Ok(volumes) = std::fs::read_dir("/Volumes") {
            roots.extend(
                volumes
                    .flatten()
                    .filter(|entry| entry.path().is_dir())
                    .map(|entry| entry.path()),
            );
        }
        roots
    }
}
