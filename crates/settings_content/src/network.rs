//! SOLO: Solo's network settings: offline mode and asking before downloads.

use collections::HashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// SOLO: Solo's network settings.
#[with_fallible_options]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct NetworkSettingsContent {
    /// Offline mode: Solo only reaches this computer and the local network
    /// (local models and SSH keep working), and downloads nothing.
    ///
    /// Default: false
    pub offline: Option<bool>,
}

/// SOLO: what Solo does when it needs to download a component by itself (a
/// language server, Node.js, Prettier, a debug adapter, Copilot, extension
/// updates).
#[with_fallible_options]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct DownloadsSettingsContent {
    /// "ask" (once per component), "auto" (download without asking) or
    /// "never" (use only what's installed).
    ///
    /// Default: ask
    pub mode: Option<DownloadMode>,
    /// Components that may (true) or may not (false) be downloaded, by id:
    /// a language server's name ("rust-analyzer", "vtsls"), "node",
    /// "prettier", "copilot", "extension-updates", or a debug adapter
    /// ("codelldb", "js-debug", "delve", "debugpy"). Solo adds the ones you
    /// allow.
    ///
    /// Default: {}
    pub allowed: Option<HashMap<String, bool>>,
    /// Whether allowed components check for and download their updates.
    ///
    /// Default: true
    pub check_updates: Option<bool>,
}

/// SOLO: what Solo does when it needs to download a component.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum DownloadMode {
    /// Ask once per component.
    #[default]
    Ask,
    /// Download without asking.
    Auto,
    /// Never download; use only what's installed.
    Never,
}
