//! SOLO: Solo updates itself from its GitHub Releases instead of Zed's servers.
//!
//! Zed's own [`crate::AutoUpdater`] never polls (see
//! `ReleaseChannel::poll_for_updates`), so it never talks to zed.dev. This
//! module is a separate, much smaller updater:
//!
//! - it asks the GitHub API for the latest release of [`SOLO_GITHUB_REPO`],
//! - tells the user about a newer version with a notification
//!   ("update and restart", "skip this version", "view release notes"),
//! - and on Windows downloads the Inno Setup installer, starts a hidden helper
//!   that waits for Solo to exit and runs the installer silently, then quits
//!   Solo through the normal quit flow (so unsaved files still prompt).
//!
//! Only stable builds check. Automatic checks follow the `auto_update`
//! setting; the "check for updates" command always checks.

use crate::{AutoUpdateSetting, UpdateCheckType};
use anyhow::{Context as _, Result};
use base64::Engine as _;
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
use std::{path::Path, sync::Arc, time::Duration};
use util::ResultExt as _;
use workspace::{
    MultiWorkspace,
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
    let request = Request::get(LATEST_RELEASE_URL)
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
/// checks the size against the one GitHub reported.
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
    Ok(())
}

/// Quotes `value` as a PowerShell single-quoted string. PowerShell also treats
/// the typographic single quotes as quote characters, so those are doubled too.
pub fn powershell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(ch);
        }
        quoted.push(ch);
    }
    quoted.push('\'');
    quoted
}

/// The PowerShell script the helper runs: wait for Solo (`pid`) to exit, then
/// run the installer silently into `install_dir`.
pub fn installer_helper_script(
    pid: u32,
    installer: &Path,
    install_dir: &Path,
    log_file: &Path,
) -> String {
    // `Start-Process -ArgumentList` joins its items with spaces without quoting
    // them, so values with spaces carry their own double quotes (Windows paths
    // can't contain `"`). Inno Setup reads `/DIR="..."` and `/LOG="..."`.
    let setup_args = [
        "/SILENT".to_string(),
        "/SUPPRESSMSGBOXES".to_string(),
        "/NORESTART".to_string(),
        format!("/DIR=\"{}\"", install_dir.display()),
        format!("/LOG=\"{}\"", log_file.display()),
    ]
    .iter()
    .map(|arg| powershell_quote(arg))
    .collect::<Vec<_>>()
    .join(", ");
    format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue\n\
         Start-Process -FilePath {} -ArgumentList @({setup_args})\n",
        powershell_quote(&installer.display().to_string()),
    )
}

/// Encodes a script for `powershell -EncodedCommand` (base64 of UTF-16LE).
/// This keeps spaces, quotes and non-ASCII paths out of command-line parsing.
pub fn encode_powershell_command(script: &str) -> String {
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The arguments for `powershell.exe` that run [`installer_helper_script`].
pub fn installer_helper_args(
    pid: u32,
    installer: &Path,
    install_dir: &Path,
    log_file: &Path,
) -> Vec<String> {
    vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-WindowStyle".into(),
        "Hidden".into(),
        "-EncodedCommand".into(),
        encode_powershell_command(&installer_helper_script(
            pid,
            installer,
            install_dir,
            log_file,
        )),
    ]
}

#[cfg(target_os = "windows")]
fn spawn_installer_helper(installer: &Path, install_dir: &Path, log_file: &Path) -> Result<()> {
    use smol::process::windows::CommandExt as _;
    use std::{path::PathBuf, process::Stdio};

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let powershell = std::env::var_os("SystemRoot")
        .map(|root| {
            PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let args = installer_helper_args(std::process::id(), installer, install_dir, log_file);
    let spawn = |flags: u32| {
        smol::process::Command::new(&powershell)
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
fn spawn_installer_helper(_installer: &Path, _install_dir: &Path, _log_file: &Path) -> Result<()> {
    anyhow::bail!("installing updates is only supported on Windows")
}

/// Whether this platform can install updates (Windows installer only).
fn can_install() -> bool {
    cfg!(target_os = "windows")
}

/// A few lines of the release notes for the notification.
fn notes_excerpt(notes: &str) -> Option<String> {
    let mut excerpt = String::new();
    for line in notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
    {
        if !excerpt.is_empty() {
            excerpt.push('\n');
        }
        excerpt.push_str(line);
    }
    if excerpt.is_empty() {
        return None;
    }
    if excerpt.chars().count() > 300 {
        excerpt = excerpt.chars().take(300).collect::<String>() + "…";
    }
    Some(excerpt)
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

#[derive(Clone, Debug, PartialEq)]
enum Activity {
    Idle,
    Checking,
    Downloading {
        version: Version,
        percent: Option<u8>,
    },
    Installing,
}

pub struct SoloUpdater {
    current_version: Version,
    activity: Activity,
    poll_task: Option<Task<()>>,
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
            poll_task: None,
        };
        updater.sync_polling(cx);
        cx.observe_global::<SettingsStore>(|updater, cx| updater.sync_polling(cx))
            .detach();
        updater
    });
    cx.set_global(GlobalSoloUpdater(updater));
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
                &[util::i18n::tr("OK", "好")],
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
        match outcome {
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
            Activity::Downloading { version, percent } => {
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

    fn install(&mut self, release: SoloRelease, cx: &mut Context<Self>) {
        if !matches!(self.activity, Activity::Idle) {
            return;
        }
        let Some(installer) = release.installer else {
            cx.open_url(&release.html_url);
            return;
        };
        self.activity = Activity::Downloading {
            version: release.version,
            percent: None,
        };
        cx.notify();
        show_download_notification(cx.entity(), cx);

        let http = cx.http_client();
        let current = self.current_version.clone();
        cx.spawn(async move |this, cx| {
            let result = download_and_install(this.clone(), http, installer, current, cx).await;
            cx.update(|cx| {
                dismiss_app_notification(&NotificationId::unique::<DownloadNotification>(), cx)
            });
            let message = match result {
                Ok(InstallResult::Quitting) => return,
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
                show_status_message(message, cx);
            })
            .ok();
        })
        .detach();
    }
}

enum InstallResult {
    Quitting,
    QuitCancelled,
}

async fn download_and_install(
    this: gpui::WeakEntity<SoloUpdater>,
    http: Arc<dyn HttpClient>,
    installer: SoloInstaller,
    current: Version,
    cx: &mut AsyncApp,
) -> Result<InstallResult> {
    let install_dir = std::env::current_exe()
        .context("couldn't locate Solo")?
        .parent()
        .context("couldn't locate Solo's folder")?
        .to_path_buf();
    let download_dir = std::env::temp_dir().join("solo-update");
    if smol::fs::metadata(&download_dir).await.is_ok() {
        smol::fs::remove_dir_all(&download_dir).await.ok();
    }
    smol::fs::create_dir_all(&download_dir)
        .await
        .with_context(|| format!("couldn't create {}", download_dir.display()))?;
    let target = download_dir.join(&installer.name);
    let log_file = download_dir.join("install.log");

    let progress_cx = cx.clone();
    let progress_this = this.clone();
    download_installer(http, &installer, &current, &target, move |percent| {
        progress_this
            .update(&mut progress_cx.clone(), |this, cx| {
                if let Activity::Downloading { percent: p, .. } = &mut this.activity {
                    *p = Some(percent);
                    cx.notify();
                }
            })
            .ok();
    })
    .await?;
    log::info!("solo update: downloaded {}", target.display());

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

    spawn_installer_helper(&target, &install_dir, &log_file)?;
    log::info!(
        "solo update: helper started; installing into {}",
        install_dir.display()
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
                let excerpt = notes_excerpt(&release.notes);
                let mut notification = MessageNotification::new_from_builder(cx, move |_, _| {
                    Label::new(title.clone()).into_any_element()
                })
                .show_suppress_button(false);
                if let Some(excerpt) = excerpt {
                    notification = notification.secondary_content(excerpt);
                }
                if can_install() && release.installer.is_some() {
                    let release = release.clone();
                    notification = notification
                        .primary_message(util::i18n::tr("Update and Restart", "更新并重启"))
                        .primary_on_click(move |_, cx| {
                            if let Some(global) = cx.try_global::<GlobalSoloUpdater>() {
                                let updater = global.0.clone();
                                let release = release.clone();
                                updater.update(cx, |updater, cx| updater.install(release, cx));
                            }
                        });
                }
                notification
                    .secondary_message(util::i18n::tr("Skip This Version", "跳过此版本"))
                    .secondary_on_click(move |_, cx| {
                        let kvp = KeyValueStore::global(cx);
                        let version = version.to_string();
                        cx.background_spawn(async move {
                            kvp.write_kvp(SKIPPED_VERSION_KEY.to_string(), version)
                                .await
                                .log_err();
                        })
                        .detach();
                    })
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
        {"name": "Solo-Setup-1.29.0.exe", "size": 1234, "browser_download_url": "https://example.com/Solo-Setup-1.29.0.exe"}
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
    fn quotes_powershell_strings() {
        assert_eq!(powershell_quote(r"C:\Solo"), r"'C:\Solo'");
        assert_eq!(powershell_quote("it's"), "'it''s'");
        assert_eq!(powershell_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(powershell_quote("$env:TEMP `x`"), "'$env:TEMP `x`'");
    }

    fn decode_powershell_command(encoded: &str) -> String {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn builds_helper_command_for_paths_with_spaces_and_chinese() {
        let installer =
            Path::new(r"C:\Users\张三\AppData\Local\Temp\solo-update\Solo-Setup-1.29.0.exe");
        let install_dir = Path::new(r"D:\Program Files\终端 Solo's");
        let log_file = Path::new(r"C:\Users\张三\AppData\Local\Temp\solo-update\install.log");
        let script = installer_helper_script(4242, installer, install_dir, log_file);
        assert_eq!(
            script,
            "Wait-Process -Id 4242 -ErrorAction SilentlyContinue\n\
             Start-Process -FilePath 'C:\\Users\\张三\\AppData\\Local\\Temp\\solo-update\\Solo-Setup-1.29.0.exe' \
             -ArgumentList @('/SILENT', '/SUPPRESSMSGBOXES', '/NORESTART', \
             '/DIR=\"D:\\Program Files\\终端 Solo''s\"', \
             '/LOG=\"C:\\Users\\张三\\AppData\\Local\\Temp\\solo-update\\install.log\"')\n"
        );

        let args = installer_helper_args(4242, installer, install_dir, log_file);
        assert_eq!(
            &args[..7],
            &[
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-EncodedCommand"
            ]
        );
        assert_eq!(args.len(), 8);
        // The encoded command is plain base64, so nothing needs command-line quoting.
        assert!(
            args[7]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        );
        assert_eq!(decode_powershell_command(&args[7]), script);
    }

    #[test]
    fn excerpts_release_notes() {
        assert_eq!(notes_excerpt("\n\n  "), None);
        assert_eq!(
            notes_excerpt("## New\n\n- a\n- b\n- c\n- d"),
            Some("## New\n- a\n- b\n- c".to_string())
        );
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
        };
        let mut progress = Vec::new();
        download_installer(http.clone(), &installer, &version("1.28.1"), &target, |p| {
            progress.push(p)
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap().len(), 1000);
        assert_eq!(progress.last(), Some(&100));

        installer.size = 2000;
        let error = download_installer(http, &installer, &version("1.28.1"), &target, |_| {})
            .await
            .unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error:#}");
        assert!(!target.exists());
    }
}
