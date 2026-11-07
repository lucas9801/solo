use std::path::PathBuf;

use collections::{HashMap, HashSet};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

use crate::{FontFamilyName, FontFeaturesContent, FontSize, FontWeightContent};

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct ProjectTerminalSettingsContent {
    /// What shell to use when opening a terminal.
    ///
    /// Default: system
    pub shell: Option<Shell>,
    /// What working directory to use when launching the terminal
    ///
    /// Default: current_project_directory
    pub working_directory: Option<WorkingDirectory>,
    /// Any key-value pairs added to this list will be added to the terminal's
    /// environment. Use `:` to separate multiple values.
    ///
    /// Default: {}
    pub env: Option<HashMap<String, String>>,
    /// Activates the python virtual environment, if one is found, in the
    /// terminal's working directory (as resolved by the working_directory
    /// setting). Set this to "off" to disable this behavior.
    ///
    /// Default: on
    pub detect_venv: Option<VenvSettings>,
    /// Whether to load Solo's shell integration into supported shells
    /// (PowerShell and bash) that run without custom arguments. It marks each
    /// command so the terminal can show command blocks.
    ///
    /// Default: true
    pub shell_integration: Option<bool>,
    /// Regexes used to identify paths for hyperlink navigation.
    ///
    /// Default:
    ///
    /// ```json
    /// {
    ///   "terminal": {
    ///     "path_hyperlink_regexes": [
    ///       // Python-style diagnostics
    ///       "File \"(?<path>[^\"]+)\", line (?<line>[0-9]+)",
    ///       // Common path syntax with optional line, column, description, trailing punctuation, or
    ///       // surrounding symbols or quotes
    ///       [
    ///         "(?x)",
    ///         "(?<path>",
    ///         "    (",
    ///         "        # multi-char path: first char (not opening delimiter, space, or box drawing char)",
    ///         "        [^({\\[<\"'`\\ \\u2500-\\u257F]",
    ///         "        # middle chars: non-space, and colon/paren only if not followed by digit/paren/space",
    ///         "        ([^\\ :(]|[:(][^0-9()\\ ])*",
    ///         "        # last char: not closing delimiter or colon",
    ///         "        [^()}\\]>\"'`.,;:\\ ]",
    ///         "    |",
    ///         "        # single-char path: not delimiter, punctuation, space, or box drawing char",
    ///         "        [^(){}\\[\\]<>\"'`.,;:\\ \\u2500-\\u257F]",
    ///         "    )",
    ///         "    # optional line/column suffix (included in path for PathWithPosition::parse_str)",
    ///         "    (:+[0-9]+(:[0-9]+)?|:?\\([0-9]+([,:]?[0-9]+)?\\))?",
    ///         ")"
    ///       ]
    ///     ]
    ///   }
    /// }
    /// ```
    ///
    /// Use `"..."` to add regexes without repeating Solo's defaults. In project
    /// settings, it extends the user or parent configuration value. Omit
    /// `"..."` to replace the inherited list. Set `[]` to clear it. Omitting this
    /// setting keeps the inherited list.
    ///
    /// ```json
    /// {
    ///   "terminal": {
    ///     "path_hyperlink_regexes": [
    ///       "\\s+(-->|:::|at) (?<link>(?<path>.+?))(:$|$)",
    ///       "\\s+(Compiling|Checking|Documenting) [^(]+\\((?<link>(?<path>.+))\\)",
    ///       "..."
    ///     ]
    ///   }
    /// }
    /// ```
    ///
    /// Inherited regexes are inserted at `"..."`, and duplicates keep their first
    /// occurrence. Regexes are duplicates when their text is identical after
    /// joining multiline entries with newlines.
    ///
    /// Each regex can be a single string or an array of strings joined with
    /// newlines. The marker is recognized only as a top-level string. To use
    /// `...` as a regex matching three characters, write `["..."]` as an entry.
    /// A `"..."` line inside a multiline entry is always regex text.
    ///
    /// The optional named capture `path` selects the hyperlink target. Without
    /// it, the entire match is the target. With `path`, `line` and `column`
    /// specify the position. Without a captured `line`, built-in suffix
    /// processing parses `line:column` and `(line,column)` variants. The optional
    /// `link` capture selects the clickable text, otherwise the entire match is
    /// clickable.
    ///
    /// Processing stops at the first regex that matches the terminal line, even
    /// if the cursor is outside its clickable text. Put more specific regexes
    /// before broader ones. Regexes use Rust's `regex` syntax. Invalid regexes
    /// are logged and ignored. The `path_hyperlink_timeout_ms` setting controls
    /// the discovery timeout. Setting it to `0` disables path hyperlinks.
    pub path_hyperlink_regexes: Option<PathHyperlinkRegexes>,
    /// Timeout for hover and Cmd-click path hyperlink discovery in milliseconds.
    ///
    /// Default: 1
    pub path_hyperlink_timeout_ms: Option<u64>,
}

#[with_fallible_options]
#[derive(Clone, Debug, PartialEq, Default, Serialize, JsonSchema, MergeFrom)]
pub struct TerminalSettingsContent {
    #[serde(flatten)]
    pub project: ProjectTerminalSettingsContent,
    /// Sets the terminal's font size.
    ///
    /// If this option is not included,
    /// the terminal will default to matching the buffer's font size.
    pub font_size: Option<FontSize>,
    /// Sets the terminal's font family.
    ///
    /// If this option is not included,
    /// the terminal will default to matching the buffer's font family.
    pub font_family: Option<FontFamilyName>,

    /// Sets the terminal's font fallbacks.
    ///
    /// If this option is not included,
    /// the terminal will default to matching the buffer's font fallbacks.
    #[schemars(extend("uniqueItems" = true))]
    pub font_fallbacks: Option<Vec<FontFamilyName>>,

    /// Sets the terminal's line height.
    ///
    /// Default: comfortable
    pub line_height: Option<TerminalLineHeight>,
    pub font_features: Option<FontFeaturesContent>,
    /// Sets the terminal's font weight in CSS weight units 0-900.
    pub font_weight: Option<FontWeightContent>,
    /// Default cursor shape for the terminal.
    /// Can be "bar", "block", "underline", or "hollow".
    ///
    /// Default: "block"
    pub cursor_shape: Option<CursorShapeContent>,
    /// Sets the cursor blinking behavior in the terminal.
    ///
    /// Default: terminal_controlled
    pub blinking: Option<TerminalBlink>,
    /// Sets whether Alternate Scroll mode (code: ?1007) is active by default.
    /// Alternate Scroll mode converts mouse scroll events into up / down key
    /// presses when in the alternate screen (e.g. when running applications
    /// like vim or  less). The terminal can still set and unset this mode.
    ///
    /// Default: on
    pub alternate_scroll: Option<AlternateScroll>,
    /// Sets whether the option key behaves as the meta key.
    ///
    /// Default: false
    pub option_as_meta: Option<bool>,
    /// Whether or not selecting text in the terminal will automatically
    /// copy to the system clipboard.
    ///
    /// Default: false
    pub copy_on_select: Option<bool>,
    /// Whether to keep the text selection after copying it to the clipboard.
    ///
    /// Default: true
    pub keep_selection_on_copy: Option<bool>,
    /// Whether cmd-click (ctrl-click on Linux and Windows) opens hyperlinks even
    /// when the terminal application has enabled mouse reporting (e.g. vim with
    /// mouse=a, htop). When false, these clicks are forwarded to the application
    /// instead, and hyperlinks can still be opened with shift-cmd-click
    /// (shift-ctrl-click).
    ///
    /// Default: true
    pub open_links_in_mouse_mode: Option<bool>,
    /// Whether to show the terminal button in the status bar.
    ///
    /// Default: true
    pub button: Option<bool>,
    pub dock: Option<TerminalDockPosition>,
    /// Whether the terminal panel should open on startup.
    ///
    /// Default: false
    pub starts_open: Option<bool>,
    /// Whether the terminal panel should use flexible (proportional) sizing.
    ///
    /// Default: true
    pub flexible: Option<bool>,
    /// Default width when the terminal is docked to the left or right.
    ///
    /// Default: 640
    pub default_width: Option<crate::PixelSetting>,
    /// Default height when the terminal is docked to the bottom.
    ///
    /// Default: 320
    pub default_height: Option<crate::PixelSetting>,
    /// The maximum number of lines to keep in the scrollback history.
    /// Maximum allowed value is 100_000, all values above that will be treated as 100_000.
    /// 0 disables the scrolling.
    /// Existing terminals will not pick up this change until they are recreated.
    /// See <a href="https://github.com/alacritty/alacritty/blob/cb3a79dbf6472740daca8440d5166c1d4af5029e/extra/man/alacritty.5.scd?plain=1#L207-L213">Alacritty documentation</a> for more information.
    ///
    /// Default: 10_000
    pub max_scroll_history_lines: Option<usize>,
    /// The multiplier for scrolling with the mouse wheel.
    ///
    /// Default: 1.0
    pub scroll_multiplier: Option<f32>,
    /// Toolbar related settings
    pub toolbar: Option<TerminalToolbarContent>,
    /// Scrollbar-related settings
    pub scrollbar: Option<ScrollbarSettingsContent>,
    /// The minimum APCA perceptual contrast between foreground and background colors.
    ///
    /// APCA (Accessible Perceptual Contrast Algorithm) is more accurate than WCAG 2.x,
    /// especially for dark mode. Values range from 0 to 106.
    ///
    /// Based on APCA Readability Criterion (ARC) Bronze Simple Mode:
    /// https://readtech.org/ARC/tests/bronze-simple-mode/
    /// - 0: No contrast adjustment
    /// - 45: Minimum for large fluent text (36px+)
    /// - 60: Minimum for other content text
    /// - 75: Minimum for body text
    /// - 90: Preferred for body text
    ///
    /// Default: 45
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub minimum_contrast: Option<f32>,
    /// Whether to show a badge on the terminal panel icon with the count of open terminals.
    ///
    /// Default: false
    pub show_count_badge: Option<bool>,
    /// What to do when the `BEL` character (`\a`) is printed to terminal.
    ///
    /// Default: "system"
    pub bell: Option<TerminalBell>,
    /// Whether to open files in the editor when they change on disk while a
    /// command runs in the focused terminal, e.g. files edited by an AI agent.
    /// Needs shell integration to know when a command runs.
    ///
    /// Default: true
    pub open_changed_files: Option<bool>,
    /// Whether to type commands in an editor at the bottom of the terminal
    /// while the shell waits at its prompt, with completions and history
    /// suggestions. Needs shell integration.
    ///
    /// Default: true
    pub input_editor: Option<bool>,
    /// Whether shells with Solo's integration show a compact prompt (the
    /// directory name and an arrow) while the input editor is on, since the
    /// input editor already shows the full path and git branch.
    ///
    /// Default: true
    pub compact_prompt: Option<bool>,
    /// Whether the input editor opens completions as you type, rather than
    /// only on Tab.
    ///
    /// Default: false
    pub completions_as_you_type: Option<bool>,
    /// Whether file paths and URLs in the terminal are underlined on hover
    /// and open with a plain click, rather than only with ctrl/cmd held.
    ///
    /// Default: true
    pub open_links_on_click: Option<bool>,
    /// Show a desktop notification when a command that ran at least this many
    /// seconds finishes while Solo is in the background. 0 turns it off.
    ///
    /// Default: 10
    pub notify_long_commands_after_seconds: Option<u64>,
    /// Whether programs in the terminal can show desktop notifications (with
    /// OSC 9 or OSC 777) while Solo is in the background, e.g. Claude Code
    /// when it finishes a turn.
    ///
    /// Default: true
    pub program_notifications: Option<bool>,
    /// Whether terminals show their previous contents and command blocks
    /// again when Solo restarts. The contents are saved in Solo's data
    /// directory. Terminals are reopened either way.
    ///
    /// Default: false
    pub restore_contents: Option<bool>,
    /// Programs to treat as AI agents, besides the built-in ones (claude,
    /// codex, gemini, aider, ...): their tab shows whether they're working,
    /// waiting for you, done or failed.
    ///
    /// Default: []
    pub agent_commands: Option<Vec<String>>,
    /// Whether Claude Code started in a terminal with shell integration
    /// reports what it's doing (working, waiting for you, idle) through hooks
    /// Solo adds for that session with `claude --settings`, instead of Solo
    /// guessing from its output. Your own settings and hooks still apply.
    /// Takes effect in new terminals.
    ///
    /// Default: true
    pub claude_code_hooks: Option<bool>,
    /// When to ask before closing a terminal (its tab, its window or Solo)
    /// would end the program running in it.
    ///
    /// Default: "agents"
    pub confirm_close: Option<TerminalConfirmClose>,
    /// Whether the project panel, file finder and project search follow the
    /// active terminal's working directory: they narrow to the git repository
    /// it's in (or the directory itself outside a repository), switching when
    /// another terminal becomes active or the shell shows its prompt in a new
    /// directory.
    ///
    /// Default: true
    pub follow_working_directory: Option<bool>,
    /// The command "New Agent Task" runs in the new task's terminal, in the
    /// task's git worktree.
    ///
    /// Default: "claude"
    pub agent_task_command: Option<String>,
    /// What agents in Solo's terminals can do with `solo-cli agent`: list
    /// terminals, wait for agents and read their answers, open files and
    /// reviews, and (with "full") send agents prompts and start agent tasks.
    ///
    /// Default: "full"
    pub agent_control: Option<AgentControl>,
}

crate::fallible_options::flattened_deserialize!(TerminalSettingsContent {
    sections: { project },
    options: {
        font_size, font_family, font_fallbacks, line_height, font_features, font_weight,
        cursor_shape, blinking, alternate_scroll, option_as_meta, copy_on_select,
        keep_selection_on_copy, open_links_in_mouse_mode, button, dock, starts_open, flexible,
        default_width, default_height, max_scroll_history_lines, scroll_multiplier, toolbar,
        scrollbar, minimum_contrast, show_count_badge, bell, open_changed_files,
        input_editor, compact_prompt, completions_as_you_type, open_links_on_click,
        notify_long_commands_after_seconds, program_notifications, restore_contents,
        agent_commands, claude_code_hooks, follow_working_directory, confirm_close,
        agent_task_command, agent_control,
    },
    defaults: {},
});

/// Shell configuration to open the terminal with.
#[derive(
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::EnumDiscriminants,
)]
#[strum_discriminants(derive(strum::VariantArray, strum::VariantNames, strum::FromRepr))]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    /// Use the system's default terminal configuration in /etc/passwd
    #[default]
    System,
    /// Use a specific program with no arguments.
    Program(String),
    /// Use a specific program with arguments.
    WithArguments {
        /// The program to run.
        program: String,
        /// The arguments to pass to the program.
        args: Vec<String>,
        /// An optional string to override the title of the terminal tab
        title_override: Option<String>,
    },
}

#[derive(
    Clone,
    Debug,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::EnumDiscriminants,
)]
#[strum_discriminants(derive(strum::VariantArray, strum::VariantNames, strum::FromRepr))]
#[serde(rename_all = "snake_case")]
pub enum WorkingDirectory {
    /// Use the current file's directory, falling back to the project directory,
    /// then the first project in the workspace.
    CurrentFileDirectory,
    /// Use the current file's project directory. Fallback to the
    /// first project directory strategy if unsuccessful.
    CurrentProjectDirectory,
    /// Use the first project in this workspace's directory. Fallback to using
    /// this platform's home directory.
    FirstProjectDirectory,
    /// Always use this platform's home directory (if it can be found).
    AlwaysHome,
    /// Always use a specific directory. This value will be shell expanded.
    /// If this path is not a valid directory the terminal will default to
    /// this platform's home directory  (if it can be found).
    Always { directory: String },
}

#[with_fallible_options]
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Eq, Default,
)]
pub struct ScrollbarSettingsContent {
    /// When to show the scrollbar in the terminal.
    ///
    /// Default: inherits editor scrollbar settings
    pub show: Option<ShowScrollbar>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema, MergeFrom, Default)]
#[serde(rename_all = "snake_case")]
pub enum TerminalLineHeight {
    /// Use a line height that's comfortable for reading, 1.618
    #[default]
    Comfortable,
    /// Use a standard line height, 1.3. This option is useful for TUIs,
    /// particularly if they use box characters
    Standard,
    /// Use a custom line height.
    Custom(#[serde(serialize_with = "crate::serialize_f32_with_two_decimal_places")] f32),
}

impl TerminalLineHeight {
    pub fn value(&self) -> f32 {
        match self {
            TerminalLineHeight::Comfortable => 1.618,
            TerminalLineHeight::Standard => 1.3,
            TerminalLineHeight::Custom(line_height) => f32::max(*line_height, 1.),
        }
    }
}

/// When to show the scrollbar.
///
/// Default: auto
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum ShowScrollbar {
    /// Show the scrollbar if there's important information or
    /// follow the system's configured behavior.
    #[default]
    Auto,
    /// Match the system's configured behavior.
    System,
    /// Always show the scrollbar.
    Always,
    /// Never show the scrollbar.
    Never,
}

#[derive(
    Clone,
    Copy,
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
// todo() -> combine with CursorShape
pub enum CursorShapeContent {
    /// Cursor is a block like `█`.
    #[default]
    Block,
    /// Cursor is an underscore like `_`.
    Underline,
    /// Cursor is a vertical bar like `⎸`.
    Bar,
    /// Cursor is a hollow box like `▯`.
    Hollow,
}

#[derive(
    Copy,
    Clone,
    Debug,
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
pub enum TerminalBlink {
    /// Never blink the cursor, ignoring the terminal mode.
    Off,
    /// Default the cursor blink to off, but allow the terminal to
    /// set blinking.
    TerminalControlled,
    /// Always blink the cursor, ignoring the terminal mode.
    On,
}

#[derive(
    Clone,
    Copy,
    Debug,
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
pub enum AlternateScroll {
    On,
    Off,
}

// Toolbar related settings
#[with_fallible_options]
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Eq)]
pub struct TerminalToolbarContent {
    /// Whether to display the terminal title in breadcrumbs inside the terminal pane.
    /// Only shown if the terminal title is not empty.
    ///
    /// The shell running in the terminal needs to be configured to emit the title.
    /// Example: `echo -e "\e]2;New Title\007";`
    ///
    /// Default: true
    pub breadcrumbs: Option<bool>,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum TerminalBell {
    /// Play an OS-specific alert sound.
    #[default]
    System,
    /// Do not play any sound.
    Off,
}

/// SOLO: when to ask before closing a terminal ends the program running in it.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentControl {
    /// Everything, sending prompts and starting tasks included.
    #[default]
    Full,
    /// Looking only: no sending prompts or starting tasks.
    ReadOnly,
    /// Nothing.
    Off,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum TerminalConfirmClose {
    /// Only when an AI agent is running (see `agent_commands`).
    #[default]
    Agents,
    /// When any command is running, an AI agent or not.
    Commands,
    /// Never ask.
    Never,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom,
)]
#[serde(rename_all = "snake_case")]
pub enum CondaManager {
    /// Automatically detect the conda manager
    #[default]
    Auto,
    /// Use conda
    Conda,
    /// Use mamba
    Mamba,
    /// Use micromamba
    Micromamba,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
#[serde(rename_all = "snake_case")]
pub enum VenvSettings {
    #[default]
    Off,
    On {
        /// Default directories to search for virtual environments, relative
        /// to the current working directory. We recommend overriding this
        /// in your project's settings, rather than globally.
        activate_script: Option<ActivateScript>,
        venv_name: Option<String>,
        directories: Option<Vec<PathBuf>>,
        /// Preferred Conda manager to use when activating Conda environments.
        ///
        /// Default: auto
        conda_manager: Option<CondaManager>,
    },
}
#[with_fallible_options]
pub struct VenvSettingsContent<'a> {
    pub activate_script: ActivateScript,
    pub venv_name: &'a str,
    pub directories: &'a [PathBuf],
    pub conda_manager: CondaManager,
}

impl VenvSettings {
    pub fn as_option(&self) -> Option<VenvSettingsContent<'_>> {
        match self {
            VenvSettings::Off => None,
            VenvSettings::On {
                activate_script,
                venv_name,
                directories,
                conda_manager,
            } => Some(VenvSettingsContent {
                activate_script: activate_script.unwrap_or(ActivateScript::Default),
                venv_name: venv_name.as_deref().unwrap_or(""),
                directories: directories.as_deref().unwrap_or(&[]),
                conda_manager: conda_manager.unwrap_or(CondaManager::Auto),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(transparent)]
pub struct PathHyperlinkRegexes(pub Vec<PathHyperlinkRegex>);

impl crate::merge_from::MergeFrom for PathHyperlinkRegexes {
    fn merge_from(&mut self, other: &Self) {
        let inherited = std::mem::take(&mut self.0);
        let mut seen = HashSet::default();
        self.0 = other
            .0
            .iter()
            .flat_map(|entry| {
                if entry.is_rest() {
                    inherited.as_slice()
                } else {
                    std::slice::from_ref(entry)
                }
            })
            .filter(|entry| {
                let regex = match entry {
                    PathHyperlinkRegex::SingleLine(regex) => regex.clone(),
                    PathHyperlinkRegex::MultiLine(lines) => lines.join("\n"),
                };
                // Project layers can still contain an unresolved marker, which must
                // not collapse with the literal regex written as ["..."]
                seen.insert((entry.is_rest(), regex))
            })
            .cloned()
            .collect();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema, MergeFrom)]
#[serde(untagged)]
pub enum PathHyperlinkRegex {
    SingleLine(String),
    MultiLine(Vec<String>),
}

impl PathHyperlinkRegex {
    fn is_rest(&self) -> bool {
        matches!(self, Self::SingleLine(regex) if regex == crate::SplicingVec::REST)
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum TerminalDockPosition {
    Left,
    Bottom,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
#[serde(rename_all = "snake_case")]
pub enum ActivateScript {
    #[default]
    Default,
    Csh,
    Fish,
    Nushell,
    PowerShell,
    Pyenv,
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use crate::{PathHyperlinkRegexes, ProjectSettingsContent, Shell, merge_from::MergeFrom};

    #[test]
    fn test_path_hyperlink_regexes_preserve_entry_forms() -> anyhow::Result<()> {
        let entries = json!([
            "...",
            ["..."],
            ["(?x)", "# comment", "...", "(?<path>file)"],
            "first\nsecond",
            ["first", "second"]
        ]);
        let regexes: PathHyperlinkRegexes = serde_json::from_value(entries.clone())?;
        assert_eq!(serde_json::to_value(regexes)?, entries);
        Ok(())
    }

    #[test]
    fn test_path_hyperlink_regexes_merge_preserves_first_representation() -> anyhow::Result<()> {
        let mut regexes: PathHyperlinkRegexes = serde_json::from_value(json!([
            "...",
            ["..."],
            "first\nsecond",
            ["(?x)", "# comment", "(?<path>file)"]
        ]))?;
        regexes.merge_from(&serde_json::from_value(json!([
            ["first", "second"],
            "...",
            "(?x)\n# comment\n(?<path>file)",
            "..."
        ]))?);
        assert_eq!(
            serde_json::to_value(regexes)?,
            json!([
                ["first", "second"],
                "...",
                ["..."],
                ["(?x)", "# comment", "(?<path>file)"]
            ])
        );
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_project_settings() {
        let project_content =
            json!({"terminal": {"shell": {"program": "/bin/project"}}, "option_as_meta": true});

        let _user_content =
            json!({"terminal": {"shell": {"program": "/bin/user"}}, "option_as_meta": false});

        let project_settings =
            serde_json::from_value::<ProjectSettingsContent>(project_content).unwrap();

        assert_eq!(
            project_settings.terminal.unwrap().shell,
            Some(Shell::Program("/bin/project".to_owned()))
        );
    }
}
