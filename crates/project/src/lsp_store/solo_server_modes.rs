//! SOLO: how much each language server analyzes in a project, as chosen by
//! the user: everything, only the open files (no background checks or
//! indexing), or nothing (the server is off). Chosen per project and per
//! server, or for all of a project's servers at once; the UI persists the
//! choices and loads them into this global. Servers without a choice use the
//! `global_lsp_settings.default_analysis_mode` setting.
//!
//! The choice is applied to the server's settings when the language server
//! tree picks the servers for a file, so a change restarts or stops the
//! affected servers the same way a settings change does.

use std::path::{Path, PathBuf};

use collections::HashMap;
use gpui::{App, Global};
use lsp::LanguageServerName;
use serde::{Deserialize, Serialize};
use settings::{LanguageServerAnalysisMode, LspSettings, Settings as _};

use crate::project_settings::ProjectSettings;

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

impl From<LanguageServerAnalysisMode> for AnalysisMode {
    fn from(mode: LanguageServerAnalysisMode) -> Self {
        match mode {
            LanguageServerAnalysisMode::Full => Self::Full,
            LanguageServerAnalysisMode::OpenFilesOnly => Self::OpenFilesOnly,
            LanguageServerAnalysisMode::Off => Self::Off,
        }
    }
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

/// The analysis modes chosen per project root and server, and for all the
/// servers of a project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageServerModes {
    pub projects: HashMap<PathBuf, HashMap<LanguageServerName, AnalysisMode>>,
    /// The mode chosen for all of a project's servers, for the servers that
    /// start there later.
    #[serde(default)]
    pub project_modes: HashMap<PathBuf, AnalysisMode>,
}

impl Global for LanguageServerModes {}

impl LanguageServerModes {
    /// The mode of `server` in the project, given the default for projects
    /// without a choice.
    pub fn mode_or(
        &self,
        project_root: &Path,
        server: &LanguageServerName,
        default: AnalysisMode,
    ) -> AnalysisMode {
        self.chosen(project_root, server).unwrap_or(default)
    }

    /// The mode with the default `Full`.
    pub fn mode(&self, project_root: &Path, server: &LanguageServerName) -> AnalysisMode {
        self.mode_or(project_root, server, AnalysisMode::Full)
    }

    /// The mode chosen for the server in this project, or for all of the
    /// project's servers.
    pub fn chosen(&self, project_root: &Path, server: &LanguageServerName) -> Option<AnalysisMode> {
        self.projects
            .get(project_root)
            .and_then(|servers| servers.get(server))
            .or_else(|| self.project_modes.get(project_root))
            .copied()
    }

    /// Whether the user picked a mode for this server in this project.
    pub fn is_chosen(&self, project_root: &Path, server: &LanguageServerName) -> bool {
        self.chosen(project_root, server).is_some()
    }

    /// Sets the mode of all of a project's servers: the ones given, those
    /// with a choice there, and those that start there later.
    pub fn set_for_project(
        &mut self,
        project_root: PathBuf,
        servers: impl IntoIterator<Item = LanguageServerName>,
        mode: AnalysisMode,
    ) {
        let chosen = self.projects.entry(project_root.clone()).or_default();
        for server in servers {
            chosen.insert(server, mode);
        }
        for chosen_mode in chosen.values_mut() {
            *chosen_mode = mode;
        }
        self.project_modes.insert(project_root, mode);
    }

    /// The mode shared by the given servers of the given projects, or `None`
    /// when they differ. Without servers, the mode chosen for the projects
    /// (or the default).
    pub fn common_mode(
        &self,
        servers: &[(PathBuf, LanguageServerName)],
        project_roots: &[PathBuf],
        default: AnalysisMode,
    ) -> Option<AnalysisMode> {
        let mut modes: Vec<AnalysisMode> = servers
            .iter()
            .map(|(root, server)| self.mode_or(root, server, default))
            .collect();
        for root in project_roots {
            // Servers chosen in the project that aren't running, such as the
            // ones turned off.
            modes.extend(
                self.projects
                    .get(root)
                    .into_iter()
                    .flatten()
                    .map(|(_, mode)| *mode),
            );
        }
        if modes.is_empty() {
            modes.extend(
                project_roots
                    .iter()
                    .map(|root| self.project_modes.get(root).copied().unwrap_or(default)),
            );
        }
        let (first, rest) = modes.split_first()?;
        rest.iter().all(|mode| mode == first).then_some(*first)
    }

    pub fn set(&mut self, project_root: PathBuf, server: LanguageServerName, mode: AnalysisMode) {
        self.projects
            .entry(project_root)
            .or_default()
            .insert(server, mode);
    }
}

/// The mode for projects without a choice, from the settings.
pub fn default_mode(cx: &App) -> AnalysisMode {
    ProjectSettings::get_global(cx)
        .global_lsp_settings
        .default_analysis_mode
        .into()
}

pub fn mode_for(project_root: &Path, server: &LanguageServerName, cx: &App) -> AnalysisMode {
    cx.try_global::<LanguageServerModes>()
        .and_then(|modes| modes.chosen(project_root, server))
        .unwrap_or_else(|| default_mode(cx))
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

/// Sets the mode of all of a project's servers (see
/// [`LanguageServerModes::set_for_project`]).
pub fn set_project_mode(
    project_roots: &[PathBuf],
    servers: &[(PathBuf, LanguageServerName)],
    mode: AnalysisMode,
    cx: &mut App,
) {
    let modes = cx.default_global::<LanguageServerModes>();
    for project_root in project_roots {
        let project_servers = servers
            .iter()
            .filter(|(root, _)| root == project_root)
            .map(|(_, server)| server.clone());
        modes.set_for_project(project_root.clone(), project_servers, mode);
    }
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
        // Choices saved before project-wide modes existed still load.
        let old_json = r#"{"projects":{"/a":{"rust-analyzer":"off"}}}"#;
        assert_eq!(
            serde_json::from_str::<LanguageServerModes>(old_json)
                .unwrap()
                .mode(Path::new("/a"), &rust_analyzer),
            AnalysisMode::Off
        );
    }

    #[test]
    fn default_mode_applies_to_servers_without_a_choice() {
        let mut modes = LanguageServerModes::default();
        let rust_analyzer = LanguageServerName::new_static("rust-analyzer");
        let clangd = LanguageServerName::new_static("clangd");
        modes.set(
            PathBuf::from("/a"),
            rust_analyzer.clone(),
            AnalysisMode::Full,
        );
        let default = AnalysisMode::OpenFilesOnly;
        assert_eq!(
            modes.mode_or(Path::new("/a"), &rust_analyzer, default),
            AnalysisMode::Full
        );
        assert_eq!(
            modes.mode_or(Path::new("/a"), &clangd, default),
            AnalysisMode::OpenFilesOnly
        );
        assert_eq!(
            modes.mode_or(Path::new("/b"), &rust_analyzer, default),
            AnalysisMode::OpenFilesOnly
        );
    }

    #[test]
    fn project_mode_writes_every_servers_choice() {
        let mut modes = LanguageServerModes::default();
        let a = PathBuf::from("/a");
        let rust_analyzer = LanguageServerName::new_static("rust-analyzer");
        let json = LanguageServerName::new_static("json-language-server");
        let clangd = LanguageServerName::new_static("clangd");
        // clangd was turned off and isn't running.
        modes.set(a.clone(), clangd.clone(), AnalysisMode::Off);
        modes.set(a.clone(), json.clone(), AnalysisMode::Full);
        let running = vec![
            (a.clone(), rust_analyzer.clone()),
            (a.clone(), json.clone()),
        ];
        let roots = vec![a.clone()];
        let default = AnalysisMode::Full;

        // Differing modes have no common one.
        assert_eq!(modes.common_mode(&running, &roots, default), None);

        modes.set_for_project(
            a.clone(),
            running.iter().map(|(_, server)| server.clone()),
            AnalysisMode::OpenFilesOnly,
        );
        for server in [&rust_analyzer, &json, &clangd] {
            assert_eq!(
                modes.projects[&a].get(server),
                Some(&AnalysisMode::OpenFilesOnly)
            );
        }
        assert_eq!(
            modes.common_mode(&running, &roots, default),
            Some(AnalysisMode::OpenFilesOnly)
        );
        // A server that starts later uses the project's mode.
        let typos = LanguageServerName::new_static("typos");
        assert_eq!(
            modes.mode_or(&a, &typos, default),
            AnalysisMode::OpenFilesOnly
        );
        // Other projects aren't affected.
        assert_eq!(
            modes.mode_or(Path::new("/b"), &rust_analyzer, default),
            AnalysisMode::Full
        );

        // One server changed on its own makes them differ again.
        modes.set(a, json, AnalysisMode::Full);
        assert_eq!(modes.common_mode(&running, &roots, default), None);

        // Without servers, the project's mode or the default.
        let b = vec![PathBuf::from("/b")];
        assert_eq!(
            modes.common_mode(&[], &b, AnalysisMode::Off),
            Some(AnalysisMode::Off)
        );
    }
}
