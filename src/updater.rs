//! Product configuration for the shared GitHub updater.
use rust_auto_updator::{Config, Prepared};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/siarheisinelnikau/space-sniffer/releases/latest";
pub const CAN_INSTALL: bool = cfg!(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
));
pub fn config() -> Result<Config, String> {
    #[cfg(not(target_os = "linux"))]
    let mut config = Config::new(
        "siarheisinelnikau",
        "space-sniffer",
        CURRENT_VERSION,
        "SpaceSniffer",
    )?;
    #[cfg(target_os = "linux")]
    let mut config = Config::debian(
        "siarheisinelnikau",
        "space-sniffer",
        CURRENT_VERSION,
        "space-sniffer",
    )?;
    config.asset_platform = Some(
        if cfg!(target_os = "linux") {
            "ubuntu"
        } else {
            "windows"
        }
        .into(),
    );
    config.allow_asset_digest = true;
    #[cfg(not(target_os = "linux"))]
    {
        config.legacy_executable_names =
            vec!["SpaceSniffer.exe".into(), "space-sniffer.exe".into()];
        config.expected_subsystem = Some(2);
        config.install.helper_name = "SpaceSniffer-updater.exe".into();
    }
    #[cfg(target_os = "linux")]
    {
        config.product_name = "SpaceSniffer".into();
        config.legacy_executable_names =
            vec!["SpaceSniffer.deb".into(), "space-sniffer.deb".into()];
        config.install.helper_name = "space-sniffer-updater".into();
    }
    config.max_executable_size = 256 * 1024 * 1024;
    config.install.stage_prefix = ".space-sniffer-update-".into();
    Ok(config)
}
#[derive(Clone, Debug)]
pub struct Release {
    pub version: semver::Version,
    pub page_url: String,
    pub notes: String,
    pub asset: Option<Asset>,
}
#[derive(Clone, Debug)]
pub struct Asset {
    pub size: u64,
    shared: Box<rust_auto_updator::Release>,
}
impl Asset {
    pub fn is_verifiable(&self) -> bool {
        true
    }
}
fn adapt(shared: rust_auto_updator::Release) -> Release {
    Release {
        version: shared.version.clone(),
        page_url: shared.page.clone(),
        notes: shared.notes.clone(),
        asset: Some(Asset {
            size: shared.size(),
            shared: Box::new(shared),
        }),
    }
}
pub fn check() -> Result<Option<Release>, String> {
    rust_auto_updator::check_latest(&config()?).map(|release| release.map(adapt))
}
pub struct Download {
    prepared: Prepared,
}
impl Download {
    pub fn handoff(&mut self, root: Option<&Path>) -> Result<(), String> {
        let args = match root {
            Some(path) => vec![
                path.to_str()
                    .ok_or("The current root cannot be represented as an update restart argument.")?
                    .to_owned(),
            ],
            None => Vec::new(),
        };
        self.prepared.set_restart_args(args)?;
        rust_auto_updator::handoff(&mut self.prepared)
    }
}
pub fn download(
    asset: &Asset,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<Download, String> {
    let prepared = rust_auto_updator::prepare_download_cancellable(
        &asset.shared,
        &config()?,
        cancelled,
        |bytes, _| progress(bytes),
    )?;
    Ok(Download { prepared })
}
/// Route the helper before CLI parsing or GUI side effects. Return the remaining
/// application arguments; cleanup is deferred until the new host initializes.
pub type RoutedArguments = (Vec<std::ffi::OsString>, Option<PathBuf>);

pub fn route_arguments(
    mut args: Vec<std::ffi::OsString>,
) -> Result<Option<RoutedArguments>, String> {
    if args.first().is_some_and(|arg| arg == "--apply-update") {
        rust_auto_updator::apply_update(&config()?)?;
        return Ok(None);
    }
    let cleanup = if let Some(index) = args.iter().position(|arg| arg == "--cleanup-update") {
        if index + 2 != args.len() {
            return Err("Invalid update cleanup arguments".into());
        }
        let directory = PathBuf::from(&args[index + 1]);
        args.truncate(index);
        Some(directory)
    } else {
        None
    };
    Ok(Some((args, cleanup)))
}
pub fn cleanup(directory: PathBuf) {
    if let Ok(config) = config() {
        rust_auto_updator::cleanup_after_update(directory, config.install);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_selection_stays_bound_to_the_product_and_accepts_legacy_names() {
        let config = config().unwrap();
        let version = "9.0.0".parse().unwrap();
        let names = config.executable_names(&version);
        let platform = if cfg!(target_os = "linux") {
            "ubuntu"
        } else {
            "windows"
        };
        let extension = if cfg!(target_os = "linux") {
            "deb"
        } else {
            "exe"
        };
        assert_eq!(
            names[0],
            format!(
                "SpaceSniffer-v9.0.0-{platform}-{}.{extension}",
                config.architecture.debian_name()
            )
        );
        for name in names {
            let release = serde_json::json!({"tag_name":"v9.0.0","html_url":"https://github.com/siarheisinelnikau/space-sniffer/releases/tag/v9.0.0","draft":false,"prerelease":false,
                "assets":[{"name":name,"size":1024,"browser_download_url":format!("https://github.com/siarheisinelnikau/space-sniffer/releases/download/v9.0.0/{name}"),"digest":format!("sha256:{}","ab".repeat(32))}]});
            let parsed = rust_auto_updator::parse_release(&release.to_string(), &config)
                .unwrap()
                .unwrap();
            assert_eq!(parsed.executable_name(), name);
            assert!(adapt(parsed).asset.unwrap().is_verifiable());
        }
    }
    #[test]
    fn cleanup_protocol_preserves_root_and_rejects_extra_arguments() {
        let arguments = vec![
            "C:/data".into(),
            "--cleanup-update".into(),
            "C:/stage".into(),
        ];
        let (args, cleanup) = route_arguments(arguments).unwrap().unwrap();
        assert_eq!(args, vec![std::ffi::OsString::from("C:/data")]);
        assert_eq!(cleanup, Some(PathBuf::from("C:/stage")));
        assert!(route_arguments(vec!["--cleanup-update".into()]).is_err());
    }
}
