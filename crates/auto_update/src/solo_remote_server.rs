//! SOLO: the remote server for SSH, WSL and dev container projects, from
//! Solo's own GitHub releases instead of Zed's servers (which have none for
//! Solo's versions).
//!
//! `.github/workflows/solo-package.yml` builds it for Linux (x86_64 and
//! aarch64, static musl binaries) and attaches it to each release as
//! `solo-remote-server-linux-<arch>.gz`. Solo downloads the one of its own
//! version, checks its size and SHA-256 against the ones GitHub reports (as
//! the updater does for the installer) and keeps it in
//! `remote_servers_dir()`; the transports upload it to the remote side. The
//! remote side never downloads it itself, since it couldn't check it.

use crate::solo_update::{
    LATEST_RELEASE_URL, download_asset, fetch_release_json, parse_release_assets, release_api_url,
};
use anyhow::{Context as _, Result};
use http_client::HttpClient;
use semver::Version;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// The name of the release file with the remote server for `os` and `arch`
/// (as `remote::RemotePlatform` spells them), if Solo builds one for them.
pub fn asset_name(os: &str, arch: &str) -> Result<String> {
    match (os, arch) {
        ("linux", "x86_64" | "aarch64") => Ok(format!("solo-remote-server-linux-{arch}.gz")),
        ("linux", _) => anyhow::bail!("Solo has no remote server for Linux on {arch}"),
        ("macos", _) => anyhow::bail!("Solo has no remote server for macOS yet"),
        _ => anyhow::bail!("Solo has no remote server for {os} yet"),
    }
}

/// Makes sure `platform_dir` holds the remote server of `version` (the latest
/// release when `None`) as `<version>.gz`, downloading and checking it if it
/// doesn't, and returns its path. With a version that's already there,
/// nothing is downloaded.
pub async fn download(
    http: Arc<dyn HttpClient>,
    current: &Version,
    version: Option<&Version>,
    os: &str,
    arch: &str,
    platform_dir: &Path,
) -> Result<PathBuf> {
    let name = asset_name(os, arch)?;
    let version = version.map(|version| {
        let mut version = version.clone();
        version.pre = semver::Prerelease::EMPTY;
        version.build = semver::BuildMetadata::EMPTY;
        version
    });
    if let Some(version) = &version {
        let path = version_path(platform_dir, version);
        if smol::fs::metadata(&path).await.is_ok() {
            return Ok(path);
        }
    }

    let url = match &version {
        Some(version) => release_api_url(version),
        None => LATEST_RELEASE_URL.to_string(),
    };
    let json = fetch_release_json(http.clone(), &url, current)
        .await
        .with_context(|| match &version {
            Some(version) => format!("couldn't find Solo {version}'s release on GitHub"),
            None => "couldn't find Solo's latest release on GitHub".to_string(),
        })?;
    let (release_version, assets) = parse_release_assets(&json)?;
    if let Some(version) = &version {
        anyhow::ensure!(
            &release_version == version,
            "GitHub returned the release of {release_version}, not {version}"
        );
    }
    let asset = assets
        .into_iter()
        .find(|asset| asset.name == name)
        .with_context(|| format!("Solo {release_version}'s release has no {name}"))?;

    let path = version_path(platform_dir, &release_version);
    if smol::fs::metadata(&path).await.is_ok() {
        return Ok(path);
    }
    smol::fs::create_dir_all(platform_dir)
        .await
        .with_context(|| format!("couldn't create {}", platform_dir.display()))?;
    // Downloaded next to it and renamed once checked, so a `.gz` in the
    // cache is always a whole, checked file.
    let partial = platform_dir.join(format!(
        "{release_version}.gz.download-{}",
        std::process::id()
    ));
    log::info!("downloading {name} of Solo {release_version}");
    let downloaded = download_asset(http, &asset, "remote server", current, &partial, |_| {}).await;
    if let Err(error) = downloaded {
        smol::fs::remove_file(&partial).await.ok();
        return Err(error);
    }
    smol::fs::rename(&partial, &path)
        .await
        .with_context(|| format!("couldn't move the remote server to {}", path.display()))?;
    Ok(path)
}

fn version_path(platform_dir: &Path, version: &Version) -> PathBuf {
    platform_dir.join(format!("{version}.gz"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SERVER: &[u8] = b"a gzipped remote server";

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn release_json(tag: &str, digest: &str) -> String {
        format!(
            r#"{{
                "tag_name": "{tag}",
                "html_url": "https://github.com/lucas9801/solo/releases/tag/{tag}",
                "assets": [
                    {{"name": "Solo-Setup-2.3.0.exe", "size": 5,
                      "browser_download_url": "https://example.com/setup"}},
                    {{"name": "solo-remote-server-linux-x86_64.gz", "size": {size},
                      "browser_download_url": "https://example.com/server-x86_64",
                      "digest": "sha256:{digest}"}}
                ]
            }}"#,
            size = SERVER.len()
        )
    }

    fn fake_github(digest: String, requests: Arc<AtomicUsize>) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |request| {
            let digest = digest.clone();
            let requests = requests.clone();
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                let uri = request.uri().to_string();
                let body = match uri.as_str() {
                    "https://api.github.com/repos/lucas9801/solo/releases/tags/v2.3.0"
                    | "https://api.github.com/repos/lucas9801/solo/releases/latest" => {
                        release_json("v2.3.0", &digest).into_bytes()
                    }
                    "https://example.com/server-x86_64" => SERVER.to_vec(),
                    _ => return Ok(Response::builder().status(404).body("".into()).unwrap()),
                };
                Ok(Response::builder().status(200).body(body.into()).unwrap())
            }
        })
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn names_the_release_files() {
        assert_eq!(
            asset_name("linux", "x86_64").unwrap(),
            "solo-remote-server-linux-x86_64.gz"
        );
        assert_eq!(
            asset_name("linux", "aarch64").unwrap(),
            "solo-remote-server-linux-aarch64.gz"
        );
        assert!(asset_name("macos", "aarch64").is_err());
        assert!(asset_name("windows", "x86_64").is_err());
    }

    #[gpui::test]
    async fn downloads_and_checks_the_remote_server(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let platform_dir = dir.path().join("stable").join("linux-x86_64");
        let requests = Arc::new(AtomicUsize::new(0));
        let http = fake_github(sha256_hex(SERVER), requests.clone());

        let path = download(
            http.clone(),
            &version("2.3.0"),
            Some(&version("2.3.0+stable.12.abc")),
            "linux",
            "x86_64",
            &platform_dir,
        )
        .await
        .unwrap();
        assert_eq!(path, platform_dir.join("2.3.0.gz"));
        assert_eq!(std::fs::read(&path).unwrap(), SERVER);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read_dir(&platform_dir).unwrap().count(), 1);

        // Already downloaded: no request at all.
        let again = download(
            http.clone(),
            &version("2.3.0"),
            Some(&version("2.3.0")),
            "linux",
            "x86_64",
            &platform_dir,
        )
        .await
        .unwrap();
        assert_eq!(again, path);
        assert_eq!(requests.load(Ordering::SeqCst), 2);

        // The latest release, for builds without a released version.
        std::fs::remove_file(&path).unwrap();
        let latest = download(
            http,
            &version("2.3.0"),
            None,
            "linux",
            "x86_64",
            &platform_dir,
        )
        .await
        .unwrap();
        assert_eq!(latest, path);
    }

    #[gpui::test]
    async fn rejects_a_remote_server_that_doesnt_match(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let platform_dir = dir.path().join("linux-x86_64");
        let http = fake_github(sha256_hex(b"another file"), Arc::default());

        let error = download(
            http,
            &version("2.3.0"),
            Some(&version("2.3.0")),
            "linux",
            "x86_64",
            &platform_dir,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("corrupt"), "{error:#}");
        assert_eq!(std::fs::read_dir(&platform_dir).unwrap().count(), 0);
    }

    /// SOLO: without a hash from GitHub, the remote server isn't downloaded
    /// (it would be run on the remote machine unchecked).
    #[gpui::test]
    async fn rejects_a_remote_server_without_a_hash(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let platform_dir = dir.path().join("linux-x86_64");
        let requests = Arc::new(AtomicUsize::new(0));
        // Not `sha256:<64 hex digits>`: as good as none.
        let http = fake_github("not-a-hash".to_string(), requests.clone());

        let error = download(
            http,
            &version("2.3.0"),
            Some(&version("2.3.0")),
            "linux",
            "x86_64",
            &platform_dir,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("no SHA-256"), "{error:#}");
        // Only the release was asked for.
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(
            std::fs::read_dir(&platform_dir)
                .map(|entries| entries.count() == 0)
                .unwrap_or(true)
        );
    }

    #[gpui::test]
    async fn says_what_a_release_lacks(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let http = fake_github(sha256_hex(SERVER), Arc::default());

        let error = download(
            http.clone(),
            &version("2.3.0"),
            Some(&version("2.3.0")),
            "linux",
            "aarch64",
            dir.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Solo 2.3.0's release has no solo-remote-server-linux-aarch64.gz"
        );

        let error = download(
            http.clone(),
            &version("2.4.0"),
            Some(&version("2.4.0")),
            "linux",
            "x86_64",
            dir.path(),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("couldn't find Solo 2.4.0's release"),
            "{error:#}"
        );

        let error = download(
            http,
            &version("2.3.0"),
            Some(&version("2.3.0")),
            "macos",
            "aarch64",
            dir.path(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("macOS"), "{error:#}");
    }
}
