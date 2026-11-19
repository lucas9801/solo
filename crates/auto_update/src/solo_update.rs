//! SOLO: Solo updates itself from its GitHub Releases instead of Zed's servers.
//!
//! Zed's own [`crate::AutoUpdater`] never polls (see
//! `ReleaseChannel::poll_for_updates`), so it never talks to zed.dev. This
//! module is a separate, much smaller updater:
//!
//! - it asks the GitHub API for the latest release of [`SOLO_GITHUB_REPO`],
//! - tells the user about a newer version with the update button at the right
//!   end of the title bar ([`crate::update_button`]), whose menu offers
//!   "update and restart", "install when Solo quits", "view release notes"
//!   and "skip this version" (no notifications: the button stays),
//! - and on Windows downloads the Inno Setup installer (checked against the
//!   SHA-256 GitHub reports) and starts `solo-update-helper`, which waits for
//!   Solo to exit and runs the installer silently. "Update and restart" then
//!   quits Solo through the normal quit flow (so unsaved files still prompt)
//!   and the installer starts it again; "install when Solo quits" waits for
//!   the user to quit, so agents running in terminals carry on meanwhile.
//!
//! It's for Solo installed with the installer from a GitHub release (and
//! builds from source). Each release has that installer,
//! `Solo-Setup-<version>.exe` (`INSTALLER_PREFIX`, made by
//! `.github/workflows/solo-package.yml` from `script/solo.iss`, unsigned),
//! next to the Linux remote servers (`solo-remote-server-linux-<arch>.gz`,
//! [`crate::solo_remote_server`]), which aren't installers. The installer has
//! the same AppId as every earlier one, so it updates Solo in its folder,
//! wherever the user installed it.
//!
//! Solo Code from the Microsoft Store doesn't use this updater at all
//! ([`init`] returns at once for a packaged Solo): the Store updates it
//! ([`crate::store_update`]), whatever the GitHub release holds.
//!
//! For a release without an installer (2.5.0 was published without one), the
//! button still says a newer version is out, but its menu only offers Solo
//! Code's page in the Microsoft Store ([`STORE_PAGE_URL`]) and skipping the
//! version: nothing is downloaded and nothing fails.
//!
//! Only stable builds check. Automatic checks follow the `auto_update`
//! setting; the "check for updates" command always checks.

use crate::update_button::{self, ButtonState, UpdateAction, UpdateEntry, UpdateOffer};
use crate::{AutoUpdateSetting, UpdateCheckType};
use anyhow::{Context as _, Result};
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Global, Task, Window};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request};
use release_channel::ReleaseChannel;
use semver::Version;
use serde::Deserialize;
use settings::{Settings as _, SettingsStore};
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use util::ResultExt as _;
use workspace::{
    MultiWorkspace,
    item::RunningProgram,
    notifications::{
        NotificationId, show_app_notification, simple_message_notification::MessageNotification,
    },
};

/// The GitHub repository Solo's releases are published in.
pub const SOLO_GITHUB_REPO: &str = "lucas9801/solo";

/// Whether this Solo was installed from the Microsoft Store (it runs as a
/// packaged app): the Store updates it ([`crate::store_update`]) and Solo's
/// own updater stays off.
pub fn is_store_package() -> bool {
    util::packaged::is_packaged()
}

/// Solo Code's page in the Microsoft Store, which builds that can't install
/// a release themselves offer.
pub const STORE_PAGE_URL: &str = "https://apps.microsoft.com/detail/9NBW452JVWN6";

pub(crate) const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/lucas9801/solo/releases/latest";
const INSTALLER_PREFIX: &str = "Solo-Setup-";
const INSTALLER_SUFFIX: &str = ".exe";

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const API_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const SKIPPED_VERSION_KEY: &str = "solo-update-skipped-version";
/// The version an update is installing (or will install when Solo quits), to
/// say how it went on the next start.
const PENDING_VERSION_KEY: &str = "solo-update-pending-version";

/// A release as published on GitHub.
#[derive(Clone, Debug, PartialEq)]
pub struct SoloRelease {
    pub version: Version,
    /// The release page, which shows the release notes.
    pub html_url: String,
    /// The release notes (markdown).
    pub notes: String,
    pub prerelease: bool,
    /// The Windows installer (`Solo-Setup-<version>.exe`), if the release has one.
    pub installer: Option<SoloInstaller>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoloInstaller {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// The SHA-256 GitHub computed when the installer was uploaded (lowercase
    /// hex), if it reported one.
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct GithubReleaseJson {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAssetJson>,
}

#[derive(Deserialize)]
struct GithubAssetJson {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    /// `sha256:<hex>`.
    #[serde(default)]
    digest: Option<String>,
}

/// The hex SHA-256 of a GitHub asset `digest` (`sha256:<hex>`).
fn sha256_of_digest(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?.to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// Parses a release tag such as `v1.28.1`, `1.28.1` or `v1.29` into a version.
pub fn parse_version(tag: &str) -> Option<Version> {
    let tag = tag.trim();
    let tag = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    if let Ok(version) = Version::parse(tag) {
        return Some(version);
    }
    // Tolerate `1.29` (no patch component).
    let parts: Vec<&str> = tag.split('.').collect();
    if parts.len() == 2 && parts.iter().all(|part| part.parse::<u64>().is_ok()) {
        return Version::parse(&format!("{tag}.0")).ok();
    }
    None
}

fn is_installer_asset(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(&INSTALLER_PREFIX.to_ascii_lowercase())
        && lower.ends_with(INSTALLER_SUFFIX)
        // SOLO: the name is joined to the download folder (R3 L-U1).
        && !name.contains(['/', '\\', ':'])
        && !name.contains("..")
}

impl From<GithubAssetJson> for SoloInstaller {
    fn from(asset: GithubAssetJson) -> Self {
        SoloInstaller {
            sha256: asset.digest.as_deref().and_then(sha256_of_digest),
            name: asset.name,
            url: asset.browser_download_url,
            size: asset.size,
        }
    }
}

/// How much of `total` bytes `written` is, in percent. The total comes from
/// GitHub, so it can be anything (R3 L-U8).
fn download_percent(written: u64, total: u64) -> u8 {
    if total == 0 {
        return 100;
    }
    (u128::from(written.min(total)) * 100 / u128::from(total)) as u8
}

fn parse_release_json(json: &[u8]) -> Result<(GithubReleaseJson, Version)> {
    let release: GithubReleaseJson =
        serde_json::from_slice(json).context("unexpected response from GitHub")?;
    let version = parse_version(&release.tag_name)
        .with_context(|| format!("release tag {:?} is not a version", release.tag_name))?;
    Ok((release, version))
}

/// The version of a release (GitHub's JSON for it) and all its files, which
/// [`SoloInstaller`] describes as well as the installer.
pub(crate) fn parse_release_assets(json: &[u8]) -> Result<(Version, Vec<SoloInstaller>)> {
    let (release, version) = parse_release_json(json)?;
    Ok((
        version,
        release
            .assets
            .into_iter()
            .map(SoloInstaller::from)
            .collect(),
    ))
}

/// Parses the JSON returned by GitHub's "latest release" endpoint.
pub fn parse_release(json: &[u8]) -> Result<SoloRelease> {
    let (release, version) = parse_release_json(json)?;
    let installer = release
        .assets
        .into_iter()
        .find(|asset| is_installer_asset(&asset.name))
        .map(SoloInstaller::from);
    Ok(SoloRelease {
        version,
        html_url: release.html_url,
        notes: release.body.unwrap_or_default(),
        prerelease: release.prerelease || release.draft,
        installer,
    })
}

fn without_build_metadata(version: &Version) -> Version {
    let mut version = version.clone();
    version.build = semver::BuildMetadata::EMPTY;
    version
}

/// Whether `candidate` is newer than the running `current` version.
pub fn is_newer(current: &Version, candidate: &Version) -> bool {
    without_build_metadata(candidate) > without_build_metadata(current)
}

#[derive(Clone, Debug, PartialEq)]
pub enum CheckOutcome {
    /// A newer release the user should hear about.
    UpdateAvailable(SoloRelease),
    /// Nothing newer than the running version (pre-releases are ignored).
    UpToDate,
    /// A newer release exists, but the user skipped it (automatic checks only).
    Skipped(Version),
}

/// Decides what a check should report. Skipping a version only silences
/// automatic checks; asking explicitly always shows the newest release.
pub fn evaluate_release(
    current: &Version,
    release: SoloRelease,
    skipped: Option<&Version>,
    check_type: UpdateCheckType,
) -> CheckOutcome {
    if release.prerelease || !is_newer(current, &release.version) {
        return CheckOutcome::UpToDate;
    }
    if !check_type.is_manual() && skipped == Some(&release.version) {
        return CheckOutcome::Skipped(release.version);
    }
    CheckOutcome::UpdateAvailable(release)
}

pub(crate) fn user_agent(current: &Version) -> String {
    format!("Solo/{current}")
}

/// Fetches the latest published release from GitHub.
pub async fn fetch_latest_release(
    http: Arc<dyn HttpClient>,
    current: &Version,
) -> Result<SoloRelease> {
    fetch_release(http, LATEST_RELEASE_URL, current).await
}

/// The tag Solo's release of `version` is published under (`v1.2.3`).
fn release_tag(version: &Version) -> String {
    let version = without_build_metadata(version);
    format!("v{version}")
}

/// The GitHub API URL of the release of `version`.
pub fn release_api_url(version: &Version) -> String {
    format!(
        "https://api.github.com/repos/{SOLO_GITHUB_REPO}/releases/tags/{}",
        release_tag(version)
    )
}

/// The GitHub page of the release of `version`, which shows its notes.
pub fn release_page_url(version: &Version) -> String {
    format!(
        "https://github.com/{SOLO_GITHUB_REPO}/releases/tag/{}",
        release_tag(version)
    )
}

/// Fetches the release of `version` (normally the running one) from GitHub,
/// for its release notes.
pub async fn fetch_release_for_version(
    http: Arc<dyn HttpClient>,
    version: &Version,
) -> Result<SoloRelease> {
    fetch_release(http, &release_api_url(version), version).await
}

async fn fetch_release(
    http: Arc<dyn HttpClient>,
    url: &str,
    current: &Version,
) -> Result<SoloRelease> {
    parse_release(&fetch_release_json(http, url, current).await?)
}

/// GitHub's JSON for the release at `url` (an API URL).
pub(crate) async fn fetch_release_json(
    http: Arc<dyn HttpClient>,
    url: &str,
    current: &Version,
) -> Result<Vec<u8>> {
    let request = Request::get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", user_agent(current))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .follow_redirects(RedirectPolicy::FollowAll)
        .timeout(API_TIMEOUT)
        .body(AsyncBody::default())?;
    let mut response = http.send(request).await.context("couldn't reach GitHub")?;
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("couldn't read GitHub's response")?;
    let status = response.status();
    if status.as_u16() == 404 {
        anyhow::bail!("no release has been published yet");
    }
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        let text: String = text.chars().take(200).collect();
        anyhow::bail!("GitHub returned {}: {text}", status.as_u16());
    }
    Ok(body)
}

/// One whole check: fetch the latest release and decide what to report.
pub async fn check_for_update(
    http: Arc<dyn HttpClient>,
    current: &Version,
    skipped: Option<&Version>,
    check_type: UpdateCheckType,
) -> Result<CheckOutcome> {
    let release = fetch_latest_release(http, current).await?;
    Ok(evaluate_release(current, release, skipped, check_type))
}

/// Downloads the installer to `target`, reporting whole percentages, and
/// checks its size and SHA-256 against the ones GitHub reported.
pub async fn download_installer(
    http: Arc<dyn HttpClient>,
    installer: &SoloInstaller,
    current: &Version,
    target: &Path,
    on_progress: impl FnMut(u8),
) -> Result<()> {
    download_asset(http, installer, "installer", current, target, on_progress).await
}

/// Downloads a release file (`what` names it in errors) to `target`,
/// reporting whole percentages, and checks its size and SHA-256 against the
/// ones GitHub reported. A file that doesn't match is deleted.
pub(crate) async fn download_asset(
    http: Arc<dyn HttpClient>,
    installer: &SoloInstaller,
    what: &str,
    current: &Version,
    target: &Path,
    mut on_progress: impl FnMut(u8),
) -> Result<()> {
    // SOLO: fails closed: nothing is downloaded to be run without the hash
    // GitHub reports for it.
    let Some(expected) = installer.sha256.clone() else {
        anyhow::bail!(
            "GitHub reported no SHA-256 for {}, so the {what} can't be checked and wasn't downloaded; download it from the release page instead",
            installer.name
        );
    };
    let request = Request::get(installer.url.as_str())
        .header("Accept", "application/octet-stream")
        .header("User-Agent", user_agent(current))
        .follow_redirects(RedirectPolicy::FollowAll)
        .timeout(DOWNLOAD_TIMEOUT)
        .body(AsyncBody::default())?;
    let mut response = http
        .send(request)
        .await
        .with_context(|| format!("couldn't download the {what}"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "downloading the {what} failed: {}",
        response.status()
    );

    let total = if installer.size > 0 {
        Some(installer.size)
    } else {
        response
            .headers()
            .get(http_client::http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|total| *total > 0)
    };

    let mut file = smol::fs::File::create(target)
        .await
        .with_context(|| format!("couldn't create {}", target.display()))?;
    let mut written: u64 = 0;
    let mut last_percent = None;
    let mut buffer = vec![0u8; 64 * 1024];
    let body = response.body_mut();
    loop {
        let read = body.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).await?;
        written += read as u64;
        if let Some(total) = total {
            let percent = download_percent(written, total);
            if last_percent != Some(percent) {
                last_percent = Some(percent);
                on_progress(percent);
            }
        }
    }
    file.flush().await?;
    drop(file);

    if installer.size > 0 && written != installer.size {
        smol::fs::remove_file(target).await.ok();
        anyhow::bail!(
            "the downloaded {what} is incomplete ({written} of {} bytes)",
            installer.size
        );
    }
    let path = target.to_path_buf();
    let actual = smol::unblock(move || sha256_of_file(&path)).await?;
    if actual != expected {
        smol::fs::remove_file(target).await.ok();
        anyhow::bail!("the downloaded {what} is corrupt: its SHA-256 is {actual}, not {expected}");
    }
    Ok(())
}

fn sha256_of_file(path: &Path) -> Result<String> {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("couldn't read {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

const UPDATE_HELPER: &str = "solo-update-helper.exe";

/// The arguments for `solo-update-helper` (the `solo_update_helper` crate):
/// wait for Solo (`pid`) to exit, then run the installer silently into
/// `install_dir`, and have it start Solo again if `relaunch`.
pub fn installer_helper_args(
    pid: u32,
    installer: &Path,
    install_dir: &Path,
    log_file: &Path,
    relaunch: bool,
) -> Vec<std::ffi::OsString> {
    let mut args = vec![
        pid.to_string().into(),
        installer.as_os_str().to_owned(),
        install_dir.as_os_str().to_owned(),
        log_file.as_os_str().to_owned(),
    ];
    if !relaunch {
        args.push("--no-launch".into());
    }
    args
}

#[cfg(target_os = "windows")]
fn spawn_installer_helper(
    installer: &Path,
    install_dir: &Path,
    log_file: &Path,
    relaunch: bool,
) -> Result<()> {
    use smol::process::windows::CommandExt as _;
    use std::process::Stdio;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // The installer replaces the helper next to solo.exe, so it runs from a
    // copy next to the installer.
    let helper = install_dir.join(UPDATE_HELPER);
    let helper_copy = installer
        .parent()
        .context("the installer has no folder")?
        .join(UPDATE_HELPER);
    std::fs::copy(&helper, &helper_copy).with_context(|| {
        format!(
            "couldn't copy the update helper {}; reinstall Solo from its release page",
            helper.display()
        )
    })?;
    let args = installer_helper_args(
        std::process::id(),
        installer,
        install_dir,
        log_file,
        relaunch,
    );
    let spawn = |flags: u32| {
        smol::process::Command::new(&helper_copy)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    // Break away from Solo's job (if any) so the helper outlives Solo; not
    // every job allows that, so fall back to a plain detached spawn.
    let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(_child) => Ok(()),
        Err(error) => {
            log::warn!("solo update: spawning helper outside the job failed: {error}");
            spawn(base)
                .map(|_child| ())
                .context("couldn't start the update helper")
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn spawn_installer_helper(
    _installer: &Path,
    _install_dir: &Path,
    _log_file: &Path,
    _relaunch: bool,
) -> Result<()> {
    anyhow::bail!("installing updates is only supported on Windows")
}

/// Whether this platform can install updates (Windows installer only).
fn can_install() -> bool {
    cfg!(target_os = "windows")
}

/// The most characters of the release notes' summary the update menu shows.
const SUMMARY_MAX_CHARS: usize = 200;

/// One sentence of the release notes for the update menu, in the UI's
/// language, as plain text.
///
/// Solo's release notes start with a summary in Chinese on the first line and
/// in English on the second, each one plain sentence (checked by
/// `script/release-solo.ps1`). Notes that don't (older releases') give the
/// plain text of their first paragraph that isn't only headings.
pub fn notes_summary(notes: &str, chinese: bool) -> Option<String> {
    let mut lines = notes.lines().map(str::trim).filter(|line| !line.is_empty());
    let summary = match (lines.next(), lines.next()) {
        (Some(zh), Some(en)) if is_summary_line(zh, true) && is_summary_line(en, false) => {
            if chinese { zh } else { en }.to_string()
        }
        _ => first_paragraph_text(notes)?,
    };
    Some(truncate_at_boundary(&summary, SUMMARY_MAX_CHARS))
}

/// Whether `line` is one of the summary lines the release notes start with:
/// plain text, with Chinese in it or not.
fn is_summary_line(line: &str, chinese: bool) -> bool {
    let markdown = line.starts_with(['#', '-', '*', '>', '[', '!', '|', '`', '<', '+'])
        || line.contains("](")
        || line.contains("://");
    let has_cjk = line.chars().any(is_cjk);
    !markdown && has_cjk == chinese && (chinese || line.chars().any(char::is_alphabetic))
}

fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// The plain text of the first paragraph with any, skipping headings.
fn first_paragraph_text(notes: &str) -> Option<String> {
    let mut paragraph = Vec::new();
    for line in notes.lines().map(str::trim) {
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let text = plain_text(line);
        if !text.is_empty() {
            paragraph.push(text);
        }
    }
    (!paragraph.is_empty()).then(|| paragraph.join(" "))
}

/// One line of markdown as plain text: no list or quote markers, emphasis or
/// code marks; links as their text, without addresses.
fn plain_text(line: &str) -> String {
    let mut line = line.trim_start_matches(['>', ' ']);
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            line = rest;
        }
    }
    if let Some((number, rest)) = line.split_once(". ")
        && !number.is_empty()
        && number.chars().all(|character| character.is_ascii_digit())
    {
        line = rest;
    }
    let mut text = String::new();
    let mut rest = line;
    while !rest.is_empty() {
        // `[text](address)` and `![alt](address)`: the text.
        if let Some(after) = rest.strip_prefix("![").or_else(|| rest.strip_prefix('['))
            && let Some((label, after)) = after.split_once("](")
            && let Some((_, after)) = after.split_once(')')
        {
            text.push_str(label);
            rest = after;
            continue;
        }
        let character = rest.chars().next().unwrap_or_default();
        if !matches!(character, '*' | '`' | '~') {
            text.push(character);
        }
        rest = &rest[character.len_utf8()..];
    }
    text.split_whitespace()
        .filter(|word| !word.contains("://") && !word.starts_with("www."))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` cut to at most `max_chars` (and "…"), between words or, in
/// Chinese, between characters.
fn truncate_at_boundary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    let next = text.chars().nth(max_chars).unwrap_or(' ');
    let mid_word = |character: char| character.is_alphanumeric() && !is_cjk(character);
    let kept = match kept.chars().last() {
        Some(last) if mid_word(last) && mid_word(next) => match kept.rfind(char::is_whitespace) {
            Some(space) => kept[..space].to_string(),
            None => kept,
        },
        _ => kept,
    };
    format!("{}…", kept.trim_end())
}

fn read_skipped_version(cx: &App) -> Task<Option<Version>> {
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.read_kvp(SKIPPED_VERSION_KEY)
            .ok()
            .flatten()
            .and_then(|version| parse_version(&version))
    })
}

/// When a downloaded update is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallWhen {
    /// Now: Solo quits, and the installer starts it again.
    Restart,
    /// When the user quits Solo; it isn't started again.
    OnQuit,
}

#[derive(Clone, Debug, PartialEq)]
enum Activity {
    Idle,
    Checking,
    Downloading {
        version: Version,
        percent: Option<u8>,
        when: InstallWhen,
        /// Replacing an update waiting for Solo to quit, without asking.
        quiet: bool,
    },
    Installing,
}

/// A downloaded update, installed when Solo quits.
#[derive(Clone, Debug, PartialEq)]
struct ReadyOnQuit {
    version: Version,
    installer: PathBuf,
    install_dir: PathBuf,
    log_file: PathBuf,
}

pub struct SoloUpdater {
    current_version: Version,
    activity: Activity,
    /// Survives other activity: a newer version may be checked for and
    /// downloaded meanwhile.
    ready_on_quit: Option<ReadyOnQuit>,
    /// What the last check found.
    checked: UpdateStatus,
    /// The release the update button offers.
    release: Option<SoloRelease>,
    /// What "retry" in the update button's menu does after a failure.
    retry: Option<Retry>,
    /// The update button's state, which this keeps up to date.
    entry: Entity<UpdateEntry>,
    poll_task: Option<Task<()>>,
    _quit_subscription: Option<gpui::Subscription>,
}

/// What failed, to try again.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Retry {
    Check,
    Install(InstallWhen),
}

/// What Solo knows about updates, for the About window.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateStatus {
    /// Not checked yet since Solo started.
    NotChecked,
    Checking,
    UpToDate,
    /// A newer version, skipped by the user or not.
    Available(Version),
    Failed,
}

/// The updater, in stable builds (others don't update).
pub fn updater(cx: &App) -> Option<Entity<SoloUpdater>> {
    Some(cx.try_global::<GlobalSoloUpdater>()?.0.clone())
}

/// What a check's outcome says about updates.
fn update_status(outcome: &Result<CheckOutcome>) -> UpdateStatus {
    match outcome {
        Ok(CheckOutcome::UpdateAvailable(release)) => {
            UpdateStatus::Available(release.version.clone())
        }
        Ok(CheckOutcome::Skipped(version)) => UpdateStatus::Available(version.clone()),
        Ok(CheckOutcome::UpToDate) => UpdateStatus::UpToDate,
        Err(_) => UpdateStatus::Failed,
    }
}

/// What to tell the user about the update the last run was installing.
#[derive(Clone, Debug, PartialEq)]
pub enum StartupNotice {
    Updated(Version),
    NotInstalled(Version),
}

/// Compares the version an update was installing with the one running now.
pub fn startup_notice(current: &Version, pending: Option<&Version>) -> Option<StartupNotice> {
    let pending = pending?;
    if without_build_metadata(current) >= without_build_metadata(pending) {
        Some(StartupNotice::Updated(current.clone()))
    } else {
        Some(StartupNotice::NotInstalled(pending.clone()))
    }
}

struct GlobalSoloUpdater(Entity<SoloUpdater>);

impl Global for GlobalSoloUpdater {}

struct UpdateStatusNotification;

/// Starts the GitHub Releases updater (stable builds only, not the
/// Microsoft Store's).
pub fn init(cx: &mut App) {
    if ReleaseChannel::try_global(cx) != Some(ReleaseChannel::Stable) || is_store_package() {
        return;
    }
    let current_version = release_channel::AppVersion::global(cx);
    let entry = update_button::entry_or_init(cx);
    let updater = cx.new(|cx| {
        let mut updater = SoloUpdater::new(current_version, entry.clone());
        updater._quit_subscription = Some(cx.on_app_quit(|updater: &mut SoloUpdater, _| {
            updater.install_ready_update_at_quit();
            async {}
        }));
        updater.sync_polling(cx);
        cx.observe_global::<SettingsStore>(|updater, cx| updater.sync_polling(cx))
            .detach();
        updater
    });
    let weak = updater.downgrade();
    entry.update(cx, |entry, _| {
        entry.set_handler(move |action, _window, cx| {
            if let Some(updater) = weak.upgrade() {
                updater.update(cx, |updater, cx| updater.act(action, cx));
            }
        })
    });
    cx.set_global(GlobalSoloUpdater(updater.clone()));
    report_last_update(updater.read(cx).current_version.clone(), cx);
}

/// Says whether the update the last run was installing went in.
fn report_last_update(current: Version, cx: &mut App) {
    let kvp = KeyValueStore::global(cx);
    cx.spawn(async move |cx| {
        let pending = kvp
            .read_kvp(PENDING_VERSION_KEY)
            .ok()
            .flatten()
            .and_then(|version| parse_version(&version));
        let Some(notice) = startup_notice(&current, pending.as_ref()) else {
            return;
        };
        kvp.delete_kvp(PENDING_VERSION_KEY.to_string())
            .await
            .log_err();
        let chinese = util::i18n::is_chinese();
        let message = match notice {
            StartupNotice::Updated(version) => {
                log::info!("solo update: updated to {version}");
                if chinese {
                    format!("Solo 已更新到 {version}。")
                } else {
                    format!("Solo was updated to {version}.")
                }
            }
            StartupNotice::NotInstalled(version) => {
                let log = update_download_dir().join("install.log");
                log::warn!("solo update: {version} wasn't installed");
                if chinese {
                    format!("Solo {version} 没有装上，日志在 {}。", log.display())
                } else {
                    format!(
                        "Solo {version} wasn't installed; the log is in {}.",
                        log.display()
                    )
                }
            }
        };
        cx.update(|cx| show_status_message(message, cx));
    })
    .detach();
}

fn write_pending_version(version: &Version, cx: &App) -> Task<()> {
    let kvp = KeyValueStore::global(cx);
    let version = version.to_string();
    cx.background_spawn(async move {
        kvp.write_kvp(PENDING_VERSION_KEY.to_string(), version)
            .await
            .log_err();
    })
}

/// Where this Solo downloads updates: a folder of its own, so another Solo
/// (another channel, say) doesn't delete an installer waiting to run.
fn update_download_dir() -> PathBuf {
    std::env::temp_dir()
        .join("solo-update")
        .join(std::process::id().to_string())
}

/// The names of the programs (agents) that quitting Solo now would end.
pub fn running_program_names(cx: &App) -> Vec<String> {
    running_programs(cx)
        .iter()
        .map(|program| program.name.to_string())
        .collect()
}

/// The programs running in Solo's windows that quitting would end, e.g.
/// agents in terminals (not those that keep running in a terminal host).
fn running_programs(cx: &App) -> Vec<RunningProgram> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .filter_map(|window| window.read(cx).ok())
        .flat_map(|multi_workspace| multi_workspace.workspaces().cloned().collect::<Vec<_>>())
        .flat_map(|workspace| {
            workspace
                .read(cx)
                .items_with_panels(cx)
                .iter()
                .filter(|item| !item.keeps_running_after_quit(cx))
                .filter_map(|item| item.running_program(cx))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The "check for updates" command: always checks, and reports back on the
/// update button. The Microsoft Store's Solo asks the Store
/// ([`crate::store_update`]).
pub fn check_manually(window: &mut Window, cx: &mut App) {
    if is_store_package() {
        crate::store_update::check_manually(cx);
        return;
    }
    match cx.try_global::<GlobalSoloUpdater>() {
        Some(global) => {
            let updater = global.0.clone();
            updater.update(cx, |updater, cx| updater.check(UpdateCheckType::Manual, cx));
        }
        None => {
            drop(window.prompt(
                gpui::PromptLevel::Info,
                util::i18n::tr(
                    "Update checks are only available in stable builds",
                    "只有正式版会检查更新",
                ),
                Some(&format!(
                    "{} https://github.com/{SOLO_GITHUB_REPO}/releases",
                    util::i18n::tr("Download Solo from", "可在此下载 Solo：")
                )),
                &[util::i18n::tr("OK", "确定")],
                cx,
            ));
        }
    }
}

fn show_status_message(message: String, cx: &mut App) {
    show_app_notification(
        NotificationId::unique::<UpdateStatusNotification>(),
        cx,
        move |cx| {
            let message = message.clone();
            cx.new(|cx| MessageNotification::new(message, cx).show_suppress_button(false))
        },
    );
}

/// The most characters of a failure's reason the update menu shows.
const REASON_MAX_CHARS: usize = 160;

/// Why a check or an update failed, for the update menu: `prefix` and the
/// error, cut short.
fn failure_reason(prefix: &str, error: &anyhow::Error) -> String {
    truncate_at_boundary(&format!("{prefix}{error:#}"), REASON_MAX_CHARS)
}

/// Whether Solo can install `release` itself.
fn can_install_release(release: &SoloRelease) -> bool {
    can_install() && release.installer.is_some()
}

/// The page the update menu opens for `release`: its release notes, or the
/// Microsoft Store page when Solo can't install it itself.
fn page_for(release: &SoloRelease) -> &str {
    if can_install_release(release) {
        &release.html_url
    } else {
        STORE_PAGE_URL
    }
}

/// What the update button offers for `release`.
fn offer_for(release: &SoloRelease) -> UpdateOffer {
    UpdateOffer {
        version: Some(release.version.clone()),
        notes: Some(release.notes.clone()),
        notes_url: Some(page_for(release).to_string()),
        can_install: can_install_release(release),
        can_install_on_quit: true,
    }
}

impl SoloUpdater {
    fn new(current_version: Version, entry: Entity<UpdateEntry>) -> Self {
        SoloUpdater {
            current_version,
            activity: Activity::Idle,
            ready_on_quit: None,
            checked: UpdateStatus::NotChecked,
            release: None,
            retry: None,
            entry,
            poll_task: None,
            _quit_subscription: None,
        }
    }

    /// What's known about updates: whether a check runs, or what the last
    /// one found.
    pub fn status(&self) -> UpdateStatus {
        match self.activity {
            Activity::Checking => UpdateStatus::Checking,
            _ => self.checked.clone(),
        }
    }

    fn show(&self, state: ButtonState, cx: &mut Context<Self>) {
        self.entry
            .update(cx, |entry, cx| entry.set_state(state, cx));
    }

    /// Offers `release` on the update button.
    fn offer(&mut self, release: SoloRelease, state: ButtonState, cx: &mut Context<Self>) {
        let offer = offer_for(&release);
        self.release = Some(release);
        self.entry.update(cx, |entry, cx| {
            entry.set_offer(Some(offer), cx);
            entry.set_state(state, cx);
        });
    }

    /// No update to offer: the button goes (or says "up to date" first).
    fn withdraw(&mut self, up_to_date: bool, cx: &mut Context<Self>) {
        self.release = None;
        self.entry.update(cx, |entry, cx| {
            entry.set_offer(None, cx);
            if up_to_date {
                entry.show_up_to_date(cx);
            } else {
                entry.set_state(ButtonState::Hidden, cx);
            }
        });
    }

    fn sync_polling(&mut self, cx: &mut Context<Self>) {
        let enabled = AutoUpdateSetting::get_global(cx).0;
        if enabled && self.poll_task.is_none() {
            self.poll_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FIRST_CHECK_DELAY).await;
                loop {
                    if this
                        .update(cx, |this, cx| this.check(UpdateCheckType::Automatic, cx))
                        .is_err()
                    {
                        break;
                    }
                    cx.background_executor().timer(POLL_INTERVAL).await;
                }
            }));
        } else if !enabled {
            self.poll_task = None;
        }
    }

    /// Carries out what the user picked in the update button's menu.
    fn act(&mut self, action: UpdateAction, cx: &mut Context<Self>) {
        match action {
            UpdateAction::Restart | UpdateAction::OnQuit => {
                let when = if action == UpdateAction::Restart {
                    InstallWhen::Restart
                } else {
                    InstallWhen::OnQuit
                };
                if let Some(release) = self.release.clone() {
                    self.install(release, when, false, cx);
                }
            }
            UpdateAction::ViewNotes => {
                if let Some(release) = &self.release {
                    cx.open_url(page_for(release));
                }
            }
            UpdateAction::Skip => self.skip(cx),
            UpdateAction::Retry => match (self.retry.take(), self.release.clone()) {
                (Some(Retry::Install(when)), Some(release)) => {
                    self.install(release, when, false, cx)
                }
                _ => self.check(UpdateCheckType::Manual, cx),
            },
        }
    }

    /// "Skip this version": the button goes until a newer version comes out.
    fn skip(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.release.clone() else {
            return;
        };
        let version = release.version;
        self.forget_skipped(&version);
        if let Some(kvp) = crate::store_update::key_value_store(cx) {
            let skipped = version.to_string();
            cx.background_spawn(async move {
                kvp.write_kvp(SKIPPED_VERSION_KEY.to_string(), skipped)
                    .await
                    .log_err();
            })
            .detach();
        }
        log::info!("solo update: skipping {version}");
        self.withdraw(false, cx);
    }

    fn check(&mut self, check_type: UpdateCheckType, cx: &mut Context<Self>) {
        // A download or an install shows on the button already.
        if !matches!(self.activity, Activity::Idle) {
            return;
        }
        // SOLO: no automatic checks in offline mode.
        if net_guard::is_offline() && !check_type.is_manual() {
            return;
        }
        self.activity = Activity::Checking;
        cx.notify();
        if check_type.is_manual() {
            self.show(ButtonState::Checking, cx);
        }

        let http = cx.http_client();
        let current = self.current_version.clone();
        let skipped = read_skipped_version(cx);
        cx.spawn(async move |this, cx| {
            let skipped = skipped.await;
            let outcome = check_for_update(http, &current, skipped.as_ref(), check_type).await;
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                cx.notify();
                this.handle_outcome(outcome, check_type, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Shows what a check found on the update button: never a notification.
    fn handle_outcome(
        &mut self,
        outcome: Result<CheckOutcome>,
        check_type: UpdateCheckType,
        cx: &mut Context<Self>,
    ) {
        self.checked = update_status(&outcome);
        let ready = self
            .ready_on_quit
            .as_ref()
            .map(|ready| ready.version.clone());
        match outcome {
            Ok(CheckOutcome::UpdateAvailable(release)) if ready.is_some() => {
                if ready.as_ref() == Some(&release.version) {
                    self.offer(release, ButtonState::OnQuit, cx);
                } else {
                    // The user chose to update when Solo quits: they get the
                    // newest version then, without being asked again.
                    log::info!(
                        "solo update: {} replaces the update waiting for Solo to quit",
                        release.version
                    );
                    self.show(ButtonState::OnQuit, cx);
                    self.install(release, InstallWhen::OnQuit, true, cx);
                }
            }
            Ok(CheckOutcome::UpdateAvailable(release)) => {
                log::info!("solo update: {} is available", release.version);
                self.offer(release, ButtonState::Available, cx);
            }
            Ok(CheckOutcome::UpToDate) => {
                log::info!("solo update: up to date ({})", self.current_version);
                self.withdraw(check_type.is_manual(), cx);
            }
            Ok(CheckOutcome::Skipped(version)) => {
                log::info!("solo update: {version} is available but was skipped");
                if ready.is_some() {
                    self.show(ButtonState::OnQuit, cx);
                } else {
                    self.withdraw(false, cx);
                }
            }
            Err(error) => {
                log::warn!("solo update: check failed: {error:#}");
                if check_type.is_manual() {
                    self.retry = Some(Retry::Check);
                    let prefix = util::i18n::tr("Couldn't check for updates: ", "检查更新失败：");
                    self.show(
                        ButtonState::Failed {
                            reason: failure_reason(prefix, &error),
                        },
                        cx,
                    );
                } else if ready.is_some() {
                    self.show(ButtonState::OnQuit, cx);
                } else if self.release.is_some() {
                    self.show(ButtonState::Available, cx);
                }
                // Otherwise an automatic check says nothing; it's tried again
                // later.
            }
        }
    }

    fn install(
        &mut self,
        release: SoloRelease,
        when: InstallWhen,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.activity, Activity::Idle) {
            return;
        }
        let Some(installer) = release.installer.clone() else {
            // A newer release without an installer while an older one waits
            // for Solo to quit: that one still goes in, and the next run
            // offers this release's page.
            if quiet {
                log::info!(
                    "solo update: {} has no installer; keeping the update waiting for Solo to quit",
                    release.version
                );
                return;
            }
            cx.open_url(STORE_PAGE_URL);
            return;
        };
        let version = release.version.clone();
        // Already downloaded, to install when Solo quits.
        let downloaded = self
            .ready_on_quit
            .clone()
            .filter(|ready| ready.version == version && ready.installer.is_file());
        if downloaded.is_some() && when == InstallWhen::OnQuit {
            self.offer(release, ButtonState::OnQuit, cx);
            return;
        }
        self.activity = Activity::Downloading {
            version: version.clone(),
            percent: None,
            when,
            quiet,
        };
        cx.notify();
        if !quiet {
            self.show(ButtonState::Downloading { percent: None }, cx);
        }

        let http = cx.http_client();
        let current = self.current_version.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let ready = match downloaded {
                    Some(ready) => ready,
                    None => {
                        download(this.clone(), http, installer, version, current, cx.clone())
                            .await?
                    }
                };
                cx.update(|cx| write_pending_version(&ready.version, cx))
                    .await;
                match when {
                    InstallWhen::Restart => install_and_restart(this.clone(), ready, cx).await,
                    InstallWhen::OnQuit => {
                        this.update(cx, |this, _| {
                            if let Some(previous) = this.ready_on_quit.replace(ready.clone())
                                && previous.installer != ready.installer
                            {
                                std::fs::remove_file(&previous.installer).ok();
                            }
                        })?;
                        log::info!(
                            "solo update: {} will be installed when Solo quits",
                            ready.version
                        );
                        Ok(InstallResult::OnQuit)
                    }
                }
            }
            .await;
            this.update(cx, |this, cx| {
                this.finish_install(result, release, when, quiet, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Shows how an update went on the button.
    fn finish_install(
        &mut self,
        result: Result<InstallResult>,
        release: SoloRelease,
        when: InstallWhen,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        if !matches!(result, Ok(InstallResult::Quitting)) {
            self.activity = Activity::Idle;
            cx.notify();
        }
        match result {
            Ok(InstallResult::Quitting) => {}
            Ok(InstallResult::OnQuit) => self.offer(release, ButtonState::OnQuit, cx),
            Ok(InstallResult::QuitCancelled) => {
                log::info!("solo update: cancelled because Solo didn't quit");
                self.offer(release, ButtonState::Available, cx);
            }
            Err(error) => {
                log::error!("solo update: install failed: {error:#}");
                if quiet {
                    // The update already waiting for Solo to quit still is.
                    self.show(ButtonState::OnQuit, cx);
                    return;
                }
                self.retry = Some(Retry::Install(when));
                let reason = failure_reason("", &error);
                self.offer(release, ButtonState::Failed { reason }, cx);
            }
        }
    }

    /// Starts the helper for an update waiting for Solo to quit. It can't stop
    /// Solo from quitting: problems are only logged.
    fn install_ready_update_at_quit(&mut self) {
        let Some(ready) = self.ready_on_quit.take() else {
            return;
        };
        if !ready.installer.is_file() {
            log::error!(
                "solo update: the installer {} is gone",
                ready.installer.display()
            );
            return;
        }
        match spawn_installer_helper(&ready.installer, &ready.install_dir, &ready.log_file, false) {
            Ok(()) => log::info!("solo update: installing {} as Solo quits", ready.version),
            Err(error) => log::error!("solo update: couldn't install at quit: {error:#}"),
        }
    }

    /// Forgets a downloaded update the user skipped.
    fn forget_skipped(&mut self, version: &Version) {
        if let Some(ready) = self
            .ready_on_quit
            .take_if(|ready| &ready.version == version)
        {
            std::fs::remove_file(&ready.installer).ok();
            log::info!("solo update: {version} was skipped; it won't be installed");
        }
    }
}

enum InstallResult {
    Quitting,
    QuitCancelled,
    OnQuit,
}

/// Downloads and checks the installer.
async fn download(
    this: gpui::WeakEntity<SoloUpdater>,
    http: Arc<dyn HttpClient>,
    installer: SoloInstaller,
    version: Version,
    current: Version,
    cx: AsyncApp,
) -> Result<ReadyOnQuit> {
    let install_dir = std::env::current_exe()
        .context("couldn't locate Solo")?
        .parent()
        .context("couldn't locate Solo's folder")?
        .to_path_buf();
    // A folder per version: an installer waiting for Solo to quit stays until
    // its replacement is downloaded.
    let download_dir = update_download_dir().join(version.to_string());
    if smol::fs::metadata(&download_dir).await.is_ok() {
        smol::fs::remove_dir_all(&download_dir).await.ok();
    }
    smol::fs::create_dir_all(&download_dir)
        .await
        .with_context(|| format!("couldn't create {}", download_dir.display()))?;
    let target = download_dir.join(&installer.name);
    let log_file = update_download_dir().join("install.log");

    download_installer(http, &installer, &current, &target, move |percent| {
        this.update(&mut cx.clone(), |this, cx| {
            if let Activity::Downloading {
                percent: p, quiet, ..
            } = &mut this.activity
            {
                *p = Some(percent);
                let quiet = *quiet;
                cx.notify();
                if !quiet {
                    this.show(
                        ButtonState::Downloading {
                            percent: Some(percent),
                        },
                        cx,
                    );
                }
            }
        })
        .ok();
    })
    .await?;
    log::info!("solo update: downloaded {}", target.display());
    Ok(ReadyOnQuit {
        version,
        installer: target,
        install_dir,
        log_file,
    })
}

/// Quits Solo through the normal quit flow and installs the update.
async fn install_and_restart(
    this: gpui::WeakEntity<SoloUpdater>,
    ready: ReadyOnQuit,
    cx: &mut AsyncApp,
) -> Result<InstallResult> {
    this.update(cx, |this, cx| {
        this.activity = Activity::Installing;
        cx.notify();
        this.show(ButtonState::Installing, cx);
    })?;

    // Close every window the way Quit does, so unsaved files still prompt.
    let windows = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .filter_map(|window| window.downcast::<MultiWorkspace>())
            .collect::<Vec<_>>()
    });
    if !workspace::prepare_windows_to_quit(&windows, cx).await {
        return Ok(InstallResult::QuitCancelled);
    }

    // Installing now: not again at quit.
    this.update(cx, |this, _| this.ready_on_quit = None).ok();
    spawn_installer_helper(&ready.installer, &ready.install_dir, &ready.log_file, true)?;
    log::info!(
        "solo update: helper started; installing into {}",
        ready.install_dir.display()
    );
    cx.update(|cx| cx.quit());
    Ok(InstallResult::Quitting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};

    fn release_json(tag: &str, prerelease: bool, assets: &str) -> String {
        format!(
            r#"{{
                "tag_name": "{tag}",
                "name": "Solo {tag}",
                "html_url": "https://github.com/lucas9801/solo/releases/tag/{tag}",
                "body": "\n## What's new\n\n- Faster terminals\n",
                "draft": false,
                "prerelease": {prerelease},
                "assets": [{assets}]
            }}"#
        )
    }

    const SETUP_ASSET: &str = r#"
        {"name": "Solo-1.29.0-windows-x64.zip", "size": 10, "browser_download_url": "https://example.com/solo.zip"},
        {"name": "Solo-Setup-1.29.0.exe", "size": 1234, "browser_download_url": "https://example.com/Solo-Setup-1.29.0.exe",
         "digest": "sha256:2C26B46B68FFC68FF99B453C1D30413413422D706483BFA0F98A5E886266E7AE"}
    "#;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_release_with_installer() {
        let release =
            parse_release(release_json("v1.29.0", false, SETUP_ASSET).as_bytes()).unwrap();
        assert_eq!(release.version, version("1.29.0"));
        assert_eq!(
            release.html_url,
            "https://github.com/lucas9801/solo/releases/tag/v1.29.0"
        );
        assert!(release.notes.contains("Faster terminals"));
        assert!(!release.prerelease);
        assert_eq!(
            release.installer,
            Some(SoloInstaller {
                name: "Solo-Setup-1.29.0.exe".into(),
                url: "https://example.com/Solo-Setup-1.29.0.exe".into(),
                size: 1234,
                sha256: Some(
                    "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae".into()
                ),
            })
        );
    }

    #[test]
    fn parses_release_without_installer_or_v_prefix() {
        let assets = r#"{"name": "Solo-1.29.0.zip", "size": 10, "browser_download_url": "https://example.com/solo.zip"}"#;
        let release = parse_release(release_json("1.29.0", false, assets).as_bytes()).unwrap();
        assert_eq!(release.version, version("1.29.0"));
        assert_eq!(release.installer, None);

        let release = parse_release(release_json("v1.29.0", false, "").as_bytes()).unwrap();
        assert_eq!(release.installer, None);
    }

    #[test]
    fn parses_prerelease_and_bad_tags() {
        let release =
            parse_release(release_json("v1.30.0-beta.1", true, SETUP_ASSET).as_bytes()).unwrap();
        assert!(release.prerelease);
        assert_eq!(release.version, version("1.30.0-beta.1"));
        // Never offered, even though it's newer.
        assert_eq!(
            evaluate_release(&version("1.28.1"), release, None, UpdateCheckType::Manual),
            CheckOutcome::UpToDate
        );

        assert!(parse_release(release_json("nightly", false, "").as_bytes()).is_err());
        assert!(parse_release(b"{\"message\": \"Not Found\"}").is_err());
    }

    /// The installer's name becomes a file name in the download folder: one
    /// that leads out of it isn't an installer (R3 L-U1).
    #[test]
    fn installer_names_stay_in_the_download_folder() {
        assert!(is_installer_asset("Solo-Setup-1.29.0.exe"));
        assert!(!is_installer_asset(r"Solo-Setup-..\..\evil.exe"));
        assert!(!is_installer_asset("Solo-Setup-../../evil.exe"));
        assert!(!is_installer_asset("Solo-Setup-C:evil.exe"));
    }

    /// Releases also carry the installer as `Solo-Setup.exe` (a copy under a
    /// fixed name for README's `releases/latest/download/` links); the
    /// updater only takes the versioned one.
    #[test]
    fn fixed_name_installer_isnt_the_update() {
        assert!(!is_installer_asset("Solo-Setup.exe"));
        assert!(!is_installer_asset("solo-setup.exe"));

        let versioned = r#"{"name": "Solo-Setup-2.5.3.exe", "size": 1234, "browser_download_url": "https://example.com/Solo-Setup-2.5.3.exe"}"#;
        let fixed = r#"{"name": "Solo-Setup.exe", "size": 1234, "browser_download_url": "https://example.com/Solo-Setup.exe"}"#;
        for assets in [
            format!("{fixed}, {versioned}"),
            format!("{versioned}, {fixed}"),
        ] {
            let release = parse_release(release_json("v2.5.3", false, &assets).as_bytes()).unwrap();
            let installer = release.installer.unwrap();
            assert_eq!(installer.name, "Solo-Setup-2.5.3.exe", "assets: {assets}");
            assert_eq!(installer.url, "https://example.com/Solo-Setup-2.5.3.exe");
        }

        // A release with only the fixed name has no installer for the updater.
        let release = parse_release(release_json("v2.5.3", false, fixed).as_bytes()).unwrap();
        assert!(release.installer.is_none());
    }

    #[test]
    fn download_percents_dont_overflow() {
        assert_eq!(download_percent(50, 200), 25);
        assert_eq!(download_percent(300, 200), 100);
        assert_eq!(download_percent(u64::MAX / 2, u64::MAX), 49);
        assert_eq!(download_percent(u64::MAX, u64::MAX), 100);
    }

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("v1.28.1"), Some(version("1.28.1")));
        assert_eq!(parse_version("V1.28.1"), Some(version("1.28.1")));
        assert_eq!(parse_version(" 1.28.1 "), Some(version("1.28.1")));
        assert_eq!(parse_version("v1.29"), Some(version("1.29.0")));
        assert_eq!(parse_version("release-1"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer(&version("1.28.1"), &version("1.28.2")));
        assert!(is_newer(&version("1.28.1"), &version("1.29.0")));
        assert!(is_newer(&version("1.9.0"), &version("1.10.0")));
        assert!(!is_newer(&version("1.28.1"), &version("1.28.1")));
        assert!(!is_newer(&version("1.28.1"), &version("1.28.0")));
        assert!(!is_newer(&version("1.28.1+abc"), &version("1.28.1")));
        assert!(is_newer(&version("1.29.0-dev"), &version("1.29.0")));
    }

    fn release(text: &str) -> SoloRelease {
        SoloRelease {
            version: version(text),
            html_url: "https://example.com".into(),
            notes: String::new(),
            prerelease: false,
            installer: None,
        }
    }

    #[test]
    fn release_with_installer_and_remote_servers_picks_the_installer() {
        // A release as solo-package.yml makes it: the installer next to the
        // Linux remote servers, which aren't installers whatever their order.
        let assets = r#"
            {"name": "solo-remote-server-linux-aarch64.gz", "size": 30, "browser_download_url": "https://example.com/aarch64.gz"},
            {"name": "solo-remote-server-linux-x86_64.gz", "size": 40, "browser_download_url": "https://example.com/x86_64.gz"},
            {"name": "Solo-Setup-2.5.1.exe", "size": 1234, "browser_download_url": "https://example.com/Solo-Setup-2.5.1.exe",
             "digest": "sha256:2C26B46B68FFC68FF99B453C1D30413413422D706483BFA0F98A5E886266E7AE"}
        "#;
        let release = parse_release(release_json("v2.5.1", false, assets).as_bytes()).unwrap();
        let installer = release.installer.clone().unwrap();
        assert_eq!(installer.name, "Solo-Setup-2.5.1.exe");
        assert_eq!(installer.url, "https://example.com/Solo-Setup-2.5.1.exe");
        assert_eq!(installer.size, 1234);
        assert!(installer.sha256.is_some());
        assert_eq!(
            evaluate_release(
                &version("2.4.2"),
                release.clone(),
                None,
                UpdateCheckType::Automatic
            ),
            CheckOutcome::UpdateAvailable(release.clone())
        );
        if cfg!(target_os = "windows") {
            let offer = offer_for(&release);
            assert!(offer.can_install);
            assert_eq!(offer.notes_url.as_deref(), Some(release.html_url.as_str()));
        }

        // Only the remote servers (2.5.0): nothing to install, the Store page.
        let assets = r#"
            {"name": "solo-remote-server-linux-aarch64.gz", "size": 30, "browser_download_url": "https://example.com/aarch64.gz"},
            {"name": "solo-remote-server-linux-x86_64.gz", "size": 40, "browser_download_url": "https://example.com/x86_64.gz"}
        "#;
        let release = parse_release(release_json("v2.5.0", false, assets).as_bytes()).unwrap();
        assert_eq!(release.installer, None);
        let offer = offer_for(&release);
        assert!(!offer.can_install);
        assert_eq!(offer.notes_url.as_deref(), Some(STORE_PAGE_URL));
    }

    #[test]
    fn release_without_installer_offers_the_store_page() {
        let offer = offer_for(&release("1.29.0"));
        assert!(!offer.can_install);
        assert_eq!(offer.notes_url.as_deref(), Some(STORE_PAGE_URL));
        assert_eq!(page_for(&release("1.29.0")), STORE_PAGE_URL);
    }

    #[test]
    fn skipped_version_only_silences_automatic_checks() {
        let current = version("1.28.1");
        let skipped = version("1.29.0");
        assert_eq!(
            evaluate_release(
                &current,
                release("1.29.0"),
                Some(&skipped),
                UpdateCheckType::Automatic
            ),
            CheckOutcome::Skipped(version("1.29.0"))
        );
        assert_eq!(
            evaluate_release(
                &current,
                release("1.29.0"),
                Some(&skipped),
                UpdateCheckType::Manual
            ),
            CheckOutcome::UpdateAvailable(release("1.29.0"))
        );
        // A newer release than the skipped one shows up again.
        assert_eq!(
            evaluate_release(
                &current,
                release("1.29.1"),
                Some(&skipped),
                UpdateCheckType::Automatic
            ),
            CheckOutcome::UpdateAvailable(release("1.29.1"))
        );
        assert_eq!(
            evaluate_release(&current, release("1.28.1"), None, UpdateCheckType::Manual),
            CheckOutcome::UpToDate
        );
    }

    #[test]
    fn helper_args_keep_paths_whole() {
        let installer =
            Path::new(r"C:\Users\张三\AppData\Local\Temp\solo-update\Solo-Setup-1.29.0.exe");
        let install_dir = Path::new(r"D:\Program Files\终端 Solo's");
        let log_file = Path::new(r"C:\Users\张三\AppData\Local\Temp\solo-update\install.log");
        assert_eq!(
            installer_helper_args(4242, installer, install_dir, log_file, true),
            vec![
                std::ffi::OsString::from("4242"),
                installer.into(),
                install_dir.into(),
                log_file.into(),
            ]
        );
    }

    #[test]
    fn summarizes_release_notes_in_the_ui_language() {
        // v2.1.0's notes as published: a summary in each language first.
        let notes = include_str!("../test_data/release-notes-v2.1.0.md");
        assert_eq!(
            notes_summary(notes, true).as_deref(),
            Some("Solo 2.1：agent 状态更准，常驻终端更可靠，Codex 的会话信息和 Claude 一样详细。")
        );
        assert_eq!(
            notes_summary(notes, false).as_deref(),
            Some(
                "Solo 2.1: more accurate agent status, more reliable terminals that outlive \
                 Solo, and Codex sessions as detailed as Claude's."
            )
        );
        // The same with Windows line ends.
        assert_eq!(
            notes_summary(&notes.replace('\n', "\r\n"), true),
            notes_summary(notes, true)
        );
    }

    #[test]
    fn summarizes_release_notes_without_summary_lines() {
        assert_eq!(notes_summary("\n\n  ", true), None);
        // An older release's: the first paragraph that isn't headings, plain.
        let notes = "## What's new\n\n\
            - **Agent tabs** show [the status](https://example.com/status) of `claude`;\n\
            - see https://github.com/lucas9801/solo for more.\n\n\
            ## Fixes\n\n- Something.";
        let plain = "Agent tabs show the status of claude; see for more.";
        assert_eq!(notes_summary(notes, true).as_deref(), Some(plain));
        assert_eq!(notes_summary(notes, false).as_deref(), Some(plain));
        assert_eq!(
            notes_summary(&notes.replace('\n', "\r\n"), false).as_deref(),
            Some(plain)
        );
        // Links on the second line: not the summary convention either.
        let notes = "Solo 2.1：更准。\n[User guide](https://example.com/guide)\n\nMore.";
        assert_eq!(
            notes_summary(notes, false).as_deref(),
            Some("Solo 2.1：更准。 User guide")
        );
    }

    #[test]
    fn update_status_from_check_outcomes() {
        let version = Version::new(2, 2, 0);
        assert_eq!(
            update_status(&Ok(CheckOutcome::Skipped(version.clone()))),
            UpdateStatus::Available(version)
        );
        assert_eq!(
            update_status(&Ok(CheckOutcome::UpToDate)),
            UpdateStatus::UpToDate
        );
        assert_eq!(
            update_status(&Err(anyhow::anyhow!("offline"))),
            UpdateStatus::Failed
        );
    }

    #[test]
    fn truncates_summaries_between_words() {
        assert_eq!(truncate_at_boundary("short", 10), "short");
        assert_eq!(
            truncate_at_boundary("more reliable terminals", 16),
            "more reliable…"
        );
        assert_eq!(truncate_at_boundary("one, two", 4), "one,…");
        assert_eq!(
            truncate_at_boundary("agent 状态更准，常驻终端更可靠", 10),
            "agent 状态更准…"
        );
        // One long word: cut in it rather than show nothing.
        assert_eq!(truncate_at_boundary("abcdefgh", 4), "abcd…");
    }

    fn fake_http(status: u16, body: String) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |request| {
            let body = body.clone();
            async move {
                assert_eq!(request.uri().to_string(), LATEST_RELEASE_URL);
                assert_eq!(
                    request.headers().get("Accept").unwrap(),
                    "application/vnd.github+json"
                );
                assert_eq!(request.headers().get("User-Agent").unwrap(), "Solo/1.28.1");
                Ok(Response::builder()
                    .status(status)
                    .body(body.into())
                    .unwrap())
            }
        })
    }

    #[gpui::test]
    async fn check_reports_new_version(_cx: &mut TestAppContext) {
        let http = fake_http(200, release_json("v1.29.0", false, SETUP_ASSET));
        let outcome = check_for_update(http, &version("1.28.1"), None, UpdateCheckType::Automatic)
            .await
            .unwrap();
        let CheckOutcome::UpdateAvailable(release) = outcome else {
            panic!("expected an update, got {outcome:?}");
        };
        assert_eq!(release.version, version("1.29.0"));
        assert_eq!(release.installer.unwrap().size, 1234);
    }

    #[gpui::test]
    async fn check_reports_up_to_date(_cx: &mut TestAppContext) {
        let http = fake_http(200, release_json("v1.28.1", false, SETUP_ASSET));
        let outcome = check_for_update(http, &version("1.28.1"), None, UpdateCheckType::Manual)
            .await
            .unwrap();
        assert_eq!(outcome, CheckOutcome::UpToDate);
    }

    #[gpui::test]
    async fn check_reports_failures(_cx: &mut TestAppContext) {
        let http = fake_http(403, r#"{"message": "API rate limit exceeded"}"#.into());
        let error = check_for_update(http, &version("1.28.1"), None, UpdateCheckType::Manual)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("403"), "{error:#}");

        let http = fake_http(404, String::new());
        let error = check_for_update(http, &version("1.28.1"), None, UpdateCheckType::Manual)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no release"), "{error:#}");

        let http: Arc<dyn HttpClient> =
            FakeHttpClient::create(|_| async move { Err(anyhow::anyhow!("connection refused")) });
        let error = check_for_update(http, &version("1.28.1"), None, UpdateCheckType::Manual)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("connection refused"),
            "{error:#}"
        );
    }

    #[gpui::test]
    async fn fetches_the_release_of_a_version(_cx: &mut TestAppContext) {
        assert_eq!(
            release_api_url(&version("2.1.0+stable.abc")),
            "https://api.github.com/repos/lucas9801/solo/releases/tags/v2.1.0"
        );
        assert_eq!(
            release_page_url(&version("2.1.0")),
            "https://github.com/lucas9801/solo/releases/tag/v2.1.0"
        );
        let http = FakeHttpClient::create(|request| async move {
            assert_eq!(
                request.uri().to_string(),
                "https://api.github.com/repos/lucas9801/solo/releases/tags/v2.1.0"
            );
            assert_eq!(request.headers().get("User-Agent").unwrap(), "Solo/2.1.0");
            let body = release_json("v2.1.0", false, SETUP_ASSET);
            Ok(Response::builder().status(200).body(body.into()).unwrap())
        });
        let release = fetch_release_for_version(http, &version("2.1.0"))
            .await
            .unwrap();
        assert_eq!(release.version, version("2.1.0"));
        assert_eq!(
            release.html_url,
            "https://github.com/lucas9801/solo/releases/tag/v2.1.0"
        );
    }

    #[gpui::test]
    async fn downloads_installer_and_checks_size(cx: &mut TestAppContext) {
        // Writes a real file through smol's blocking thread pool.
        cx.background_executor.allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Solo-Setup-1.29.0.exe");
        let http: Arc<dyn HttpClient> = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(200)
                .body(vec![7u8; 1000].into())
                .unwrap())
        });

        let mut installer = SoloInstaller {
            name: "Solo-Setup-1.29.0.exe".into(),
            url: "https://example.com/Solo-Setup-1.29.0.exe".into(),
            size: 1000,
            // Of 1000 sevens.
            sha256: Some(sha256_of_bytes(&[7u8; 1000])),
        };
        let mut progress = Vec::new();
        download_installer(http.clone(), &installer, &version("1.28.1"), &target, |p| {
            progress.push(p)
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap().len(), 1000);
        assert_eq!(progress.last(), Some(&100));

        // A different file than GitHub hashed is thrown away.
        installer.sha256 = Some(sha256_of_bytes(b"something else"));
        let error = download_installer(
            http.clone(),
            &installer,
            &version("1.28.1"),
            &target,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("corrupt"), "{error:#}");
        assert!(!target.exists());

        installer.size = 2000;
        let error = download_installer(
            http.clone(),
            &installer,
            &version("1.28.1"),
            &target,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error:#}");
        assert!(!target.exists());

        // No hash from GitHub: nothing to check it against, so not
        // downloaded (and never run).
        installer.size = 1000;
        installer.sha256 = None;
        let error = download_installer(http, &installer, &version("1.28.1"), &target, |_| {})
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no SHA-256"), "{error:#}");
        assert!(!target.exists());
    }

    fn sha256_of_bytes(bytes: &[u8]) -> String {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        sha256_of_file(file.path()).unwrap()
    }

    #[test]
    fn hashes_files() {
        assert_eq!(
            sha256_of_bytes(b"foo"),
            "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
        );
        assert_eq!(sha256_of_digest("sha256:ABC"), None);
        assert_eq!(
            sha256_of_digest(
                "md5:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
            ),
            None
        );
    }

    #[test]
    fn tells_how_the_last_update_went() {
        assert_eq!(startup_notice(&version("1.28.3"), None), None);
        assert_eq!(
            startup_notice(&version("1.28.4"), Some(&version("1.28.4"))),
            Some(StartupNotice::Updated(version("1.28.4")))
        );
        assert_eq!(
            startup_notice(&version("1.28.3"), Some(&version("1.28.4"))),
            Some(StartupNotice::NotInstalled(version("1.28.4")))
        );
    }

    fn button_updater(cx: &mut TestAppContext) -> (Entity<SoloUpdater>, Entity<UpdateEntry>) {
        cx.update(|cx| {
            let entry = update_button::entry_or_init(cx);
            let updater = cx.new(|_| SoloUpdater::new(version("1.28.1"), entry.clone()));
            (updater, entry)
        })
    }

    fn installable(text: &str) -> SoloRelease {
        SoloRelease {
            notes: "Solo 1.29：更快。
Solo 1.29: faster."
                .into(),
            installer: Some(SoloInstaller {
                name: format!("Solo-Setup-{text}.exe"),
                url: "https://example.com/setup.exe".into(),
                size: 1,
                sha256: None,
            }),
            ..release(text)
        }
    }

    #[gpui::test]
    async fn checks_show_on_the_button_not_in_notifications(cx: &mut TestAppContext) {
        let (updater, entry) = button_updater(cx);
        let state = |cx: &mut TestAppContext| entry.read_with(cx, |e, _| e.state().clone());

        // An automatic check finding a version: the button, nothing else
        // (the updater has no way to show a notification any more).
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(
                Ok(CheckOutcome::UpdateAvailable(installable("1.29.0"))),
                UpdateCheckType::Automatic,
                cx,
            )
        });
        assert_eq!(state(cx), ButtonState::Available);
        let offer = entry.read_with(cx, |e, _| e.offer().cloned()).unwrap();
        assert_eq!(offer.version, Some(version("1.29.0")));
        assert_eq!(offer.can_install, cfg!(target_os = "windows"));
        assert_eq!(
            update_button::offer_summary(&offer, true).as_deref(),
            Some("Solo 1.29：更快。")
        );

        // A failed automatic check keeps the offer; a failed manual one says so.
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(
                Err(anyhow::anyhow!("offline")),
                UpdateCheckType::Automatic,
                cx,
            )
        });
        assert_eq!(state(cx), ButtonState::Available);
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(Err(anyhow::anyhow!("offline")), UpdateCheckType::Manual, cx)
        });
        let ButtonState::Failed { reason } = state(cx) else {
            panic!("expected a failure");
        };
        assert!(reason.contains("offline"), "{reason}");

        // Skipped: gone, and automatic checks keep it gone.
        updater.update(cx, |updater, cx| updater.act(UpdateAction::Skip, cx));
        assert_eq!(state(cx), ButtonState::Hidden);
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(
                Ok(CheckOutcome::Skipped(version("1.29.0"))),
                UpdateCheckType::Automatic,
                cx,
            )
        });
        assert_eq!(state(cx), ButtonState::Hidden);
        // A newer version brings it back.
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(
                Ok(CheckOutcome::UpdateAvailable(installable("1.29.1"))),
                UpdateCheckType::Automatic,
                cx,
            )
        });
        assert_eq!(state(cx), ButtonState::Available);

        // Up to date: said for a few seconds after a manual check only.
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(Ok(CheckOutcome::UpToDate), UpdateCheckType::Manual, cx)
        });
        assert_eq!(state(cx), ButtonState::UpToDate);
        cx.executor()
            .advance_clock(update_button::UP_TO_DATE_DURATION * 2);
        assert_eq!(state(cx), ButtonState::Hidden);
        updater.update(cx, |updater, cx| {
            updater.handle_outcome(Ok(CheckOutcome::UpToDate), UpdateCheckType::Automatic, cx)
        });
        assert_eq!(state(cx), ButtonState::Hidden);
    }

    #[gpui::test]
    async fn install_results_show_on_the_button(cx: &mut TestAppContext) {
        let (updater, entry) = button_updater(cx);
        let state = |cx: &mut TestAppContext| entry.read_with(cx, |e, _| e.state().clone());
        let finish = |result: Result<InstallResult>, quiet: bool, cx: &mut TestAppContext| {
            updater.update(cx, |updater, cx| {
                updater.finish_install(
                    result,
                    installable("1.29.0"),
                    InstallWhen::OnQuit,
                    quiet,
                    cx,
                )
            })
        };
        finish(Ok(InstallResult::OnQuit), false, cx);
        assert_eq!(state(cx), ButtonState::OnQuit);
        finish(Ok(InstallResult::QuitCancelled), false, cx);
        assert_eq!(state(cx), ButtonState::Available);
        finish(Err(anyhow::anyhow!("disk full")), false, cx);
        assert_eq!(
            state(cx),
            ButtonState::Failed {
                reason: "disk full".into()
            }
        );
        assert_eq!(
            updater.read_with(cx, |updater, _| updater.retry),
            Some(Retry::Install(InstallWhen::OnQuit))
        );
        // Replacing an update waiting for Solo to quit failed: that one stays.
        finish(Err(anyhow::anyhow!("disk full")), true, cx);
        assert_eq!(state(cx), ButtonState::OnQuit);
    }

    #[test]
    fn helper_args_say_whether_to_start_solo_again() {
        let path = Path::new("x");
        assert_eq!(installer_helper_args(1, path, path, path, true).len(), 4);
        assert_eq!(
            installer_helper_args(1, path, path, path, false).last(),
            Some(&std::ffi::OsString::from("--no-launch"))
        );
    }
}
