//! SOLO: the Microsoft Store edition's updates, found, downloaded and
//! installed from inside Solo through `Windows.Services.Store.StoreContext`,
//! without opening the Store.
//!
//! - Checks follow Solo's own updater: 30 seconds after Solo starts, then
//!   every 6 hours, unless `auto_update` is off; "check for updates" always
//!   checks.
//! - When the Store allows it (the user lets the Store update apps on its
//!   own, and the network isn't metered), a found update is downloaded in
//!   the background, silently.
//! - The update button ([`crate::update_button`]) offers it like Solo's own
//!   updater does. "Update and restart" lets the Store install it, which
//!   closes Solo; Windows starts Solo again because it registered for a
//!   restart after updates (`util::packaged::register_restart_after_update`).
//!   When the update can't be installed silently, the Store shows its own
//!   small dialog over Solo's window instead. "Install when Solo quits"
//!   starts a silent install as Solo quits, without the restart.
//! - Nothing here may crash Solo or show an error box: Store failures (no
//!   network, the Store service unavailable) are logged and shown on the
//!   button only when the user asked; automatic checks try again later.
//!
//! The Store is reached through [`StoreUpdates`], so the flow can be tested
//! with a fake.

use crate::update_button::{self, ButtonState, UpdateAction, UpdateEntry, UpdateOffer};
use crate::{AutoUpdateSetting, UpdateCheckType};
use anyhow::{Result, anyhow};
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Context, Entity, Global, Task, Window};
use semver::Version;
use settings::{Settings as _, SettingsStore};
use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};
use util::ResultExt as _;

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const SKIPPED_VERSION_KEY: &str = "solo-store-update-skipped-version";

/// An update the Store has for Solo.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreUpdate {
    /// The version of Solo it installs, when the Store says.
    pub version: Option<Version>,
}

/// How a Store download or install ended.
#[derive(Clone, Debug, PartialEq)]
pub enum StoreOutcome {
    Completed,
    /// The user cancelled the Store's dialog.
    Canceled,
    Failed(String),
}

/// Whole percentages of a download or install.
pub type ProgressFn = Box<dyn Fn(u8) + Send>;

/// The Microsoft Store, as far as updating Solo goes. The calls block: they
/// run on background threads.
pub trait StoreUpdates: Send + Sync + 'static {
    /// Asks the Store whether it has an update for Solo.
    fn find_update(&self) -> Result<Option<StoreUpdate>>;
    /// Whether the update can be downloaded (and installed) without the
    /// Store asking the user.
    fn can_silently_download(&self) -> Result<bool>;
    /// Downloads the update found last, silently, without installing it.
    fn download(&self, progress: ProgressFn) -> Result<StoreOutcome>;
    /// Downloads (unless done) and installs the update found last, which
    /// closes Solo. Not `silent`, the Store shows a dialog over `window`.
    fn install(
        &self,
        silent: bool,
        window: Option<isize>,
        progress: ProgressFn,
    ) -> Result<StoreOutcome>;
    /// Starts installing the update found last silently, without waiting:
    /// Solo is quitting.
    fn start_install_at_quit(&self) -> Result<()>;
}

/// Runs a Store call, turning a panic into an error: a Store failure must
/// never take Solo down.
fn guarded<T>(call: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err(anyhow!("the Microsoft Store call panicked")))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Activity {
    Idle,
    Checking,
    /// Downloading in the background, silently.
    Downloading,
    /// Installing at the user's request (downloading first if needed).
    Installing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Retry {
    Check,
    Install,
}

pub struct StoreUpdater {
    client: Arc<dyn StoreUpdates>,
    activity: Activity,
    /// The update the button offers.
    update: Option<StoreUpdate>,
    /// Whether it was downloaded in the background.
    downloaded: bool,
    /// Whether the user chose to install it when Solo quits.
    on_quit: bool,
    /// Whether the Store installs updates without asking, which installing
    /// when Solo quits needs (known once a background download was tried).
    silent: bool,
    retry: Option<Retry>,
    /// The version the user skipped.
    skipped: Option<Version>,
    /// The window to show the Store's dialog over.
    window: Option<isize>,
    entry: Entity<UpdateEntry>,
    poll_task: Option<Task<()>>,
    _quit_subscription: Option<gpui::Subscription>,
}

struct GlobalStoreUpdater(Entity<StoreUpdater>);

impl Global for GlobalStoreUpdater {}

/// The Store edition's updater, if this Solo is the Store's.
pub fn updater(cx: &App) -> Option<Entity<StoreUpdater>> {
    Some(cx.try_global::<GlobalStoreUpdater>()?.0.clone())
}

/// Starts the Store updater when Solo runs as the Store's package.
pub fn init(cx: &mut App) {
    if !crate::solo_update::is_store_package() {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let client: Arc<dyn StoreUpdates> = Arc::new(windows_store::WindowsStore::default());
        start(client, cx);
    }
}

/// Starts the updater with `client`.
pub fn start(client: Arc<dyn StoreUpdates>, cx: &mut App) -> Entity<StoreUpdater> {
    let entry = update_button::entry_or_init(cx);
    let updater = cx.new(|cx| {
        let mut updater = StoreUpdater::new(client, entry.clone());
        updater._quit_subscription = Some(cx.on_app_quit(|updater: &mut StoreUpdater, _| {
            updater.install_at_quit();
            async {}
        }));
        updater.sync_polling(cx);
        cx.observe_global::<SettingsStore>(|updater, cx| updater.sync_polling(cx))
            .detach();
        updater
    });
    let weak = updater.downgrade();
    entry.update(cx, |entry, _| {
        entry.set_handler(move |action, window, cx| {
            if let Some(updater) = weak.upgrade() {
                let hwnd = window_handle(window);
                updater.update(cx, |updater, cx| {
                    if hwnd.is_some() {
                        updater.window = hwnd;
                    }
                    updater.act(action, cx)
                });
            }
        })
    });
    if let Some(kvp) = key_value_store(cx) {
        let weak = updater.downgrade();
        cx.spawn(async move |cx| {
            let skipped = cx
                .background_spawn(async move {
                    kvp.read_kvp(SKIPPED_VERSION_KEY)
                        .ok()
                        .flatten()
                        .and_then(|version| crate::solo_update::parse_version(&version))
                })
                .await;
            weak.update(cx, |updater, _| updater.skipped = skipped).ok();
        })
        .detach();
    }
    cx.set_global(GlobalStoreUpdater(updater.clone()));
    updater
}

/// Solo's key-value store, when its database is open (not in tests).
pub(crate) fn key_value_store(cx: &App) -> Option<KeyValueStore> {
    cx.has_global::<db::AppDatabase>()
        .then(|| KeyValueStore::global(cx))
}

/// The "check for updates" command in the Store edition: checks, and says
/// what it found on the update button.
pub fn check_manually(cx: &mut App) {
    match updater(cx) {
        Some(updater) => {
            updater.update(cx, |updater, cx| updater.check(UpdateCheckType::Manual, cx))
        }
        None => log::warn!("store update: no Store updater runs"),
    }
}

/// The handle of `window` (an `HWND`), for the Store's dialog.
fn window_handle(window: &Window) -> Option<isize> {
    use raw_window_handle::RawWindowHandle;
    match raw_window_handle::HasWindowHandle::window_handle(window)
        .ok()?
        .as_raw()
    {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

impl StoreUpdater {
    fn new(client: Arc<dyn StoreUpdates>, entry: Entity<UpdateEntry>) -> Self {
        Self {
            client,
            activity: Activity::Idle,
            update: None,
            downloaded: false,
            on_quit: false,
            silent: false,
            retry: None,
            skipped: None,
            window: None,
            entry,
            poll_task: None,
            _quit_subscription: None,
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

    fn show(&self, state: ButtonState, cx: &mut Context<Self>) {
        self.entry
            .update(cx, |entry, cx| entry.set_state(state, cx));
    }

    /// What the button shows for the update found, when nothing runs.
    fn resting_state(&self) -> ButtonState {
        match (&self.update, self.on_quit) {
            (None, _) => ButtonState::Hidden,
            (Some(_), true) => ButtonState::OnQuit,
            (Some(_), false) => ButtonState::Available,
        }
    }

    fn rest(&self, cx: &mut Context<Self>) {
        self.show(self.resting_state(), cx);
    }

    fn offer(&self, update: &StoreUpdate) -> UpdateOffer {
        UpdateOffer {
            version: update.version.clone(),
            notes: None,
            notes_url: update
                .version
                .as_ref()
                .map(crate::solo_update::release_page_url),
            can_install: true,
            can_install_on_quit: self.silent,
        }
    }

    fn check(&mut self, check_type: UpdateCheckType, cx: &mut Context<Self>) {
        // SOLO: no automatic checks in offline mode.
        if self.activity != Activity::Idle || (net_guard::is_offline() && !check_type.is_manual()) {
            return;
        }
        self.activity = Activity::Checking;
        if check_type.is_manual() {
            self.show(ButtonState::Checking, cx);
        }
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { guarded(|| client.find_update()) })
                .await;
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.handle_check(found, check_type, cx);
            })
            .ok();
        })
        .detach();
    }

    fn handle_check(
        &mut self,
        found: Result<Option<StoreUpdate>>,
        check_type: UpdateCheckType,
        cx: &mut Context<Self>,
    ) {
        match found {
            Err(error) => {
                log::warn!("store update: check failed: {error:#}");
                if check_type.is_manual() {
                    self.retry = Some(Retry::Check);
                    let reason = format!(
                        "{}{error:#}",
                        util::i18n::tr("Couldn't check for updates: ", "检查更新失败：")
                    );
                    self.show(ButtonState::Failed { reason }, cx);
                } else {
                    // Tried again at the next check.
                    self.rest(cx);
                }
            }
            Ok(None) => {
                log::info!("store update: up to date");
                self.update = None;
                self.downloaded = false;
                self.on_quit = false;
                self.entry.update(cx, |entry, cx| {
                    entry.set_offer(None, cx);
                    if check_type.is_manual() {
                        entry.show_up_to_date(cx);
                    } else {
                        entry.set_state(ButtonState::Hidden, cx);
                    }
                });
            }
            Ok(Some(update)) => {
                log::info!("store update: {:?} is available", update.version);
                let skipped = !check_type.is_manual()
                    && update.version.is_some()
                    && update.version == self.skipped;
                if self.update.as_ref() != Some(&update) {
                    self.downloaded = false;
                }
                if skipped {
                    log::info!("store update: it was skipped");
                    self.update = None;
                    self.entry.update(cx, |entry, cx| {
                        entry.set_offer(None, cx);
                        entry.set_state(ButtonState::Hidden, cx);
                    });
                    return;
                }
                let offer = self.offer(&update);
                self.update = Some(update);
                let state = self.resting_state();
                self.entry.update(cx, |entry, cx| {
                    entry.set_offer(Some(offer), cx);
                    entry.set_state(state, cx);
                });
                if !self.downloaded {
                    self.download_in_background(cx);
                }
            }
        }
    }

    /// Downloads the update silently if the Store allows it; otherwise it's
    /// downloaded when the user installs it.
    fn download_in_background(&mut self, cx: &mut Context<Self>) {
        self.activity = Activity::Downloading;
        let client = self.client.clone();
        let (progress, mut percents) = progress_channel();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    guarded(|| {
                        if !client.can_silently_download()? {
                            return Ok(None);
                        }
                        client.download(progress).map(Some)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                let silent = matches!(result, Ok(Some(_)));
                if silent != this.silent {
                    this.silent = silent;
                    if !silent {
                        this.on_quit = false;
                    }
                    if let Some(offer) = this.update.as_ref().map(|update| this.offer(update)) {
                        this.entry
                            .update(cx, |entry, cx| entry.set_offer(Some(offer), cx));
                    }
                }
                match result {
                    Ok(Some(StoreOutcome::Completed)) => {
                        log::info!("store update: downloaded");
                        this.downloaded = true;
                    }
                    Ok(Some(outcome)) => {
                        log::warn!("store update: background download ended: {outcome:?}")
                    }
                    Ok(None) => log::info!(
                        "store update: can't download silently; it's downloaded when installed"
                    ),
                    Err(error) => log::warn!("store update: background download failed: {error:#}"),
                }
                this.rest(cx);
            })
            .ok();
        })
        .detach();
        self.follow_progress(&mut percents, cx);
    }

    /// Shows download percentages on the button while they come.
    fn follow_progress(
        &self,
        percents: &mut Option<smol::channel::Receiver<u8>>,
        cx: &mut Context<Self>,
    ) {
        let Some(percents) = percents.take() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            while let Ok(percent) = percents.recv().await {
                let shown = this.update(cx, |this, cx| {
                    if matches!(this.activity, Activity::Downloading | Activity::Installing) {
                        this.show(
                            ButtonState::Downloading {
                                percent: Some(percent),
                            },
                            cx,
                        );
                    }
                });
                if shown.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn act(&mut self, action: UpdateAction, cx: &mut Context<Self>) {
        match action {
            UpdateAction::Restart => self.install_now(cx),
            UpdateAction::OnQuit => {
                if self.update.is_some() {
                    self.on_quit = true;
                    if self.activity == Activity::Idle {
                        self.rest(cx);
                    }
                }
            }
            UpdateAction::ViewNotes => {
                if let Some(version) = self.update.as_ref().and_then(|u| u.version.as_ref()) {
                    cx.open_url(&crate::solo_update::release_page_url(version));
                }
            }
            UpdateAction::Skip => {
                let Some(update) = self.update.take() else {
                    return;
                };
                self.on_quit = false;
                self.downloaded = false;
                if let Some(version) = update.version {
                    log::info!("store update: skipping {version}");
                    if let Some(kvp) = key_value_store(cx) {
                        let skipped = version.to_string();
                        cx.background_spawn(async move {
                            kvp.write_kvp(SKIPPED_VERSION_KEY.to_string(), skipped)
                                .await
                                .log_err();
                        })
                        .detach();
                    }
                    self.skipped = Some(version);
                }
                self.entry.update(cx, |entry, cx| {
                    entry.set_offer(None, cx);
                    entry.set_state(ButtonState::Hidden, cx);
                });
            }
            UpdateAction::Retry => match self.retry.take() {
                Some(Retry::Install) if self.update.is_some() => self.install_now(cx),
                _ => self.check(UpdateCheckType::Manual, cx),
            },
        }
    }

    /// "Update and restart": the Store installs the update, which closes
    /// Solo, and Windows starts it again.
    fn install_now(&mut self, cx: &mut Context<Self>) {
        if self.update.is_none() || self.activity != Activity::Idle {
            return;
        }
        self.activity = Activity::Installing;
        self.show(ButtonState::Downloading { percent: None }, cx);
        let client = self.client.clone();
        let window = self.window;
        let (progress, mut percents) = progress_channel();
        cx.spawn(async move |this, cx| {
            // Unsaved files are asked about, and the session saved, so the
            // workspaces come back when Windows starts Solo again.
            let windows = cx.update(|cx| {
                cx.windows()
                    .into_iter()
                    .filter_map(|window| window.downcast::<workspace::MultiWorkspace>())
                    .collect::<Vec<_>>()
            });
            // SOLO: Windows starts Solo again: what runs keeps running.
            if !workspace::prepare_windows_to_restart(&windows, cx).await {
                this.update(cx, |this, cx| {
                    log::info!("store update: cancelled because Solo didn't quit");
                    this.activity = Activity::Idle;
                    this.rest(cx);
                })
                .ok();
                return;
            }
            let result = cx
                .background_spawn(async move {
                    guarded(|| {
                        let silent = client.can_silently_download().unwrap_or(false);
                        log::info!("store update: installing (silently: {silent})");
                        client.install(silent, window, progress)
                    })
                })
                .await;
            this.update(cx, |this, cx| this.finish_install(result, cx))
                .ok();
        })
        .detach();
        self.follow_progress(&mut percents, cx);
    }

    fn finish_install(&mut self, result: Result<StoreOutcome>, cx: &mut Context<Self>) {
        self.activity = Activity::Idle;
        // SOLO: unless the Store closes Solo to install, Solo doesn't
        // restart after all.
        if !matches!(result, Ok(StoreOutcome::Completed)) {
            workspace::solo_quit::restart_cancelled(cx);
        }
        match result {
            Ok(StoreOutcome::Completed) => {
                // The Store closes Solo to install; until it does:
                log::info!("store update: installed; waiting for the Store to restart Solo");
                self.show(ButtonState::Installing, cx);
            }
            Ok(StoreOutcome::Canceled) => {
                log::info!("store update: the user cancelled the Store's dialog");
                self.rest(cx);
            }
            Ok(StoreOutcome::Failed(reason)) => {
                log::warn!("store update: install failed: {reason}");
                self.retry = Some(Retry::Install);
                self.show(ButtonState::Failed { reason }, cx);
            }
            Err(error) => {
                log::warn!("store update: install failed: {error:#}");
                self.retry = Some(Retry::Install);
                self.show(
                    ButtonState::Failed {
                        reason: format!("{error:#}"),
                    },
                    cx,
                );
            }
        }
    }

    /// "Install when Solo quits": a silent install starts as Solo quits, and
    /// Solo isn't started again. If the Store doesn't allow a silent one, the
    /// update stays for the next start (or the Store's own updating).
    fn install_at_quit(&mut self) {
        if !self.on_quit || self.update.is_none() {
            return;
        }
        // The user quit: Solo isn't to come back when the install closes it.
        util::packaged::unregister_restart_after_update();
        let client = self.client.clone();
        let result = guarded(|| {
            if !client.can_silently_download()? {
                anyhow::bail!("the Store doesn't allow installing silently");
            }
            client.start_install_at_quit()
        });
        match result {
            Ok(()) => log::info!("store update: installing as Solo quits"),
            Err(error) => log::warn!("store update: couldn't install at quit: {error:#}"),
        }
    }
}

/// A progress callback for a Store call and the percentages it sends.
fn progress_channel() -> (ProgressFn, Option<smol::channel::Receiver<u8>>) {
    let (sender, receiver) = smol::channel::unbounded();
    (
        Box::new(move |percent| {
            sender.try_send(percent).ok();
        }),
        Some(receiver),
    )
}

/// The Store's percentage (0 to 1) as a whole number.
pub fn percent_of(progress: f64) -> u8 {
    if progress.is_finite() {
        (progress.clamp(0.0, 1.0) * 100.0).round() as u8
    } else {
        0
    }
}

/// A Store package version (`Major.Minor.Build.Revision`) as Solo's version:
/// Solo's packages are numbered `<version>.0`.
pub fn version_of_package(major: u16, minor: u16, build: u16) -> Version {
    Version::new(major.into(), minor.into(), build.into())
}

#[cfg(target_os = "windows")]
mod windows_store {
    //! The real Store, through `Windows.Services.Store`.

    use super::{
        ProgressFn, StoreOutcome, StoreUpdate, StoreUpdates, percent_of, version_of_package,
    };
    use anyhow::{Context as _, Result};
    use parking_lot::Mutex;
    use windows::Services::Store::{
        StoreContext, StorePackageUpdate, StorePackageUpdateResult, StorePackageUpdateState,
        StorePackageUpdateStatus,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::IInitializeWithWindow;
    use windows::core::Interface as _;
    use windows_collections::IIterable;
    use windows_future::{AsyncOperationProgressHandler, IAsyncOperationWithProgress};

    /// The Store's context and the updates it found last.
    #[derive(Default)]
    pub struct WindowsStore {
        state: Mutex<Option<(StoreContext, Vec<StorePackageUpdate>)>>,
    }

    impl WindowsStore {
        fn context(&self) -> Result<StoreContext> {
            if let Some((context, _)) = &*self.state.lock() {
                return Ok(context.clone());
            }
            StoreContext::GetDefault().context("couldn't reach the Microsoft Store")
        }

        fn found(&self) -> Result<(StoreContext, IIterable<StorePackageUpdate>)> {
            let state = self.state.lock();
            let (context, updates) = state
                .as_ref()
                .filter(|(_, updates)| !updates.is_empty())
                .context("no update was found")?;
            let updates: Vec<Option<StorePackageUpdate>> =
                updates.iter().cloned().map(Some).collect();
            Ok((context.clone(), IIterable::from(updates)))
        }
    }

    type UpdateOperation =
        IAsyncOperationWithProgress<StorePackageUpdateResult, StorePackageUpdateStatus>;

    /// Waits for a Store download or install, reporting its progress.
    fn wait(operation: UpdateOperation, progress: ProgressFn) -> Result<StoreOutcome> {
        operation.SetProgress(&AsyncOperationProgressHandler::new(
            move |_, status: windows::core::Ref<StorePackageUpdateStatus>| {
                if let Some(status) = status.as_ref() {
                    progress(percent_of(status.TotalDownloadProgress));
                }
                Ok(())
            },
        ))?;
        let result = operation.join()?;
        let state = result.OverallState()?;
        Ok(match state {
            StorePackageUpdateState::Completed => StoreOutcome::Completed,
            StorePackageUpdateState::Canceled => StoreOutcome::Canceled,
            other => StoreOutcome::Failed(state_reason(other)),
        })
    }

    fn state_reason(state: StorePackageUpdateState) -> String {
        let (english, chinese) = match state {
            StorePackageUpdateState::ErrorLowBattery => ("The battery is low", "电量低"),
            StorePackageUpdateState::ErrorWiFiRecommended
            | StorePackageUpdateState::ErrorWiFiRequired => {
                ("The Store waits for Wi-Fi", "Store 要等 Wi-Fi")
            }
            _ => (
                "The Microsoft Store couldn't update Solo",
                "Microsoft Store 没能更新 Solo",
            ),
        };
        util::i18n::tr(english, chinese).to_string()
    }

    impl StoreUpdates for WindowsStore {
        fn find_update(&self) -> Result<Option<StoreUpdate>> {
            let context = self.context()?;
            let found = context
                .GetAppAndOptionalStorePackageUpdatesAsync()?
                .join()
                .context("couldn't ask the Microsoft Store for updates")?;
            let mut updates = Vec::new();
            for index in 0..found.Size()? {
                updates.push(found.GetAt(index)?);
            }
            let version = updates.first().and_then(|update| {
                let version = update.Package().ok()?.Id().ok()?.Version().ok()?;
                Some(version_of_package(
                    version.Major,
                    version.Minor,
                    version.Build,
                ))
            });
            let available = !updates.is_empty();
            *self.state.lock() = Some((context, updates));
            Ok(available.then_some(StoreUpdate { version }))
        }

        fn can_silently_download(&self) -> Result<bool> {
            Ok(self.context()?.CanSilentlyDownloadStorePackageUpdates()?)
        }

        fn download(&self, progress: ProgressFn) -> Result<StoreOutcome> {
            let (context, updates) = self.found()?;
            wait(
                context.TrySilentDownloadStorePackageUpdatesAsync(&updates)?,
                progress,
            )
        }

        fn install(
            &self,
            silent: bool,
            window: Option<isize>,
            progress: ProgressFn,
        ) -> Result<StoreOutcome> {
            let (context, updates) = self.found()?;
            if silent {
                return wait(
                    context.TrySilentDownloadAndInstallStorePackageUpdatesAsync(&updates)?,
                    progress,
                );
            }
            // The Store's dialog belongs to Solo's window.
            if let Some(window) = window {
                let initialize: IInitializeWithWindow = context.cast()?;
                // SAFETY: `window` is the handle of one of Solo's windows.
                if let Err(error) =
                    unsafe { initialize.Initialize(HWND(window as *mut core::ffi::c_void)) }
                {
                    log::warn!("store update: couldn't give the Store Solo's window: {error}");
                }
            }
            wait(
                context.RequestDownloadAndInstallStorePackageUpdatesAsync(&updates)?,
                progress,
            )
        }

        fn start_install_at_quit(&self) -> Result<()> {
            let (context, updates) = self.found()?;
            // Not waited for: Solo quits meanwhile.
            drop(context.TrySilentDownloadAndInstallStorePackageUpdatesAsync(&updates)?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use parking_lot::Mutex;

    /// A Store that answers as told and counts what it's asked.
    #[derive(Default)]
    struct FakeStore {
        update: Mutex<Option<Result<Option<StoreUpdate>, String>>>,
        silent: Mutex<bool>,
        download: Mutex<Option<StoreOutcome>>,
        install: Mutex<Option<Result<StoreOutcome, String>>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeStore {
        fn called(&self, call: &str) {
            self.calls.lock().push(call.to_string());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().clone()
        }
    }

    impl StoreUpdates for FakeStore {
        fn find_update(&self) -> Result<Option<StoreUpdate>> {
            self.called("find");
            match self.update.lock().clone() {
                Some(Ok(update)) => Ok(update),
                Some(Err(error)) => Err(anyhow!(error)),
                None => panic!("the Store crashed"),
            }
        }

        fn can_silently_download(&self) -> Result<bool> {
            Ok(*self.silent.lock())
        }

        fn download(&self, progress: ProgressFn) -> Result<StoreOutcome> {
            self.called("download");
            progress(50);
            progress(100);
            Ok(self
                .download
                .lock()
                .clone()
                .unwrap_or(StoreOutcome::Completed))
        }

        fn install(
            &self,
            silent: bool,
            _window: Option<isize>,
            progress: ProgressFn,
        ) -> Result<StoreOutcome> {
            self.called(if silent {
                "install silently"
            } else {
                "install with dialog"
            });
            progress(80);
            match self.install.lock().clone() {
                Some(Ok(outcome)) => Ok(outcome),
                Some(Err(error)) => Err(anyhow!(error)),
                None => Ok(StoreOutcome::Completed),
            }
        }

        fn start_install_at_quit(&self) -> Result<()> {
            self.called("install at quit");
            Ok(())
        }
    }

    fn update(version: &str) -> Option<StoreUpdate> {
        Some(StoreUpdate {
            version: Some(Version::parse(version).unwrap()),
        })
    }

    fn setup(cx: &mut TestAppContext, auto_update: bool) -> (Arc<FakeStore>, Entity<StoreUpdater>) {
        let store = Arc::new(FakeStore::default());
        let updater = cx.update(|cx| {
            let mut settings = SettingsStore::new(cx, &settings::default_settings());
            settings
                .set_user_settings(&format!(r#"{{"auto_update": {auto_update}}}"#), cx)
                .unwrap();
            cx.set_global(settings);
            start(store.clone(), cx)
        });
        (store, updater)
    }

    fn button(cx: &mut TestAppContext) -> ButtonState {
        cx.update(|cx| update_button::entry(cx).unwrap().read(cx).state().clone())
    }

    fn offered_version(cx: &mut TestAppContext) -> Option<Version> {
        cx.update(|cx| {
            update_button::entry(cx)
                .unwrap()
                .read(cx)
                .offer()
                .and_then(|offer| offer.version.clone())
        })
    }

    fn offered_on_quit(cx: &mut TestAppContext) -> Option<bool> {
        cx.update(|cx| {
            update_button::entry(cx)
                .unwrap()
                .read(cx)
                .offer()
                .map(|offer| offer.can_install_on_quit)
        })
    }

    fn act(updater: &Entity<StoreUpdater>, action: UpdateAction, cx: &mut TestAppContext) {
        updater.update(cx, |updater, cx| updater.act(action, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn checks_on_schedule_and_not_when_auto_update_is_off(cx: &mut TestAppContext) {
        let (store, _updater) = setup(cx, true);
        *store.update.lock() = Some(Ok(None));
        cx.executor().advance_clock(FIRST_CHECK_DELAY / 2);
        cx.run_until_parked();
        assert!(store.calls().is_empty());
        cx.executor().advance_clock(FIRST_CHECK_DELAY);
        cx.run_until_parked();
        assert_eq!(store.calls(), ["find"]);
        // Nothing new: no button.
        assert_eq!(button(cx), ButtonState::Hidden);
        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
        assert_eq!(store.calls(), ["find", "find"]);
    }

    #[gpui::test]
    async fn no_checks_when_auto_update_is_off(cx: &mut TestAppContext) {
        let (store, _updater) = setup(cx, false);
        *store.update.lock() = Some(Ok(None));
        cx.executor().advance_clock(POLL_INTERVAL * 2);
        cx.run_until_parked();
        assert!(store.calls().is_empty());
        // Asking still checks.
        cx.update(check_manually);
        cx.run_until_parked();
        assert_eq!(store.calls(), ["find"]);
        assert_eq!(button(cx), ButtonState::UpToDate);
        cx.executor()
            .advance_clock(update_button::UP_TO_DATE_DURATION * 2);
        assert_eq!(button(cx), ButtonState::Hidden);
    }

    #[gpui::test]
    async fn downloads_silently_when_the_store_allows(cx: &mut TestAppContext) {
        let (store, updater) = setup(cx, false);
        *store.update.lock() = Some(Ok(update("2.5.0")));
        *store.silent.lock() = true;
        cx.update(check_manually);
        cx.run_until_parked();
        assert_eq!(store.calls(), ["find", "download"]);
        assert!(updater.read_with(cx, |updater, _| updater.downloaded));
        assert_eq!(button(cx), ButtonState::Available);
        assert_eq!(offered_version(cx), Some(Version::new(2, 5, 0)));
        assert_eq!(offered_on_quit(cx), Some(true));

        // Installing now: silently, and the Store closes Solo.
        act(&updater, UpdateAction::Restart, cx);
        assert_eq!(store.calls(), ["find", "download", "install silently"]);
        assert_eq!(button(cx), ButtonState::Installing);
    }

    #[gpui::test]
    async fn without_silent_downloads_only_offers(cx: &mut TestAppContext) {
        let (store, updater) = setup(cx, false);
        *store.update.lock() = Some(Ok(update("2.5.0")));
        cx.update(check_manually);
        cx.run_until_parked();
        assert_eq!(store.calls(), ["find"]);
        assert_eq!(button(cx), ButtonState::Available);
        // Installing at quit can't work without silent installs: not offered.
        assert_eq!(offered_on_quit(cx), Some(false));

        // The user asks: the Store's dialog; cancelling it keeps the offer.
        *store.install.lock() = Some(Ok(StoreOutcome::Canceled));
        act(&updater, UpdateAction::Restart, cx);
        assert_eq!(store.calls(), ["find", "install with dialog"]);
        assert_eq!(button(cx), ButtonState::Available);
    }

    #[gpui::test]
    async fn failed_installs_say_why_and_retry(cx: &mut TestAppContext) {
        let (store, updater) = setup(cx, false);
        *store.update.lock() = Some(Ok(update("2.5.0")));
        *store.silent.lock() = true;
        *store.download.lock() = Some(StoreOutcome::Failed("disk full".into()));
        cx.update(check_manually);
        cx.run_until_parked();
        // A background download that failed isn't the user's problem yet.
        assert_eq!(button(cx), ButtonState::Available);
        assert!(!updater.read_with(cx, |updater, _| updater.downloaded));

        *store.install.lock() = Some(Err("network error".into()));
        act(&updater, UpdateAction::Restart, cx);
        assert_eq!(
            button(cx),
            ButtonState::Failed {
                reason: "network error".into()
            }
        );
        *store.install.lock() = Some(Ok(StoreOutcome::Completed));
        act(&updater, UpdateAction::Retry, cx);
        assert_eq!(store.calls().last().unwrap(), "install silently");
        assert_eq!(button(cx), ButtonState::Installing);
    }

    #[gpui::test]
    async fn offline_or_crashing_store_never_takes_solo_down(cx: &mut TestAppContext) {
        let (store, updater) = setup(cx, true);
        *store.update.lock() = Some(Err("no network".into()));
        cx.executor().advance_clock(FIRST_CHECK_DELAY);
        cx.run_until_parked();
        // An automatic check says nothing.
        assert_eq!(button(cx), ButtonState::Hidden);

        // A panicking Store call is an error, not a crash.
        *store.update.lock() = None;
        cx.update(check_manually);
        cx.run_until_parked();
        let ButtonState::Failed { reason } = button(cx) else {
            panic!("expected a failure, got {:?}", button(cx));
        };
        assert!(reason.contains("panicked"), "{reason}");

        // Retry checks again; and later checks still run.
        *store.update.lock() = Some(Ok(update("2.5.0")));
        act(&updater, UpdateAction::Retry, cx);
        assert_eq!(button(cx), ButtonState::Available);
        assert_eq!(store.calls().iter().filter(|c| *c == "find").count(), 3);
    }

    #[gpui::test]
    async fn skipping_hides_until_a_newer_version(cx: &mut TestAppContext) {
        let (store, updater) = setup(cx, true);
        *store.update.lock() = Some(Ok(update("2.5.0")));
        cx.executor().advance_clock(FIRST_CHECK_DELAY);
        cx.run_until_parked();
        assert_eq!(button(cx), ButtonState::Available);
        act(&updater, UpdateAction::Skip, cx);
        assert_eq!(button(cx), ButtonState::Hidden);

        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
        assert_eq!(button(cx), ButtonState::Hidden);

        *store.update.lock() = Some(Ok(update("2.5.1")));
        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
        assert_eq!(button(cx), ButtonState::Available);
        assert_eq!(offered_version(cx), Some(Version::new(2, 5, 1)));
    }

    #[gpui::test]
    async fn installs_at_quit_when_chosen(cx: &mut TestAppContext) {
        let (store, updater) = setup(cx, false);
        *store.update.lock() = Some(Ok(update("2.5.0")));
        *store.silent.lock() = true;
        cx.update(check_manually);
        cx.run_until_parked();
        act(&updater, UpdateAction::OnQuit, cx);
        assert_eq!(button(cx), ButtonState::OnQuit);
        updater.update(cx, |updater, _| updater.install_at_quit());
        assert_eq!(store.calls().last().unwrap(), "install at quit");

        // Without silent installs nothing starts at quit.
        let (store, updater) = setup(cx, false);
        *store.update.lock() = Some(Ok(update("2.5.0")));
        cx.update(check_manually);
        cx.run_until_parked();
        act(&updater, UpdateAction::OnQuit, cx);
        updater.update(cx, |updater, _| updater.install_at_quit());
        assert!(!store.calls().contains(&"install at quit".to_string()));
    }

    #[test]
    fn store_numbers() {
        assert_eq!(percent_of(0.456), 46);
        assert_eq!(percent_of(1.5), 100);
        assert_eq!(percent_of(f64::NAN), 0);
        assert_eq!(version_of_package(2, 5, 1), Version::new(2, 5, 1));
    }
}
