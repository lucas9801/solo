//! SOLO: how much each language server analyzes in a project, as chosen by
//! the user: everything, only the open files (no background checks or
//! indexing), or nothing (the server is off). Chosen per project and per
//! server; the UI persists the choices and loads them into this global.
//!
//! The choice is applied to the server's settings when the language server
//! tree picks the servers for a file, so a change restarts or stops the
//! affected servers the same way a settings change does.

use std::path::{Path, PathBuf};

use collections::HashMap;
use gpui::{App, Global};
use lsp::LanguageServerName;
use serde::{Deserialize, Serialize};
use settings::LspSettings;

/// The value of `CARGO_BUILD_JOBS` in the default settings that stands for
/// "a quarter of the logical cores, at least 2".
pub const AUTO_CARGO_JOBS: &str = "auto";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisMode {
    #[default]
    Full,
    /// No background checking or indexing: only the open files.
    OpenFilesOnly,
    Off,
}

/// Servers whose background analysis can be paused, keeping them running
/// for the open files.
pub fn supports_open_files_only(server: &LanguageServerName) -> bool {
    matches!(&*server.0, "rust-analyzer" | "clangd")
}

/// Servers that check or index a whole project in the background and can
/// take gigabytes on a large one.
pub fn is_heavy_server(server: &LanguageServerName) -> bool {
    matches!(
        &*server.0,
        "rust-analyzer" | "clangd" | "roslyn" | "omnisharp" | "jdtls" | "gopls" | "sourcekit-lsp"
    )
}

/// The analysis modes chosen per project root and server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageServerModes {
    pub projects: HashMap<PathBuf, HashMap<LanguageServerName, AnalysisMode>>,
}

impl Global for LanguageServerModes {}

impl LanguageServerModes {
    pub fn mode(&self, project_root: &Path, server: &LanguageServerName) -> AnalysisMode {
        self.projects
            .get(project_root)
            .and_then(|servers| servers.get(server))
            .copied()
            .unwrap_or_default()
    }

    /// Whether the user picked a mode for this server in this project.
    pub fn is_chosen(&self, project_root: &Path, server: &LanguageServerName) -> bool {
        self.projects
            .get(project_root)
            .is_some_and(|servers| servers.contains_key(server))
    }

    pub fn set(&mut self, project_root: PathBuf, server: LanguageServerName, mode: AnalysisMode) {
        self.projects
            .entry(project_root)
            .or_default()
            .insert(server, mode);
    }
}

pub fn mode_for(project_root: &Path, server: &LanguageServerName, cx: &App) -> AnalysisMode {
    cx.try_global::<LanguageServerModes>()
        .map(|modes| modes.mode(project_root, server))
        .unwrap_or_default()
}

/// Sets a mode; local language server stores observe this global and apply
/// it.
pub fn set_mode(
    project_root: PathBuf,
    server: LanguageServerName,
    mode: AnalysisMode,
    cx: &mut App,
) {
    cx.default_global::<LanguageServerModes>()
        .set(project_root, server, mode);
}

fn cargo_jobs() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(4);
    (cores / 4).max(2)
}

/// Applies the mode to a server's settings. Returns `false` when the server
/// should not run at all.
pub fn apply_mode(
    server: &LanguageServerName,
    mode: AnalysisMode,
    settings: &mut LspSettings,
) -> bool {
    if &*server.0 == "rust-analyzer" {
        resolve_auto_cargo_jobs(settings);
    }
    match mode {
        AnalysisMode::Full => true,
        AnalysisMode::Off => false,
        AnalysisMode::OpenFilesOnly => {
            match &*server.0 {
                "rust-analyzer" => {
                    let options = settings
                        .initialization_options
                        .get_or_insert_with(|| serde_json::json!({}));
                    util::merge_json_value_into(
                        serde_json::json!({
                            "checkOnSave": false,
                            "cachePriming": { "enable": false },
                        }),
                        options,
                    );
                }
                "clangd" => {
                    let binary = settings.binary.get_or_insert_with(Default::default);
                    let arguments = binary.arguments.get_or_insert_with(Vec::new);
                    arguments.retain(|argument| !argument.starts_with("--background-index"));
                    arguments.push("--background-index=false".to_string());
                }
                _ => {}
            }
            true
        }
    }
}

/// Replaces the `"auto"` placeholder for `CARGO_BUILD_JOBS` in
/// rust-analyzer's `cargo.extraEnv` with a number: cargo would otherwise run
/// as many `rustc` as there are logical cores, each up to 1.5 GB on a large
/// workspace.
fn resolve_auto_cargo_jobs(settings: &mut LspSettings) {
    let Some(jobs) = settings
        .initialization_options
        .as_mut()
        .and_then(|options| options.pointer_mut("/cargo/extraEnv/CARGO_BUILD_JOBS"))
    else {
        return;
    };
    if jobs.as_str() == Some(AUTO_CARGO_JOBS) {
        *jobs = serde_json::Value::String(cargo_jobs().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_cargo_jobs_become_a_number() {
        let mut settings = LspSettings {
            initialization_options: Some(serde_json::json!({
                "cargo": { "extraEnv": { "CARGO_BUILD_JOBS": "auto", "OTHER": "1" } }
            })),
            ..Default::default()
        };
        let server = LanguageServerName::new_static("rust-analyzer");
        assert!(apply_mode(&server, AnalysisMode::Full, &mut settings));
        let options = settings.initialization_options.unwrap();
        let jobs: usize = options["cargo"]["extraEnv"]["CARGO_BUILD_JOBS"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(jobs >= 2);
        assert_eq!(options["cargo"]["extraEnv"]["OTHER"], "1");
        assert!(options.get("checkOnSave").is_none());

        // A number the user set is kept.
        let mut settings = LspSettings {
            initialization_options: Some(serde_json::json!({
                "cargo": { "extraEnv": { "CARGO_BUILD_JOBS": "default" } }
            })),
            ..Default::default()
        };
        apply_mode(&server, AnalysisMode::Full, &mut settings);
        assert_eq!(
            settings.initialization_options.unwrap()["cargo"]["extraEnv"]["CARGO_BUILD_JOBS"],
            "default"
        );
    }

    #[test]
    fn open_files_only_pauses_background_work() {
        let rust_analyzer = LanguageServerName::new_static("rust-analyzer");
        let mut settings = LspSettings::default();
        assert!(apply_mode(
            &rust_analyzer,
            AnalysisMode::OpenFilesOnly,
            &mut settings
        ));
        let options = settings.initialization_options.unwrap();
        assert_eq!(options["checkOnSave"], false);
        assert_eq!(options["cachePriming"]["enable"], false);

        let clangd = LanguageServerName::new_static("clangd");
        let mut settings = LspSettings::default();
        settings.binary = Some(settings::BinarySettings {
            arguments: Some(vec!["-j=2".into(), "--background-index".into()]),
            ..Default::default()
        });
        assert!(apply_mode(
            &clangd,
            AnalysisMode::OpenFilesOnly,
            &mut settings
        ));
        assert_eq!(
            settings.binary.unwrap().arguments.unwrap(),
            vec!["-j=2".to_string(), "--background-index=false".to_string()]
        );

        assert!(!apply_mode(
            &clangd,
            AnalysisMode::Off,
            &mut LspSettings::default()
        ));
    }

    #[test]
    fn modes_are_per_project_and_server() {
        let mut modes = LanguageServerModes::default();
        let rust_analyzer = LanguageServerName::new_static("rust-analyzer");
        modes.set(
            PathBuf::from("/a"),
            rust_analyzer.clone(),
            AnalysisMode::Off,
        );
        assert_eq!(
            modes.mode(Path::new("/a"), &rust_analyzer),
            AnalysisMode::Off
        );
        assert_eq!(
            modes.mode(Path::new("/b"), &rust_analyzer),
            AnalysisMode::Full
        );
        assert!(!modes.is_chosen(Path::new("/b"), &rust_analyzer));
        let json = serde_json::to_string(&modes).unwrap();
        assert_eq!(
            serde_json::from_str::<LanguageServerModes>(&json).unwrap(),
            modes
        );
    }
}
