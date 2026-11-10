//! SOLO: Solo updates itself from its GitHub Releases instead of Zed's servers.
//!
//! Zed's own [`crate::AutoUpdater`] never polls (see
//! `ReleaseChannel::poll_for_updates`), so it never talks to zed.dev. This
//! module is a separate, much smaller updater:
//!
//! - it asks the GitHub API for the latest release of [`SOLO_GITHUB_REPO`],
//! - tells the user about a newer version with a notification
//!   ("update and restart", "skip this version", "view release notes"),
//! - and on Windows downloads the Inno Setup installer (checked against the
//!   SHA-256 GitHub reports) and starts `solo-update-helper`, which waits for
//!   Solo to exit and runs the installer silently. "Update and restart" then
//!   quits Solo through the normal quit flow (so unsaved files still prompt)
//!   and the installer starts it again; "install when Solo quits" waits for
//!   the user to quit, so agents running in terminals carry on meanwhile.
//!
//! Only stable builds check. Automatic checks follow the `auto_update`
//! setting; the "check for updates" command always checks.

use crate::{AutoUpdateSetting, UpdateCheckType};
use anyhow::{Context as _, Result};
use db::kvp::KeyValueStore;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, Global, IntoElement as _, Task, Window,
};
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
        NotificationId, dismiss_app_notification, show_app_notification,
        simple_message_notification::MessageNotification,
    },
    ui::Label,
};

/// The GitHub repository Solo's releases are published in.
pub const SOLO_GITHUB_REPO: &str = "lucas9801/solo";

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/lucas9801/solo/releases/latest";
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
    lower.starts_with(&INSTALLER_PREFIX.to_ascii_lowercase()) && lower.ends_with(INSTALLER_SUFFIX)
}

/// Parses the JSON returned by GitHub's "latest release" endpoint.
pub fn parse_release(json: &[u8]) -> Result<SoloRelease> {
    let release: GithubReleaseJson =
        serde_json::from_slice(json).context("unexpected response from GitHub")?;
    let version = parse_version(&release.tag_name)
        .with_context(|| format!("release tag {:?} is not a version", release.tag_name))?;
    let installer = release
        .assets
        .into_iter()
        .find(|asset| is_installer_asset(&asset.name))
        .map(|asset| SoloInstaller {
            name: asset.name,
            url: asset.browser_download_url,
            size: asset.size,
            sha256: asset.digest.as_deref().and_then(sha256_of_digest),
        });
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

fn user_agent(current: &Version) -> String {
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
    parse_release(&body)
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
    mut on_progress: impl FnMut(u8),
) -> Result<()> {
    let request = Request::get(installer.url.as_str())
        .header("Accept", "application/octet-stream")
        .header("User-Agent", user_agent(current))
        .follow_redirects(RedirectPolicy::FollowAll)
        .timeout(DOWNLOAD_TIMEOUT)
        .body(AsyncBody::default())?;
    let mut response = http
        .send(request)
        .await
        .context("couldn't download the installer")?;
    anyhow::ensure!(
        response.status().is_success(),
        "downloading the installer failed: {}",
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
            let percent = ((written.min(total) * 100) / total) as u8;
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
            "the downloaded installer is incomplete ({written} of {} bytes)",
            installer.size
        );
    }
    match &installer.sha256 {
        Some(expected) => {
            let path = target.to_path_buf();
            let actual = smol::unblock(move || sha256_of_file(&path)).await?;
            if &actual != expected {
                smol::fs::remove_file(target).await.ok();
                anyhow::bail!(
                    "the downloaded installer is corrupt: its SHA-256 is {actual}, not {expected}"
                );
            }
        }
        None => log::warn!(
            "solo update: GitHub reported no SHA-256 for {}",
            installer.name
        ),
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

/// The most characters of the release notes' summary the notification shows.
const SUMMARY_MAX_CHARS: usize = 200;

/// One sentence of the release notes for the notification, in the UI's
/// language, as plain text.
///
/// Solo's release notes start with a summary in Chinese on the first line and
/// in English on the second, each one plain sentence (checked by
/// `script/release-solo.ps1`). Notes that don't (older releases') give the
/// plain text of their first paragraph that isn't only headings.
fn notes_summary(notes: &str, chinese: bool) -> Option<String> {
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
    poll_task: Option<Task<()>>,
    _quit_subscription: gpui::Subscription,
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

struct UpdateAvailableNotification;
struct UpdateStatusNotification;
struct DownloadNotification;

/// Starts the GitHub Releases updater (stable builds only).
pub fn init(cx: &mut App) {
    if ReleaseChannel::try_global(cx) != Some(ReleaseChannel::Stable) {
        return;
    }
    let current_version = release_channel::AppVersion::global(cx);
    let updater = cx.new(|cx| {
        let mut updater = SoloUpdater {
            current_version,
            activity: Activity::Idle,
            ready_on_quit: None,
            checked: UpdateStatus::NotChecked,
            poll_task: None,
            _quit_subscription: cx.on_app_quit(|updater: &mut SoloUpdater, _| {
                updater.install_ready_update_at_quit();
                async {}
            }),
        };
        updater.sync_polling(cx);
        cx.observe_global::<SettingsStore>(|updater, cx| updater.sync_polling(cx))
            .detach();
        updater
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

/// The "check for updates" command: always checks, and always reports back.
pub fn check_manually(window: &mut Window, cx: &mut App) {
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

impl SoloUpdater {
    /// What's known about updates: whether a check runs, or what the last
    /// one found.
    pub fn status(&self) -> UpdateStatus {
        match self.activity {
            Activity::Checking => UpdateStatus::Checking,
            _ => self.checked.clone(),
        }
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

    fn check(&mut self, check_type: UpdateCheckType, cx: &mut Context<Self>) {
        match &self.activity {
            Activity::Idle => {}
            Activity::Checking => return,
            Activity::Downloading { version, .. } => {
                if check_type.is_manual() {
                    show_status_message(
                        if util::i18n::is_chinese() {
                            format!("正在下载 Solo {version}…")
                        } else {
                            format!("Downloading Solo {version}…")
                        },
                        cx,
                    );
                }
                return;
            }
            Activity::Installing => return,
        }
        self.activity = Activity::Checking;
        cx.notify();
        if check_type.is_manual() {
            show_status_message(
                util::i18n::tr("Checking for updates…", "正在检查更新…").to_string(),
                cx,
            );
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

    fn handle_outcome(
        &mut self,
        outcome: Result<CheckOutcome>,
        check_type: UpdateCheckType,
        cx: &mut Context<Self>,
    ) {
        self.checked = update_status(&outcome);
        match outcome {
            Ok(CheckOutcome::UpdateAvailable(release)) if self.ready_on_quit.is_some() => {
                let ready = self
                    .ready_on_quit
                    .as_ref()
                    .map(|ready| ready.version.clone());
                if ready.as_ref() == Some(&release.version) {
                    if check_type.is_manual() {
                        show_status_message(on_quit_message(&release.version), cx);
                    }
                } else {
                    // The user chose to update when Solo quits: they get the
                    // newest version then, without being asked again.
                    log::info!(
                        "solo update: {} replaces the update waiting for Solo to quit",
                        release.version
                    );
                    self.install(release, InstallWhen::OnQuit, true, cx);
                }
            }
            Ok(CheckOutcome::UpdateAvailable(release)) => {
                log::info!("solo update: {} is available", release.version);
                if check_type.is_manual() {
                    dismiss_app_notification(
                        &NotificationId::unique::<UpdateStatusNotification>(),
                        cx,
                    );
                }
                show_update_available(release, cx);
            }
            Ok(CheckOutcome::UpToDate) => {
                log::info!("solo update: up to date ({})", self.current_version);
                if check_type.is_manual() {
                    let version = &self.current_version;
                    show_status_message(
                        if util::i18n::is_chinese() {
                            format!("已是最新版本 {version}")
                        } else {
                            format!("Solo is up to date ({version})")
                        },
                        cx,
                    );
                }
            }
            Ok(CheckOutcome::Skipped(version)) => {
                log::info!("solo update: {version} is available but was skipped");
            }
            Err(error) => {
                log::warn!("solo update: check failed: {error:#}");
                if check_type.is_manual() {
                    show_status_message(
                        if util::i18n::is_chinese() {
                            format!("检查更新失败：{error:#}")
                        } else {
                            format!("Couldn't check for updates: {error:#}")
                        },
                        cx,
                    );
                }
            }
        }
    }

    fn progress_text(&self) -> String {
        match &self.activity {
            Activity::Downloading {
                version, percent, ..
            } => {
                let percent = percent.map(|p| format!(" {p}%")).unwrap_or_default();
                if util::i18n::is_chinese() {
                    format!("正在下载 Solo {version}…{percent}")
                } else {
                    format!("Downloading Solo {version}…{percent}")
                }
            }
            Activity::Installing => util::i18n::tr(
                "Closing Solo to install the update…",
                "正在关闭 Solo 以安装更新…",
            )
            .to_string(),
            _ => String::new(),
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
        let Some(installer) = release.installer else {
            cx.open_url(&release.html_url);
            return;
        };
        let version = release.version;
        // Already downloaded, to install when Solo quits.
        let downloaded = self
            .ready_on_quit
            .clone()
            .filter(|ready| ready.version == version && ready.installer.is_file());
        if let Some(ready) = &downloaded
            && when == InstallWhen::OnQuit
        {
            show_status_message(on_quit_message(&ready.version), cx);
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
            show_download_notification(cx.entity(), cx);
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
                        Ok(InstallResult::OnQuit(ready.version))
                    }
                }
            }
            .await;
            cx.update(|cx| {
                dismiss_app_notification(&NotificationId::unique::<DownloadNotification>(), cx)
            });
            let message = match result {
                Ok(InstallResult::Quitting) => return,
                Ok(InstallResult::OnQuit(version)) => on_quit_message(&version),
                Ok(InstallResult::QuitCancelled) => util::i18n::tr(
                    "Update cancelled because Solo didn't quit.",
                    "Solo 没有退出，更新已取消。",
                )
                .to_string(),
                Err(error) => {
                    log::error!("solo update: install failed: {error:#}");
                    if util::i18n::is_chinese() {
                        format!("更新失败：{error:#}")
                    } else {
                        format!("Update failed: {error:#}")
                    }
                }
            };
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                cx.notify();
                if !quiet {
                    show_status_message(message, cx);
                }
            })
            .ok();
        })
        .detach();
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

fn on_quit_message(version: &Version) -> String {
    if util::i18n::is_chinese() {
        format!("Solo {version} 会在你退出 Solo 时安装。")
    } else {
        format!("Solo {version} will be installed when you quit Solo.")
    }
}

enum InstallResult {
    Quitting,
    QuitCancelled,
    OnQuit(Version),
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
            if let Activity::Downloading { percent: p, .. } = &mut this.activity {
                *p = Some(percent);
                cx.notify();
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

fn show_download_notification(updater: Entity<SoloUpdater>, cx: &mut App) {
    show_app_notification(
        NotificationId::unique::<DownloadNotification>(),
        cx,
        move |cx| {
            let updater = updater.clone();
            cx.new(|cx| {
                cx.observe(&updater, |_, _, cx| cx.notify()).detach();
                let updater = updater.downgrade();
                MessageNotification::new_from_builder(cx, move |_, cx| {
                    let text = updater
                        .upgrade()
                        .map(|updater| updater.read(cx).progress_text())
                        .unwrap_or_default();
                    Label::new(text).into_any_element()
                })
                .show_suppress_button(false)
            })
        },
    );
}

fn show_update_available(release: SoloRelease, cx: &mut App) {
    show_app_notification(
        NotificationId::unique::<UpdateAvailableNotification>(),
        cx,
        move |cx| {
            let release = release.clone();
            cx.new(|cx| {
                let version = release.version.clone();
                let title = if util::i18n::is_chinese() {
                    format!("Solo {version} 可用")
                } else {
                    format!("Solo {version} is available")
                };
                let excerpt = notes_summary(&release.notes, util::i18n::is_chinese());
                let mut notification = MessageNotification::new_from_builder(cx, move |_, _| {
                    Label::new(title.clone()).into_any_element()
                })
                .show_suppress_button(false);
                if let Some(excerpt) = excerpt {
                    notification = notification.secondary_content(excerpt);
                }
                let skip = {
                    move |_: &mut Window, cx: &mut Context<MessageNotification>| {
                        let kvp = KeyValueStore::global(cx);
                        if let Some(global) = cx.try_global::<GlobalSoloUpdater>() {
                            let updater = global.0.clone();
                            updater.update(cx, |updater, _| updater.forget_skipped(&version));
                        }
                        let version = version.to_string();
                        cx.background_spawn(async move {
                            kvp.write_kvp(SKIPPED_VERSION_KEY.to_string(), version)
                                .await
                                .log_err();
                        })
                        .detach();
                    }
                };
                if !(can_install() && release.installer.is_some()) {
                    return notification
                        .secondary_message(util::i18n::tr("Skip This Version", "跳过此版本"))
                        .secondary_on_click(skip)
                        .more_info_message(util::i18n::tr("View Release Notes", "查看更新内容"))
                        .more_info_url(release.html_url.clone());
                }
                let install = |when: InstallWhen| {
                    let release = release.clone();
                    move |_: &mut Window, cx: &mut Context<MessageNotification>| {
                        if let Some(global) = cx.try_global::<GlobalSoloUpdater>() {
                            let updater = global.0.clone();
                            let release = release.clone();
                            updater.update(cx, |updater, cx| {
                                updater.install(release, when, false, cx)
                            });
                        }
                    }
                };
                let restart = util::i18n::tr("Update and Restart", "更新并重启");
                let on_quit = util::i18n::tr("Install When Solo Quits", "退出时安装");
                // With agents running, updating now would end them: installing
                // when the user quits comes first.
                let running = running_programs(cx);
                let notification = if running.is_empty() {
                    notification
                        .primary_message(restart)
                        .primary_on_click(install(InstallWhen::Restart))
                        .secondary_message(on_quit)
                        .secondary_on_click(install(InstallWhen::OnQuit))
                } else {
                    let names = running
                        .iter()
                        .map(|program| program.name.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let warning = if util::i18n::is_chinese() {
                        format!("现在更新并重启会结束：{names}")
                    } else {
                        format!("Updating and restarting now ends: {names}")
                    };
                    let notes = notes_summary(&release.notes, util::i18n::is_chinese())
                        .map(|excerpt| format!("{warning}\n\n{excerpt}"))
                        .unwrap_or(warning);
                    notification
                        .secondary_content(notes)
                        .primary_message(on_quit)
                        .primary_on_click(install(InstallWhen::OnQuit))
                        .secondary_message(restart)
                        .secondary_on_click(install(InstallWhen::Restart))
                };
                notification
                    .secondary_menu_entry(util::i18n::tr("Skip This Version", "跳过此版本"), skip)
                    .more_info_message(util::i18n::tr("View Release Notes", "查看更新内容"))
                    .more_info_url(release.html_url.clone())
            })
        },
    );
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
        let error = download_installer(http, &installer, &version("1.28.1"), &target, |_| {})
            .await
            .unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error:#}");
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
