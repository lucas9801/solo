//! SOLO: How well the bundled rules read the screens of the agents used most:
//! Claude Code, Codex, GitHub Copilot CLI, OpenCode and Gemini CLI, each
//! working, waiting for an approval or an answer, and idle.
//!
//! The screens are in `test_screens/<agent>/<state>--<what>.txt`: optional
//! `title: <terminal title>` lines, `---`, then the screen as Solo reads it.
//! They're written after the agents' interfaces (layout, spinners, footers),
//! not captured from a terminal: replace one with a real capture whenever an
//! agent changes and the rules misread it, and add the capture here.

use std::path::{Path, PathBuf};

use super::*;

/// Every agent here must have a screen of each of these.
const AGENTS: [&str; 5] = ["claude", "codex", "copilot", "opencode", "gemini"];

struct Screen {
    path: PathBuf,
    agent: String,
    expected: DetectedState,
    title: String,
    screen: String,
}

fn screens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("test_screens")
}

fn load(path: &Path, agent: &str) -> Screen {
    let name = path.file_stem().unwrap().to_string_lossy().into_owned();
    let expected = match name.split("--").next().unwrap() {
        "working" => DetectedState::Working,
        "blocked" => DetectedState::Blocked,
        "idle" => DetectedState::Idle,
        other => panic!("{}: unknown state {other:?}", path.display()),
    };
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let (header, screen) = text
        .split_once("---\n")
        .unwrap_or_else(|| panic!("{}: no `---` line", path.display()));
    let title = header
        .lines()
        .find_map(|line| line.strip_prefix("title: "))
        .unwrap_or_default()
        .to_string();
    // As the terminal gives it: no trailing blank lines.
    let screen = screen.trim_end_matches(['\n', ' ']).to_string();
    Screen {
        path: path.to_path_buf(),
        agent: agent.to_string(),
        expected,
        title,
        screen,
    }
}

fn all_screens() -> Vec<Screen> {
    let mut screens = Vec::new();
    for agent in std::fs::read_dir(screens_dir()).unwrap().flatten() {
        let name = agent.file_name().to_string_lossy().into_owned();
        let mut files: Vec<PathBuf> = std::fs::read_dir(agent.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
            .collect();
        files.sort();
        screens.extend(files.iter().map(|path| load(path, &name)));
    }
    screens
}

#[test]
fn bundled_rules_read_common_agents_screens() {
    let screens = all_screens();
    let mut wrong = Vec::new();
    for screen in &screens {
        let manifest = bundled()
            .iter()
            .find(|manifest| manifest.is_for(&screen.agent))
            .unwrap_or_else(|| panic!("no rules for {}", screen.agent));
        let detection = manifest.detect(&DetectionInput {
            screen: &screen.screen,
            osc_title: &screen.title,
            osc_progress: "",
        });
        if detection.state != screen.expected || detection.skip_state_update {
            wrong.push(format!(
                "{}: expected {:?}, read {:?} (rule {:?})",
                screen.path.display(),
                screen.expected,
                detection.state,
                detection.rule
            ));
        }
    }
    assert!(wrong.is_empty(), "misread screens:\n{}", wrong.join("\n"));
}

#[test]
fn common_agents_have_screens_of_each_state() {
    let screens = all_screens();
    for agent in AGENTS {
        for (state, what) in [
            (DetectedState::Working, "working"),
            (DetectedState::Idle, "idle"),
        ] {
            assert!(
                screens
                    .iter()
                    .any(|screen| screen.agent == agent && screen.expected == state),
                "{agent} has no {what} screen"
            );
        }
        let blocked: Vec<_> = screens
            .iter()
            .filter(|screen| screen.agent == agent && screen.expected == DetectedState::Blocked)
            .collect();
        // An approval and a question (or, for Gemini CLI, whose rules know
        // no question, two kinds of approval).
        assert!(
            blocked.len() >= 2,
            "{agent} needs an approval and a question screen"
        );
    }
}
