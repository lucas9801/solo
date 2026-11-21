//! Provides constructs for the Zed app version and release channel.

#![deny(missing_docs)]

use std::{env, str::FromStr, sync::LazyLock};

use gpui::{App, Global};
use semver::Version;

/// stable | dev | nightly | preview
pub static RELEASE_CHANNEL_NAME: LazyLock<String> = LazyLock::new(|| {
    if cfg!(debug_assertions) {
        env::var("SOLO_RELEASE_CHANNEL").unwrap_or_else(|_| compile_time_release_channel_name())
    } else {
        compile_time_release_channel_name()
    }
});

/// When a crate in zed is used as a dependency that uses the `crane` nix
/// library, it vendors each crate separately and builds it in isolation, which
/// makes the `include_str!` fail.
///
/// The build script checks for `$SOLO_RELEASE_CHANNEL` and emits the `cfg`
#[cfg(__do_not_set_zed_release_channel)]
fn compile_time_release_channel_name() -> String {
    env!("SOLO_RELEASE_CHANNEL").trim().to_string()
}

#[cfg(not(__do_not_set_zed_release_channel))]
fn compile_time_release_channel_name() -> String {
    include_str!("../../solo/RELEASE_CHANNEL")
        .trim()
        .to_string()
}

#[doc(hidden)]
pub static RELEASE_CHANNEL: LazyLock<ReleaseChannel> =
    LazyLock::new(|| match ReleaseChannel::from_str(&RELEASE_CHANNEL_NAME) {
        Ok(channel) => channel,
        _ => panic!("invalid release channel {}", *RELEASE_CHANNEL_NAME),
    });

/// The app identifier for the current release channel, Windows only.
#[cfg(target_os = "windows")]
pub fn app_identifier() -> &'static str {
    // SOLO: a debug build of the stable channel mustn't hand off to (or take
    // over from) the installed Solo.
    if cfg!(debug_assertions) && *RELEASE_CHANNEL == ReleaseChannel::Stable {
        return "Solo-Terminal-Stable-Debug";
    }
    match *RELEASE_CHANNEL {
        // SOLO: its own single-instance mutex and pipe, so opening Solo never
        // hands off to a running Zed or the other way around.
        ReleaseChannel::Dev => "Solo-Terminal-Dev",
        ReleaseChannel::Nightly => "Solo-Terminal-Nightly",
        ReleaseChannel::Preview => "Solo-Terminal-Preview",
        ReleaseChannel::Stable => "Solo-Terminal-Stable",
    }
}

/// SOLO: the name of the single-instance mutex and pipe (before
/// `-Instance-Mutex` and `-Named-Pipe`) of a Solo whose data is in
/// `custom_data_dir` (`--user-data-dir`): the app identifier, with a short
/// hash of that folder when there's one. Solos with different data folders
/// then never hand what they open to each other (a test build with its own
/// data to the installed Solo, say), and `solo-cli` reaches the one of its
/// data folder; with the default data folder it's the app identifier, as
/// before.
#[cfg(target_os = "windows")]
pub fn instance_identifier(custom_data_dir: Option<&std::path::Path>) -> String {
    match custom_data_dir {
        None => app_identifier().to_string(),
        Some(dir) => format!("{}-{:012x}", app_identifier(), data_dir_hash(dir)),
    }
}

/// SOLO: whether `name` is an instance identifier of this release channel
/// (see [`instance_identifier`]): what `solo-cli` accepts from the
/// environment of the terminal it runs in.
#[cfg(target_os = "windows")]
pub fn is_instance_identifier(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(app_identifier()) else {
        return false;
    };
    rest.is_empty()
        || rest.strip_prefix('-').is_some_and(|hash| {
            hash.len() == 12 && hash.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// SOLO: a 48-bit FNV-1a hash of `dir`, compared as Windows compares paths:
/// ignoring case and a trailing separator.
#[cfg(target_os = "windows")]
fn data_dir_hash(dir: &std::path::Path) -> u64 {
    let text = dir.to_string_lossy().to_lowercase();
    let text = text.trim_end_matches(['\\', '/']);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash & 0xffff_ffff_ffff
}

/// The Git commit SHA that Zed was built at.
#[derive(Clone, Eq, Debug, PartialEq)]
pub struct AppCommitSha(String);

struct GlobalAppCommitSha(AppCommitSha);

impl Global for GlobalAppCommitSha {}

impl AppCommitSha {
    /// Creates a new [`AppCommitSha`].
    pub fn new(sha: String) -> Self {
        AppCommitSha(sha)
    }

    /// Returns the global [`AppCommitSha`], if one is set.
    pub fn try_global(cx: &App) -> Option<AppCommitSha> {
        cx.try_global::<GlobalAppCommitSha>()
            .map(|sha| sha.0.clone())
    }

    /// Sets the global [`AppCommitSha`].
    pub fn set_global(sha: AppCommitSha, cx: &mut App) {
        cx.set_global(GlobalAppCommitSha(sha))
    }

    /// Returns the full commit SHA.
    pub fn full(&self) -> String {
        self.0.to_string()
    }

    /// Returns the short (7 character) commit SHA.
    pub fn short(&self) -> String {
        self.0.chars().take(7).collect()
    }
}

struct GlobalAppVersion(Version);

impl Global for GlobalAppVersion {}

/// The version of Zed.
pub struct AppVersion;

impl AppVersion {
    /// Load the app version from env.
    pub fn load(
        pkg_version: &str,
        build_id: Option<&str>,
        commit_sha: Option<AppCommitSha>,
    ) -> Version {
        let mut version: Version = if let Ok(from_env) = env::var("SOLO_APP_VERSION") {
            from_env.parse().expect("invalid SOLO_APP_VERSION")
        } else {
            pkg_version.parse().expect("invalid version in Cargo.toml")
        };
        let mut pre = String::from(RELEASE_CHANNEL.dev_name());

        if let Some(build_id) = build_id {
            pre.push('.');
            pre.push_str(&build_id);
        }

        if let Some(sha) = commit_sha {
            pre.push('.');
            pre.push_str(&sha.0);
        }
        if let Ok(build) = semver::BuildMetadata::new(&pre) {
            version.build = build;
        }

        version
    }

    /// Returns the global version number.
    pub fn global(cx: &App) -> Version {
        if cx.has_global::<GlobalAppVersion>() {
            cx.global::<GlobalAppVersion>().0.clone()
        } else {
            Version::new(0, 0, 0)
        }
    }
}

/// A Zed release channel.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum ReleaseChannel {
    /// The development release channel.
    ///
    /// Used for local debug builds of Zed.
    #[default]
    Dev,

    /// The Nightly release channel.
    Nightly,

    /// The Preview release channel.
    Preview,

    /// The Stable release channel.
    Stable,
}

struct GlobalReleaseChannel(ReleaseChannel);

impl Global for GlobalReleaseChannel {}

/// Initializes the release channel.
pub fn init(app_version: Version, cx: &mut App) {
    cx.set_global(GlobalAppVersion(app_version));
    cx.set_global(GlobalReleaseChannel(*RELEASE_CHANNEL))
}

/// Initializes the release channel for tests that rely on fake release channel.
pub fn init_test(app_version: Version, release_channel: ReleaseChannel, cx: &mut App) {
    cx.set_global(GlobalAppVersion(app_version));
    cx.set_global(GlobalReleaseChannel(release_channel))
}

impl ReleaseChannel {
    /// All release channels.
    pub const ALL: [ReleaseChannel; 4] = [
        ReleaseChannel::Dev,
        ReleaseChannel::Nightly,
        ReleaseChannel::Preview,
        ReleaseChannel::Stable,
    ];

    /// Returns the global [`ReleaseChannel`].
    pub fn global(cx: &App) -> Self {
        cx.global::<GlobalReleaseChannel>().0
    }

    /// Returns the global [`ReleaseChannel`], if one is set.
    pub fn try_global(cx: &App) -> Option<Self> {
        cx.try_global::<GlobalReleaseChannel>()
            .map(|channel| channel.0)
    }

    /// Returns whether we want to poll for updates for this [`ReleaseChannel`]
    pub fn poll_for_updates(&self) -> bool {
        // SOLO: updates come from Zed's servers and would replace Solo with Zed.
        false
    }

    /// Returns the display name for this [`ReleaseChannel`].
    pub fn display_name(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "Solo Dev",
            ReleaseChannel::Nightly => "Solo Nightly",
            ReleaseChannel::Preview => "Solo Preview",
            ReleaseChannel::Stable => "Solo",
        }
    }

    /// Returns the programmatic name for this [`ReleaseChannel`].
    pub fn dev_name(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "dev",
            ReleaseChannel::Nightly => "nightly",
            ReleaseChannel::Preview => "preview",
            ReleaseChannel::Stable => "stable",
        }
    }

    /// Returns the application ID that's used by Wayland as application ID
    /// and WM_CLASS on X11.
    /// This also has to match the bundle identifier for Zed on macOS.
    pub fn app_id(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "dev.solo.Solo-Dev",
            ReleaseChannel::Nightly => "dev.solo.Solo-Nightly",
            ReleaseChannel::Preview => "dev.solo.Solo-Preview",
            ReleaseChannel::Stable => "dev.solo.Solo",
        }
    }

    /// Returns the query parameter for this [`ReleaseChannel`].
    pub fn release_query_param(&self) -> Option<&'static str> {
        match self {
            Self::Dev => None,
            Self::Nightly => Some("nightly=1"),
            Self::Preview => Some("preview=1"),
            Self::Stable => None,
        }
    }
}

/// Error indicating that release channel string does not match any known release channel names.
#[derive(Copy, Clone, Debug, Hash, PartialEq)]
pub struct InvalidReleaseChannel;

impl FromStr for ReleaseChannel {
    type Err = InvalidReleaseChannel;

    fn from_str(channel: &str) -> Result<Self, Self::Err> {
        Ok(match channel {
            "dev" => ReleaseChannel::Dev,
            "nightly" => ReleaseChannel::Nightly,
            "preview" => ReleaseChannel::Preview,
            "stable" => ReleaseChannel::Stable,
            _ => return Err(InvalidReleaseChannel),
        })
    }
}

#[cfg(all(test, target_os = "windows"))]
mod instance_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_instance_identifier() {
        // The default data folder keeps the name installed Solos use.
        assert_eq!(instance_identifier(None), app_identifier());
        let test = instance_identifier(Some(Path::new(r"E:\solo-test\data")));
        let other = instance_identifier(Some(Path::new(r"E:\solo-test\other")));
        assert_ne!(test, app_identifier());
        assert_ne!(test, other);
        assert!(test.starts_with(app_identifier()));
        // The same folder, spelled differently.
        assert_eq!(
            test,
            instance_identifier(Some(Path::new(r"e:\SOLO-TEST\data\")))
        );
        assert!(is_instance_identifier(&test));
        assert!(is_instance_identifier(app_identifier()));
        assert!(!is_instance_identifier("Other-App"));
        assert!(!is_instance_identifier(&format!("{}-xyz", app_identifier())));
        assert!(!is_instance_identifier(&format!(
            "{}-0123456789ab\\..\\x",
            app_identifier()
        )));
    }
}
