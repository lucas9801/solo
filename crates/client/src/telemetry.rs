//! SOLO: what's left of Zed's telemetry. Zed queued usage events and sent
//! them to zed.dev; Solo sends no telemetry, so this only keeps the ids and
//! settings other code reads, and the OS details crash reports and system
//! specs use.

use crate::TelemetrySettings;
use gpui::App;
use parking_lot::Mutex;
use release_channel::ReleaseChannel;
use settings::{Settings, SettingsStore};
use std::env;
use std::sync::{Arc, LazyLock};

pub struct Telemetry {
    state: Arc<Mutex<TelemetryState>>,
}

struct TelemetryState {
    settings: TelemetrySettings,
    system_id: Option<Arc<str>>,       // Per system
    installation_id: Option<Arc<str>>, // Per app installation (different for dev, nightly, preview, and stable)
    metrics_id: Option<Arc<str>>,      // Per logged-in user
    is_staff: Option<bool>,
}

pub static MINIDUMP_ENDPOINT: LazyLock<Option<String>> = LazyLock::new(|| {
    option_env!("SOLO_MINIDUMP_ENDPOINT")
        .map(str::to_string)
        .or_else(|| env::var("SOLO_MINIDUMP_ENDPOINT").ok())
});

pub fn should_install_crash_handler(channel: ReleaseChannel) -> bool {
    matches!(
        // SOLO: also under its earlier name, which Solo's guide gave.
        env::var("SOLO_GENERATE_MINIDUMPS")
            .or_else(|_| env::var("ZED_GENERATE_MINIDUMPS"))
            .as_deref(),
        Ok("true" | "1")
    ) || (channel != ReleaseChannel::Dev && MINIDUMP_ENDPOINT.is_some())
}

pub fn os_name() -> String {
    #[cfg(target_os = "macos")]
    {
        "macOS".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        format!("Linux {}", gpui::guess_compositor())
    }
    #[cfg(target_os = "freebsd")]
    {
        format!("FreeBSD {}", gpui::guess_compositor())
    }

    #[cfg(target_os = "windows")]
    {
        "Windows".to_string()
    }
}

/// Note: This might do blocking IO! Only call from background threads
pub fn os_version() -> String {
    cfg_select! {
       feature = "test-support" => {
           // MacOS branch in particular is quite slow, hence we ought to "avoid" it in tests.
           "test binary".to_owned()
       }
       target_os = "macos" => {
           static MACOS_VERSION_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
               regex::Regex::new(r"(\s*\(Build [^)]*[0-9]\))").unwrap()
           });
           use objc2_foundation::NSProcessInfo;
           let process_info = NSProcessInfo::processInfo();
           let version_nsstring = process_info.operatingSystemVersionString();
           // "Version 15.6.1 (Build 24G90)" -> "15.6.1 (Build 24G90)"
           let version_string = version_nsstring.to_string().replace("Version ", "");
           // "15.6.1 (Build 24G90)" -> "15.6.1"
           // "26.0.0 (Build 25A5349a)" -> unchanged (Beta or Rapid Security Response; ends with letter)
           MACOS_VERSION_REGEX
               .replace_all(&version_string, "")
               .to_string()
       }
       any(target_os = "linux", target_os = "freebsd") => {
           use std::path::Path;

           let content = if let Ok(file) = std::fs::read_to_string(&Path::new("/etc/os-release")) {
               file
           } else if let Ok(file) = std::fs::read_to_string(&Path::new("/usr/lib/os-release")) {
               file
           } else if let Ok(file) = std::fs::read_to_string(&Path::new("/var/run/os-release")) {
               file
           } else {
               log::error!(
                   "Failed to load /etc/os-release, /usr/lib/os-release, or /var/run/os-release"
               );
               "".to_string()
           };
           util::parse_os_release(&content).unwrap_or_else(|| "unknown".to_string())
       }
       target_os = "windows" => {
           let mut info = unsafe { std::mem::zeroed() };
           let status = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut info) };
           if status.is_ok() {
               semver::Version::new(
                   info.dwMajorVersion as _,
                   info.dwMinorVersion as _,
                   info.dwBuildNumber as _,
               )
               .to_string()
           } else {
               "unknown".to_string()
           }
       }
    }
}

impl Telemetry {
    pub fn new(cx: &mut App) -> Arc<Self> {
        let state = Arc::new(Mutex::new(TelemetryState {
            settings: *TelemetrySettings::get_global(cx),
            system_id: None,
            installation_id: None,
            metrics_id: None,
            is_staff: None,
        }));

        cx.observe_global::<SettingsStore>({
            let state = state.clone();
            move |cx| {
                state.lock().settings = *TelemetrySettings::get_global(cx);
            }
        })
        .detach();

        Arc::new(Self { state })
    }

    pub fn start(
        self: &Arc<Self>,
        system_id: Option<String>,
        installation_id: Option<String>,
        _session_id: String,
        _cx: &App,
    ) {
        let mut state = self.state.lock();
        state.system_id = system_id.map(|id| id.into());
        state.installation_id = installation_id.map(|id| id.into());
    }

    pub fn metrics_enabled(self: &Arc<Self>) -> bool {
        self.state.lock().settings.metrics
    }

    pub fn diagnostics_enabled(self: &Arc<Self>) -> bool {
        self.state.lock().settings.diagnostics
    }

    pub fn set_authenticated_user_info(
        self: &Arc<Self>,
        metrics_id: Option<String>,
        is_staff: bool,
    ) {
        let mut state = self.state.lock();
        if !state.settings.metrics {
            return;
        }
        state.metrics_id = metrics_id.map(|id| id.into());
        state.is_staff = Some(is_staff);
    }

    pub fn metrics_id(self: &Arc<Self>) -> Option<Arc<str>> {
        self.state.lock().metrics_id.clone()
    }

    pub fn system_id(self: &Arc<Self>) -> Option<Arc<str>> {
        self.state.lock().system_id.clone()
    }

    pub fn installation_id(self: &Arc<Self>) -> Option<Arc<str>> {
        self.state.lock().installation_id.clone()
    }

    pub fn is_staff(self: &Arc<Self>) -> Option<bool> {
        self.state.lock().is_staff
    }
}
