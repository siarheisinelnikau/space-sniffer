//! GitHub release discovery and verified, staged Windows updates.
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/siarheisinelnikau/space-sniffer/releases/latest";
pub const CAN_INSTALL: bool = cfg!(all(target_os = "windows", target_arch = "x86_64"));
const API_URL: &str =
    "https://api.github.com/repos/siarheisinelnikau/space-sniffer/releases/latest";
const REPO_URL: &str = "https://github.com/siarheisinelnikau/space-sniffer";
const LEGACY_ASSET_NAME: &str = "space-sniffer.exe";
const STAGED_ASSET_NAME: &str = "update.exe";
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub page_url: String,
    pub notes: String,
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug)]
pub struct Asset {
    url: String,
    pub size: u64,
    sha256: Option<[u8; 32]>,
}

impl Asset {
    pub fn is_verifiable(&self) -> bool {
        self.sha256.is_some()
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    state: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .user_agent(format!("space-sniffer/{CURRENT_VERSION}"))
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// None means the latest stable release is not newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    check_with(&agent(Duration::from_secs(20)), API_URL, CURRENT_VERSION)
}

fn check_with(agent: &ureq::Agent, url: &str, current: &str) -> Result<Option<Release>, String> {
    let mut response = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(404) => "No published release is available yet.".into(),
            ureq::Error::StatusCode(403 | 429) => {
                "GitHub request limit reached. Please try again later.".into()
            }
            _ => format!("Could not check GitHub: {error}"),
        })?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .map_err(|error| format!("Could not read GitHub release: {error}"))?;
    parse_release(&bytes, current)
}

fn parse_release(bytes: &[u8], current: &str) -> Result<Option<Release>, String> {
    let release: GitHubRelease = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid GitHub release: {error}"))?;
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|error| format!("Invalid release version: {error}"))?;
    let current = Version::parse(current).map_err(|error| error.to_string())?;
    if release.draft
        || release.prerelease
        || !version.pre.is_empty()
        || !version.cmp_precedence(&current).is_gt()
    {
        return Ok(None);
    }
    // Build URLs from our repository and a parsed semver tag, never from arbitrary metadata.
    let page_url = format!("{REPO_URL}/releases/tag/{}", release.tag_name);
    let asset_name = format!("SpaceSniffer-v{version}.exe");
    // Prefer the versioned asset. Retain discovery of releases published before this rule.
    let asset = [asset_name.as_str(), LEGACY_ASSET_NAME]
        .into_iter()
        .find_map(|name| {
            let download_url = format!("{REPO_URL}/releases/download/{}/{name}", release.tag_name);
            release.assets.iter().find_map(|asset| {
                (asset.name == name
                    && asset.state == "uploaded"
                    && asset.browser_download_url == download_url
                    && (1..=MAX_DOWNLOAD).contains(&asset.size))
                .then(|| Asset {
                    url: download_url.clone(),
                    size: asset.size,
                    sha256: asset.digest.as_deref().and_then(parse_digest),
                })
            })
        });
    Ok(Some(Release {
        version,
        page_url,
        notes: release.body.unwrap_or_default(),
        asset,
    }))
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

/// Owns the temporary directory; an abandoned or failed download is removed on drop.
pub struct Download {
    directory: tempfile::TempDir,
}

impl Download {
    fn path(&self) -> std::path::PathBuf {
        self.directory.path().join(STAGED_ASSET_NAME)
    }

    pub fn install(&self) -> Result<std::path::PathBuf, String> {
        if !CAN_INSTALL {
            return Err("In-app installation is available for Windows x86-64 only.".into());
        }
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        install(&self.path())?;
        Ok(executable)
    }
}

pub fn download(
    asset: &Asset,
    cancelled: &AtomicBool,
    progress: impl FnMut(u64),
) -> Result<Download, String> {
    let expected = asset.sha256.ok_or(
        "GitHub did not provide a SHA-256 checksum. Use the release page to update manually.",
    )?;
    let directory = tempfile::Builder::new()
        .prefix("space-sniffer-update-")
        .tempdir()
        .map_err(|error| format!("Could not create update folder: {error}"))?;
    let download = Download { directory };
    let mut response = agent(Duration::from_secs(300))
        .get(&asset.url)
        .header("Accept", "application/octet-stream")
        .call()
        .map_err(|error| format!("Could not download update: {error}"))?;
    let mut file = File::create(download.path()).map_err(|error| error.to_string())?;
    copy_verified(
        response.body_mut().as_reader(),
        &mut file,
        asset.size,
        expected,
        cancelled,
        progress,
    )?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(download)
}

fn copy_verified(
    mut reader: impl Read,
    writer: &mut impl Write,
    size: u64,
    expected: [u8; 32],
    cancelled: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let mut digest = Sha256::new();
    let mut received = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Download cancelled.".into());
        }
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("Download interrupted: {error}"))?;
        if count == 0 {
            break;
        }
        received += count as u64;
        if received > size || received > MAX_DOWNLOAD {
            return Err("Update size does not match the GitHub release.".into());
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        digest.update(&buffer[..count]);
        progress(received);
    }
    if received != size {
        return Err("The update download is incomplete. Please try again.".into());
    }
    if <[u8; 32]>::from(digest.finalize()) != expected {
        return Err("Update checksum mismatch. The update was not installed.".into());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn install(path: &Path) -> Result<(), String> {
    // Keep a recovery copy: self-replace can fail after moving the running image aside.
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = executable
        .parent()
        .ok_or("The application folder could not be found.")?;
    let backup = tempfile::Builder::new()
        .prefix(".space-sniffer-recovery-")
        .tempfile_in(parent)
        .map_err(|error| format!("Cannot write to the application folder. Move the app to a writable folder and try again: {error}"))?;
    std::fs::copy(&executable, backup.path()).map_err(|error| error.to_string())?;
    backup
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    if let Err(error) = self_replace::self_replace(path) {
        if !executable.exists()
            && let Err(restore_error) = std::fs::copy(backup.path(), &executable)
        {
            let (_, recovery) = backup.keep().map_err(|error| error.to_string())?;
            return Err(format!(
                "Update failed: {error}. Restore failed: {restore_error}. The previous executable is saved at {}",
                recovery.display()
            ));
        }
        return Err(format!("Could not install update: {error}"));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn install(_path: &Path) -> Result<(), String> {
    Err("In-app installation is available for Windows x86-64 only.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn release(tag: &str) -> Value {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        let name = format!("SpaceSniffer-v{version}.exe");
        json!({
            "tag_name": tag, "draft": false, "prerelease": false, "body": "Release notes",
            "assets": [{"name": name, "state": "uploaded", "size": 4,
                "browser_download_url": format!("{REPO_URL}/releases/download/{tag}/{name}"),
                "digest": format!("sha256:{:x}", Sha256::digest(b"test"))}]
        })
    }

    fn parse(value: &Value, current: &str) -> Option<Release> {
        parse_release(&serde_json::to_vec(value).unwrap(), current).unwrap()
    }

    #[test]
    fn compares_semantic_versions_and_ignores_unstable_or_older_releases() {
        assert!(parse(&release("v0.10.0"), "0.9.0").is_some());
        assert!(parse(&release("v0.9.0"), "0.10.0").is_none());
        assert!(parse(&release("v0.10.0"), "0.10.0").is_none());
        assert!(parse(&release("v0.10.0+rebuild"), "0.10.0").is_none());
        assert!(parse(&release("v0.11.0-rc.1"), "0.10.0").is_none());
        for flag in ["draft", "prerelease"] {
            let mut value = release("v1.0.0");
            value[flag] = json!(true);
            assert!(parse(&value, "0.10.0").is_none());
        }
        assert!(parse_release(b"not json", "0.1.0").is_err());
        assert!(
            parse_release(&serde_json::to_vec(&release("../../bad")).unwrap(), "0.1.0").is_err()
        );
    }

    #[test]
    fn only_accepts_our_uploaded_executable_and_an_exact_sha256_digest() {
        let original = release("v1.0.0");
        assert!(
            parse(&original, "0.1.0")
                .unwrap()
                .asset
                .unwrap()
                .is_verifiable()
        );
        for (field, invalid) in [
            (
                "browser_download_url",
                json!("https://example.com/space-sniffer.exe"),
            ),
            ("name", json!("other.exe")),
            ("state", json!("new")),
            ("size", json!(0)),
            ("size", json!(MAX_DOWNLOAD + 1)),
        ] {
            let mut value = original.clone();
            value["assets"][0][field] = invalid;
            assert!(parse(&value, "0.1.0").unwrap().asset.is_none());
        }
        for invalid in [
            Value::Null,
            json!("sha256:1234"),
            json!("md5:abcd"),
            json!("sha256:🦀"),
        ] {
            let mut value = original.clone();
            value["assets"][0]["digest"] = invalid;
            assert!(
                !parse(&value, "0.1.0")
                    .unwrap()
                    .asset
                    .unwrap()
                    .is_verifiable()
            );
        }
    }

    #[test]
    fn prefers_the_versioned_asset_and_accepts_legacy_release_names() {
        let mut value = release("v1.0.0");
        let mut legacy = value["assets"][0].clone();
        legacy["name"] = json!(LEGACY_ASSET_NAME);
        legacy["browser_download_url"] = json!(format!(
            "{REPO_URL}/releases/download/v1.0.0/{LEGACY_ASSET_NAME}"
        ));
        value["assets"].as_array_mut().unwrap().insert(0, legacy);
        let preferred = parse(&value, "0.1.0").unwrap().asset.unwrap();
        assert!(preferred.url.ends_with("/SpaceSniffer-v1.0.0.exe"));
        value["assets"].as_array_mut().unwrap().pop();
        let fallback = parse(&value, "0.1.0").unwrap().asset.unwrap();
        assert!(fallback.url.ends_with("/space-sniffer.exe"));
    }

    #[test]
    fn verifies_download_size_digest_and_cancellation_before_installation() {
        let expected = Sha256::digest(b"test").into();
        let cancelled = AtomicBool::new(false);
        let mut written = Vec::new();
        let mut progress = Vec::new();
        copy_verified(&b"test"[..], &mut written, 4, expected, &cancelled, |n| {
            progress.push(n)
        })
        .unwrap();
        assert_eq!(written, b"test");
        assert_eq!(progress, [4]);
        for (body, size, digest) in [
            (&b"tes"[..], 4, expected),
            (&b"tests"[..], 4, expected),
            (&b"test"[..], 4, [0; 32]),
        ] {
            assert!(
                copy_verified(body, &mut Vec::new(), size, digest, &cancelled, |_| {}).is_err()
            );
        }
        cancelled.store(true, Ordering::Relaxed);
        let mut written = Vec::new();
        assert!(
            copy_verified(&b"test"[..], &mut written, 4, expected, &cancelled, |_| {}).is_err()
        );
        assert!(written.is_empty());
    }

    #[test]
    fn sends_github_headers_and_handles_http_errors() {
        use std::net::TcpListener;
        for status in [200, 403, 404, 429, 500] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/latest", listener.local_addr().unwrap());
            let body = release("v1.0.0").to_string();
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("Mock server did not receive a request: {error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.contains("accept: application/vnd.github+json\r\n"));
                assert!(request.contains("x-github-api-version: 2022-11-28\r\n"));
                assert!(request.contains("user-agent: space-sniffer/"));
                write!(socket, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let agent = ureq::Agent::config_builder()
                .user_agent(format!("space-sniffer/{CURRENT_VERSION}"))
                .proxy(None)
                .timeout_global(Some(Duration::from_secs(5)))
                .build()
                .into();
            let result = check_with(&agent, &url, "0.1.0");
            server.join().unwrap();
            match status {
                200 => assert_eq!(result.unwrap().unwrap().version, Version::new(1, 0, 0)),
                403 | 429 => assert!(result.unwrap_err().contains("request limit")),
                404 => assert!(result.unwrap_err().contains("No published release")),
                _ => assert!(result.unwrap_err().contains("Could not check GitHub")),
            }
        }
    }

    #[test]
    #[ignore = "requires GitHub access and downloads the current release"]
    fn github_release_download_smoke() {
        let release = check_with(&agent(Duration::from_secs(20)), API_URL, "0.0.0")
            .unwrap()
            .unwrap();
        let asset = release
            .asset
            .expect("The release must include the Windows executable");
        assert!(asset.is_verifiable(), "GitHub must supply a SHA-256 digest");
        let download = download(&asset, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(
            std::fs::metadata(download.path()).unwrap().len(),
            asset.size
        );
        // Do not install or execute downloaded release binaries in this smoke test.
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_replaces_a_disposable_running_executable_and_recovers_on_failure() {
        use std::{os::windows::process::CommandExt, process::Command};
        let fixture = tempfile::Builder::new()
            .prefix("update test ' & ")
            .tempdir()
            .unwrap();
        let original = std::env::current_exe().unwrap();
        for mode in ["success", "failure"] {
            let folder = fixture.path().join(mode);
            std::fs::create_dir(&folder).unwrap();
            let executable = folder.join("app.exe");
            std::fs::copy(&original, &executable).unwrap();
            let output = Command::new(&executable)
                .args([
                    "--exact",
                    "updater::tests::windows_update_child",
                    "--nocapture",
                ])
                .env("SPACE_SNIFFER_UPDATE_TEST_MODE", mode)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(folder.join("verified").exists());
            let old_len = std::fs::metadata(&original).unwrap().len();
            let new_len = std::fs::metadata(&executable).unwrap().len();
            assert_eq!(new_len, old_len + if mode == "success" { 11 } else { 0 });
            // The replacement (or recovered executable) must still launch successfully.
            assert!(
                Command::new(&executable)
                    .args(["--exact", "updater::tests::windows_update_child"])
                    .env_remove("SPACE_SNIFFER_UPDATE_TEST_MODE")
                    .creation_flags(0x08000000)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_update_child() {
        let Some(mode) = std::env::var_os("SPACE_SNIFFER_UPDATE_TEST_MODE") else {
            return;
        };
        let executable = std::env::current_exe().unwrap();
        let folder = executable.parent().unwrap();
        let replacement = folder.join("replacement.exe");
        if mode == "success" {
            std::fs::copy(&executable, &replacement).unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&replacement)
                .unwrap()
                .write_all(b"new version")
                .unwrap();
            install(&replacement).unwrap();
        } else {
            assert!(install(&replacement).is_err());
        }
        assert!(executable.is_file());
        std::fs::write(folder.join("verified"), b"ok").unwrap();
    }
}
