//! SOLO: what an AI agent in a terminal is doing, read from its screen: rules
//! per agent match text in regions of the screen (the bottom lines, the
//! terminal title, the agent's prompt box, ...) and say whether it's working,
//! waiting for the user (blocked) or idle.
//!
//! The rules are Herdr's (vendor/herdr_agent_detection, Apache-2.0,
//! https://github.com/herdrdev/herdr at ee5bfb5041bb3e4a49d591c72308aae3c81ecdde),
//! and so is how they're evaluated: this is a port of Herdr's
//! `src/detect/manifest.rs` (Copyright Herdr contributors, Apache License 2.0),
//! changed to fit Solo: no fetching rules from Herdr's server; rules a user
//! puts in a folder replace the bundled ones.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use regex::Regex;
use serde::Deserialize;

/// The version of the rule language this engine understands.
pub const ENGINE_VERSION: u32 = 3;

const MAX_RULES: usize = 128;
const MAX_GATE_DEPTH: usize = 8;
const MAX_GATES: usize = 512;
const MAX_MATCHERS_PER_GATE: usize = 32;
const MAX_MATCHERS: usize = 1024;
const MAX_MATCHER_CHARS: usize = 512;
const MAX_TOP_LINES: usize = 65535;

/// The rules bundled with Solo, by file.
const BUNDLED: &[(&str, &str)] = &[
    (
        "amp.toml",
        include_str!("../../../vendor/herdr_agent_detection/amp.toml"),
    ),
    (
        "antigravity.toml",
        include_str!("../../../vendor/herdr_agent_detection/antigravity.toml"),
    ),
    (
        "claude.toml",
        include_str!("../../../vendor/herdr_agent_detection/claude.toml"),
    ),
    (
        "cline.toml",
        include_str!("../../../vendor/herdr_agent_detection/cline.toml"),
    ),
    (
        "codex.toml",
        include_str!("../../../vendor/herdr_agent_detection/codex.toml"),
    ),
    (
        "cursor.toml",
        include_str!("../../../vendor/herdr_agent_detection/cursor.toml"),
    ),
    (
        "devin.toml",
        include_str!("../../../vendor/herdr_agent_detection/devin.toml"),
    ),
    (
        "droid.toml",
        include_str!("../../../vendor/herdr_agent_detection/droid.toml"),
    ),
    (
        "gemini.toml",
        include_str!("../../../vendor/herdr_agent_detection/gemini.toml"),
    ),
    (
        "github-copilot.toml",
        include_str!("../../../vendor/herdr_agent_detection/github-copilot.toml"),
    ),
    (
        "grok.toml",
        include_str!("../../../vendor/herdr_agent_detection/grok.toml"),
    ),
    (
        "hermes.toml",
        include_str!("../../../vendor/herdr_agent_detection/hermes.toml"),
    ),
    (
        "kilo.toml",
        include_str!("../../../vendor/herdr_agent_detection/kilo.toml"),
    ),
    (
        "kimi.toml",
        include_str!("../../../vendor/herdr_agent_detection/kimi.toml"),
    ),
    (
        "kiro.toml",
        include_str!("../../../vendor/herdr_agent_detection/kiro.toml"),
    ),
    (
        "letta.toml",
        include_str!("../../../vendor/herdr_agent_detection/letta.toml"),
    ),
    (
        "maki.toml",
        include_str!("../../../vendor/herdr_agent_detection/maki.toml"),
    ),
    (
        "muse.toml",
        include_str!("../../../vendor/herdr_agent_detection/muse.toml"),
    ),
    (
        "opencode.toml",
        include_str!("../../../vendor/herdr_agent_detection/opencode.toml"),
    ),
    (
        "pi.toml",
        include_str!("../../../vendor/herdr_agent_detection/pi.toml"),
    ),
    (
        "qodercli.toml",
        include_str!("../../../vendor/herdr_agent_detection/qodercli.toml"),
    ),
    (
        "qwen.toml",
        include_str!("../../../vendor/herdr_agent_detection/qwen.toml"),
    ),
];

/// What the screen says the agent is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectedState {
    Idle,
    Working,
    /// Waiting for the user: a permission, a question, a choice.
    Blocked,
    Unknown,
}

/// The result of reading a screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub state: DetectedState,
    /// The screen shows something (a transcript viewer, a menu) that says
    /// nothing about the agent's state: keep the last one.
    pub skip_state_update: bool,
    /// Whether the state is plainly visible, not only inferred: an idle
    /// prompt, a question, a spinner.
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    /// The rule that decided, if one matched.
    pub rule: Option<String>,
}

/// What the rules read: the screen's text (lines joined with `\n`, no
/// trailing blank lines), the terminal's title (OSC 0/2) and its last
/// progress report (the payload of OSC 9).
#[derive(Clone, Copy, Debug, Default)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    id: String,
    #[serde(default)]
    #[allow(dead_code, reason = "parsed to validate it")]
    version: Option<String>,
    #[serde(default)]
    min_engine_version: Option<u32>,
    #[serde(default)]
    #[allow(dead_code, reason = "parsed to validate it")]
    updated_at: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<RuleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    id: String,
    #[serde(default)]
    state: Option<StateName>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    // The rule's own gate (serde's `flatten` doesn't go with
    // `deny_unknown_fields`).
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
    #[serde(default)]
    all: Vec<GateFile>,
    #[serde(default)]
    any: Vec<GateFile>,
    #[serde(default)]
    not: Vec<GateFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateFile {
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
    #[serde(default)]
    all: Vec<GateFile>,
    #[serde(default)]
    any: Vec<GateFile>,
    #[serde(default)]
    not: Vec<GateFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StateName {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl From<StateName> for DetectedState {
    fn from(state: StateName) -> Self {
        match state {
            StateName::Idle => DetectedState::Idle,
            StateName::Working => DetectedState::Working,
            StateName::Blocked => DetectedState::Blocked,
            StateName::Unknown => DetectedState::Unknown,
        }
    }
}

/// A part of the screen a rule looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    WholeRecent,
    BottomLines(usize),
    BottomNonEmptyLines(usize),
    TopNonEmptyLines(usize),
    AfterLastPromptMarker,
    BeforeCurrentPromptMarker,
    WholeRecentWithoutCurrentPromptMarker,
    CurrentPromptBlockMarker,
    AfterCurrentPromptBlockMarker,
    PromptBoxBody,
    AbovePromptBox,
    LastNonEmptyAbovePromptBox,
    AfterLastHorizontalRule,
    OscTitle,
    OscProgress,
}

/// `n` in `name(n)`: a positive decimal without a leading zero.
fn region_count(text: &str, name: &str) -> Option<Result<usize>> {
    let count = text
        .strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')?;
    let valid =
        !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()) && !count.starts_with('0');
    Some(
        count
            .parse::<usize>()
            .ok()
            .filter(|_| valid)
            .ok_or_else(|| anyhow!("{name}({count}) needs a positive count")),
    )
}

impl Region {
    fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        Ok(match text {
            "whole_recent" => Region::WholeRecent,
            "after_last_prompt_marker" => Region::AfterLastPromptMarker,
            "before_current_prompt_marker" => Region::BeforeCurrentPromptMarker,
            "whole_recent_without_current_prompt_marker" => {
                Region::WholeRecentWithoutCurrentPromptMarker
            }
            "current_prompt_block_marker" => Region::CurrentPromptBlockMarker,
            "after_current_prompt_block_marker" => Region::AfterCurrentPromptBlockMarker,
            "prompt_box_body" => Region::PromptBoxBody,
            "above_prompt_box" => Region::AbovePromptBox,
            "last_non_empty_above_prompt_box" => Region::LastNonEmptyAbovePromptBox,
            "after_last_horizontal_rule" => Region::AfterLastHorizontalRule,
            "osc_title" => Region::OscTitle,
            "osc_progress" => Region::OscProgress,
            _ => {
                if let Some(count) = region_count(text, "bottom_lines") {
                    Region::BottomLines(count?)
                } else if let Some(count) = region_count(text, "bottom_non_empty_lines") {
                    Region::BottomNonEmptyLines(count?)
                } else if let Some(count) = region_count(text, "top_non_empty_lines") {
                    let count = count?;
                    ensure!(
                        count <= MAX_TOP_LINES,
                        "top_non_empty_lines({count}) is too many"
                    );
                    Region::TopNonEmptyLines(count)
                } else {
                    bail!("unknown region {text:?}");
                }
            }
        })
    }

    fn text<'a>(self, input: &DetectionInput<'a>) -> &'a str {
        let screen = input.screen;
        match self {
            Region::WholeRecent => screen,
            Region::BottomLines(count) => bottom_lines(screen, count),
            Region::BottomNonEmptyLines(count) => bottom_non_empty_lines(screen, count),
            Region::TopNonEmptyLines(count) => top_non_empty_lines(screen, count),
            Region::AfterLastPromptMarker => after_last_prompt_marker(screen),
            Region::BeforeCurrentPromptMarker => before_current_prompt_marker(screen),
            Region::WholeRecentWithoutCurrentPromptMarker => {
                let lines: Vec<&str> = screen.lines().collect();
                if current_prompt_index(&lines).is_some() {
                    ""
                } else {
                    screen
                }
            }
            Region::CurrentPromptBlockMarker => current_prompt_block_marker(screen),
            Region::AfterCurrentPromptBlockMarker => after_current_prompt_block_marker(screen),
            Region::PromptBoxBody => prompt_box_body(screen),
            Region::AbovePromptBox => above_prompt_box(screen),
            Region::LastNonEmptyAbovePromptBox => last_non_empty_line(above_prompt_box(screen)),
            Region::AfterLastHorizontalRule => after_last_horizontal_rule(screen),
            Region::OscTitle => input.osc_title,
            Region::OscProgress => input.osc_progress,
        }
    }
}

#[derive(Debug)]
struct Gate {
    /// Lowercased: matched without regard to case.
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
    all: Vec<Gate>,
    any: Vec<Gate>,
    not: Vec<Gate>,
}

impl Gate {
    fn matches(&self, text: &str, lower_text: &str) -> bool {
        self.contains
            .iter()
            .all(|needle| lower_text.contains(needle))
            && self.regex.iter().all(|regex| regex.is_match(text))
            && self
                .line_regex
                .iter()
                .all(|regex| text.lines().any(|line| regex.is_match(line)))
            && self.all.iter().all(|gate| gate.matches(text, lower_text))
            && (self.any.is_empty() || self.any.iter().any(|gate| gate.matches(text, lower_text)))
            && !self.not.iter().any(|gate| gate.matches(text, lower_text))
    }
}

#[derive(Debug)]
struct Rule {
    id: String,
    state: DetectedState,
    priority: i32,
    region: Region,
    visible_idle: bool,
    visible_blocker: bool,
    visible_working: bool,
    skip_state_update: bool,
    gate: Gate,
}

/// Counts what a manifest holds, against the limits.
#[derive(Default)]
struct Budget {
    gates: usize,
    matchers: usize,
}

fn compile_gate(gate: GateFile, depth: usize, negated: bool, budget: &mut Budget) -> Result<Gate> {
    ensure!(
        depth <= MAX_GATE_DEPTH,
        "gates nest deeper than {MAX_GATE_DEPTH}"
    );
    budget.gates += 1;
    ensure!(budget.gates <= MAX_GATES, "more than {MAX_GATES} gates");
    let direct = gate.contains.len() + gate.regex.len() + gate.line_regex.len();
    ensure!(
        direct <= MAX_MATCHERS_PER_GATE,
        "a gate has more than {MAX_MATCHERS_PER_GATE} matchers"
    );
    budget.matchers += direct;
    ensure!(
        budget.matchers <= MAX_MATCHERS,
        "more than {MAX_MATCHERS} matchers"
    );
    let positive = direct > 0 || !gate.all.is_empty() || !gate.any.is_empty();
    if negated {
        ensure!(positive || !gate.not.is_empty(), "an empty gate in `not`");
    } else {
        ensure!(positive, "a gate matches nothing of its own (only `not`)");
    }
    for matcher in gate
        .contains
        .iter()
        .chain(&gate.regex)
        .chain(&gate.line_regex)
    {
        ensure!(
            matcher.chars().count() <= MAX_MATCHER_CHARS,
            "a matcher is longer than {MAX_MATCHER_CHARS} characters"
        );
    }
    let compile = |patterns: Vec<String>| -> Result<Vec<Regex>> {
        patterns
            .iter()
            .map(|pattern| Regex::new(pattern).with_context(|| format!("regex {pattern:?}")))
            .collect()
    };
    let nested = |gates: Vec<GateFile>, negated: bool, budget: &mut Budget| -> Result<Vec<Gate>> {
        gates
            .into_iter()
            .map(|gate| compile_gate(gate, depth + 1, negated, budget))
            .collect()
    };
    Ok(Gate {
        contains: gate
            .contains
            .iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: compile(gate.regex)?,
        line_regex: compile(gate.line_regex)?,
        all: nested(gate.all, false, budget)?,
        any: nested(gate.any, false, budget)?,
        not: nested(gate.not, true, budget)?,
    })
}

/// One agent's rules, checked and compiled.
#[derive(Debug)]
pub struct Manifest {
    pub id: String,
    pub aliases: Vec<String>,
    rules: Vec<Rule>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let file: ManifestFile = toml::from_str(text).context("parsing the rules")?;
        ensure!(!file.id.trim().is_empty(), "the rules have no `id`");
        if let Some(version) = file.min_engine_version {
            ensure!(
                version <= ENGINE_VERSION,
                "the rules need engine version {version}; this is {ENGINE_VERSION}"
            );
        }
        ensure!(!file.rules.is_empty(), "no rules");
        ensure!(file.rules.len() <= MAX_RULES, "more than {MAX_RULES} rules");
        let mut budget = Budget::default();
        let mut rules = Vec::with_capacity(file.rules.len());
        for rule in file.rules {
            let id = rule.id.clone();
            rules.push(
                compile_rule(rule, file.min_engine_version, &mut budget)
                    .with_context(|| format!("rule {id:?}"))?,
            );
        }
        Ok(Self {
            id: file.id,
            aliases: file.aliases,
            rules,
        })
    }

    /// Whether this is the agent called `name` (its id or an alias, any case).
    pub fn is_for(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        self.id.to_lowercase() == name
            || self
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase() == name)
    }

    /// What the screen says: the matching rule with the highest priority
    /// decides (the earliest on a tie); with none, the agent is idle.
    pub fn detect(&self, input: &DetectionInput) -> Detection {
        let mut matched: Option<&Rule> = None;
        for rule in &self.rules {
            let text = rule.region.text(input);
            if !rule.gate.matches(text, &text.to_lowercase()) {
                continue;
            }
            if matched.is_none_or(|previous| previous.priority < rule.priority) {
                matched = Some(rule);
            }
        }
        let Some(rule) = matched else {
            return Detection {
                state: DetectedState::Idle,
                skip_state_update: false,
                visible_idle: false,
                visible_blocker: false,
                visible_working: false,
                rule: None,
            };
        };
        Detection {
            state: rule.state,
            skip_state_update: rule.skip_state_update,
            visible_idle: rule.visible_idle && rule.state == DetectedState::Idle,
            visible_blocker: rule.visible_blocker && rule.state == DetectedState::Blocked,
            visible_working: rule.visible_working && rule.state == DetectedState::Working,
            rule: Some(rule.id.clone()),
        }
    }
}

fn compile_rule(
    rule: RuleFile,
    min_engine_version: Option<u32>,
    budget: &mut Budget,
) -> Result<Rule> {
    ensure!(!rule.id.trim().is_empty(), "a rule has no `id`");
    let region = Region::parse(rule.region.as_deref().unwrap_or("whole_recent"))?;
    if matches!(region, Region::TopNonEmptyLines(_))
        && let Some(version) = min_engine_version
    {
        ensure!(
            version >= 3,
            "top_non_empty_lines needs min_engine_version 3"
        );
    }
    let state = rule
        .state
        .map(DetectedState::from)
        .unwrap_or(DetectedState::Unknown);
    if rule.skip_state_update {
        ensure!(
            state == DetectedState::Unknown
                && !rule.visible_idle
                && !rule.visible_blocker
                && !rule.visible_working,
            "a rule that skips the state update can't give a state"
        );
    }
    Ok(Rule {
        id: rule.id,
        state,
        priority: rule.priority,
        region,
        visible_idle: rule.visible_idle,
        visible_blocker: rule.visible_blocker,
        visible_working: rule.visible_working,
        skip_state_update: rule.skip_state_update,
        gate: compile_gate(
            GateFile {
                contains: rule.contains,
                regex: rule.regex,
                line_regex: rule.line_regex,
                all: rule.all,
                any: rule.any,
                not: rule.not,
            },
            1,
            false,
            budget,
        )?,
    })
}

/// The bundled rules, compiled once.
pub fn bundled() -> &'static [Manifest] {
    static BUNDLED_MANIFESTS: OnceLock<Vec<Manifest>> = OnceLock::new();
    BUNDLED_MANIFESTS.get_or_init(|| {
        BUNDLED
            .iter()
            .filter_map(|(file, text)| match Manifest::parse(text) {
                Ok(manifest) => Some(manifest),
                Err(error) => {
                    // Checked by a test; never expected.
                    eprintln!("bundled agent detection rules {file} are invalid: {error:#}");
                    None
                }
            })
            .collect()
    })
}

/// The rules for each agent: the bundled ones, replaced by any in `directory`
/// (`<agent>.toml`, for the agent of the same id). Invalid files are skipped
/// and reported.
pub fn load_with_overrides(directory: &Path) -> (HashMap<String, Manifest>, Vec<String>) {
    let mut manifests: HashMap<String, Manifest> = BUNDLED
        .iter()
        .filter_map(|(_, text)| Manifest::parse(text).ok())
        .map(|manifest| (manifest.id.clone(), manifest))
        .collect();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return (manifests, errors);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .context("reading it")
            .and_then(|text| Manifest::parse(&text));
        match parsed {
            Ok(manifest) => {
                manifests.insert(manifest.id.clone(), manifest);
            }
            Err(error) => errors.push(format!("{}: {error:#}", path.display())),
        }
    }
    (manifests, errors)
}

// Regions of the screen. Lines end with `\n`; the slices keep them.

fn line_start(lines: &[&str], index: usize) -> usize {
    lines[..index].iter().map(|line| line.len() + 1).sum()
}

fn slice_from_line<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    &content[line_start(lines, index).min(content.len())..]
}

fn slice_to_line<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    &content[..line_start(lines, index).min(content.len())]
}

fn bottom_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    slice_from_line(content, &lines, lines.len().saturating_sub(count))
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line(content, &lines, start)
}

fn top_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .nth(count - 1)
    {
        Some((index, _)) => slice_to_line(content, &lines, index + 1),
        None => content,
    }
}

/// A line of Codex's prompt.
fn prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

/// A line that starts a block of Codex's output.
fn block_marker_line(line: &str) -> bool {
    line.starts_with(['•', '■', '✗', '✓'])
}

/// The prompt being typed: the last prompt line, with no block after it.
fn current_prompt_index(lines: &[&str]) -> Option<usize> {
    let index = lines.iter().rposition(|line| prompt_line(line))?;
    if lines[index + 1..]
        .iter()
        .any(|line| block_marker_line(line))
    {
        return None;
    }
    Some(index)
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match lines.iter().rposition(|line| prompt_line(line)) {
        Some(index) => slice_from_line(content, &lines, index + 1),
        None => content,
    }
}

fn before_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match current_prompt_index(&lines) {
        Some(index) => slice_to_line(content, &lines, index),
        None => content,
    }
}

fn current_prompt_block_marker_index(lines: &[&str]) -> Option<usize> {
    let prompt = current_prompt_index(lines)?;
    lines[..prompt]
        .iter()
        .rposition(|line| block_marker_line(line))
}

fn current_prompt_block_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match current_prompt_block_marker_index(&lines) {
        Some(index) => lines[index],
        None => "",
    }
}

fn after_current_prompt_block_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match current_prompt_block_marker_index(&lines) {
        Some(index) => slice_from_line(content, &lines, index),
        None => "",
    }
}

/// A line drawn with `─`, possibly with a title after three or more.
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    let rule_chars = trimmed.chars().take_while(|&c| c == '─').count();
    if rule_chars == 0 {
        return false;
    }
    let suffix = trimmed[rule_chars * '─'.len_utf8()..].trim_start();
    suffix.is_empty() || rule_chars >= 3
}

fn horizontal_rules(lines: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_horizontal_rule(line))
        .map(|(index, _)| index)
        .collect()
}

/// The top border of the prompt box: the next to last rule.
fn prompt_box_top(lines: &[&str]) -> Option<usize> {
    let rules = horizontal_rules(lines);
    rules.len().checked_sub(2).map(|index| rules[index])
}

fn prompt_box_body(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top) = prompt_box_top(&lines) else {
        return "";
    };
    let bottom = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map(|offset| top + 1 + offset)
        .unwrap_or(lines.len());
    let start = line_start(&lines, top + 1).min(content.len());
    let end = line_start(&lines, bottom).min(content.len());
    &content[start..end]
}

fn above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match prompt_box_top(&lines) {
        Some(top) => slice_to_line(content, &lines, top),
        None => content,
    }
}

fn last_non_empty_line(content: &str) -> &str {
    content
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    match lines.iter().rposition(|line| is_horizontal_rule(line)) {
        Some(index) => slice_from_line(content, &lines, index + 1),
        None => content,
    }
}

#[cfg(test)]
mod tests;
