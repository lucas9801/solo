//! SOLO: a Warp-style input editor at the bottom of the terminal.
//!
//! While the shell waits at its prompt, commands are typed in a real editor
//! (selections, multiple cursors, undo) with completions from the shell, a
//! history that filters by what was typed, and ghost suggestions from it.
//! Enter sends the command to the shell. While a command runs, keys go
//! straight to it as usual.

mod completion;
mod history;
mod powershell;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use editor::{
    CompletionProvider, Editor, EditorElement, EditorEvent, EditorStyle, Inlay,
    actions::{MoveRight, MoveToEnd, ShowCompletions},
};
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Subscription, Task, TextStyle, Window, actions, div, relative,
};
use language::{Buffer, CodeLabel, ToOffset as _};
use project::{
    CompletionDisplayOptions, CompletionResponse, CompletionSource, InlayId,
    lsp_store::CompletionDocumentation,
};
use settings::Settings;
use terminal::{Modes, Terminal, terminal_settings::TerminalSettings};

use crate::{TerminalMode, TerminalView};
use theme::ActiveTheme;
use theme_settings::ThemeSettings;
use ui::{Icon, IconName, IconSize, Label, LabelSize, h_flex, prelude::*, v_flex};
use zed_actions::editor::{MoveDown, MoveUp};

use completion::{CompletionKind, complete};
use history::{CommandHistory, load_shell_history};

actions!(
    terminal,
    [
        /// Sends the command in the terminal input to the shell.
        SubmitInput,
        /// Shows an older command from the history, filtered by what was typed.
        InputHistoryPrevious,
        /// Shows a newer command from the history.
        InputHistoryNext,
        /// Accepts the ghost suggestion, or moves the cursor right.
        AcceptInputSuggestion,
        /// Copies the selection (input first, then terminal), or clears the input.
        CopyOrClearInput,
        /// Copies the input's selection, or else the terminal's.
        CopyInputOrTerminal,
        /// Shows completions for the word at the cursor.
        CompleteInput,
        /// Searches the command history, newest first.
        SearchInputHistory,
    ]
);

/// Id of the ghost suggestion inlay; there is at most one per input.
const SUGGESTION_INLAY_ID: usize = 0;

pub struct TerminalInput {
    terminal: Entity<Terminal>,
    editor: Entity<Editor>,
    history: Rc<RefCell<CommandHistory>>,
    /// Makes the next completion request list the history instead.
    searching_history: Rc<Cell<bool>>,
    /// Shell whose history file has been loaded.
    history_shell: Option<String>,
    /// Suffix of the history entry shown after the cursor.
    suggestion: Option<String>,
    /// Set while the input text is replaced from the history, so the edit
    /// doesn't end the history walk.
    replacing_text: bool,
    git_branch: Option<(PathBuf, Option<String>)>,
    _load_history: Task<()>,
    _subscriptions: Vec<Subscription>,
}

pub enum TerminalInputEvent {
    Submitted,
}

impl EventEmitter<TerminalInputEvent> for TerminalInput {}

impl TerminalInput {
    pub fn new(terminal: Entity<Terminal>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let history = Rc::new(RefCell::new(CommandHistory::default()));
        let searching_history = Rc::new(Cell::new(false));
        let editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(1, 12, window, cx);
            editor.set_show_gutter(false, cx);
            editor.set_placeholder_text("Type a command, Tab to complete", window, cx);
            editor.set_completion_provider(Some(Rc::new(ShellCompletionProvider {
                terminal: terminal.downgrade(),
                history: history.clone(),
                searching_history: searching_history.clone(),
            })));
            editor
        });
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, Self::handle_editor_event),
            cx.observe(&terminal, |_, _, cx| cx.notify()),
        ];
        Self {
            terminal,
            editor,
            history,
            searching_history,
            history_shell: None,
            suggestion: None,
            replacing_text: false,
            git_branch: None,
            _load_history: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.editor.focus_handle(cx).contains_focused(window, cx)
    }

    /// Loads the shell's history file once the shell is known.
    fn ensure_history(&mut self, cx: &mut Context<Self>) {
        let shell = self
            .terminal
            .read(cx)
            .shell_integration()
            .shell()
            .map(str::to_string);
        if shell.is_none() || shell == self.history_shell {
            return;
        }
        self.history_shell = shell.clone();
        let history = self.history.clone();
        self._load_history = cx.spawn(async move |_, cx| {
            let entries = cx
                .background_spawn(async move { load_shell_history(shell.as_deref()) })
                .await;
            let mut history = history.borrow_mut();
            // Keep commands submitted while loading as the most recent.
            let mut merged = CommandHistory::new(entries);
            for entry in std::mem::take(&mut *history).into_entries() {
                merged.push(entry);
            }
            *history = merged;
        });
    }

    fn handle_editor_event(
        &mut self,
        _: &Entity<Editor>,
        event: &EditorEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let EditorEvent::BufferEdited = event {
            if !self.replacing_text {
                self.history.borrow_mut().reset_navigation();
            }
            self.update_suggestion(cx);
        }
    }

    fn text(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replacing_text = true;
        self.editor.update(cx, |editor, cx| {
            editor.set_text(text, window, cx);
            editor.move_to_end(&MoveToEnd, window, cx);
        });
        self.replacing_text = false;
    }

    /// Shows the rest of the newest matching history entry after the cursor,
    /// when the cursor is at the end of the input.
    fn update_suggestion(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let suggestion = if self.history.borrow().is_navigating() || text.contains('\n') {
            None
        } else {
            self.history
                .borrow()
                .suggestion(&text)
                .map(|entry| entry[text.len()..].to_string())
                .filter(|suffix| !suffix.contains('\n'))
        };
        if suggestion == self.suggestion {
            return;
        }
        self.suggestion = suggestion.clone();
        self.editor.update(cx, |editor, cx| {
            let to_insert = suggestion
                .map(|suggestion| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let end = snapshot.anchor_after(snapshot.len());
                    Inlay::edit_prediction(SUGGESTION_INLAY_ID, end, suggestion)
                })
                .into_iter()
                .collect();
            editor.splice_inlays(
                &[InlayId::EditPrediction(SUGGESTION_INLAY_ID)],
                to_insert,
                cx,
            );
        });
    }

    fn cursor_row_and_last_row(&self, cx: &mut App) -> (u32, u32) {
        self.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let head = editor
                .selections
                .newest::<language::Point>(&snapshot)
                .head();
            let last_row = editor.buffer().read(cx).snapshot(cx).max_point().row;
            (head.row, last_row)
        })
    }

    fn submit(&mut self, _: &SubmitInput, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        self.history.borrow_mut().push(text.clone());
        self.history.borrow_mut().reset_navigation();
        self.set_text("", window, cx);
        self.update_suggestion(cx);
        // Each line is entered on its own; PowerShell and bash continue an
        // incomplete statement on the next line.
        let mut input = text.replace("\r\n", "\n").replace('\n', "\r");
        input.push('\r');
        self.terminal
            .update(cx, |terminal, _| terminal.input(input.into_bytes()));
        cx.emit(TerminalInputEvent::Submitted);
    }

    fn history_previous(
        &mut self,
        _: &InputHistoryPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (row, _) = self.cursor_row_and_last_row(cx);
        if row > 0 {
            self.editor
                .update(cx, |editor, cx| editor.move_up(&MoveUp, window, cx));
            return;
        }
        let text = self.text(cx);
        let entry = self.history.borrow_mut().previous(&text);
        if let Some(entry) = entry {
            self.set_text(&entry, window, cx);
            self.update_suggestion(cx);
        }
    }

    fn history_next(&mut self, _: &InputHistoryNext, window: &mut Window, cx: &mut Context<Self>) {
        let (row, last_row) = self.cursor_row_and_last_row(cx);
        if row < last_row {
            self.editor
                .update(cx, |editor, cx| editor.move_down(&MoveDown, window, cx));
            return;
        }
        let entry = self.history.borrow_mut().next();
        if let Some(entry) = entry {
            self.set_text(&entry, window, cx);
            self.update_suggestion(cx);
        }
    }

    fn accept_suggestion(
        &mut self,
        _: &AcceptInputSuggestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at_end = self.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<language::Point>(&snapshot);
            selection.is_empty()
                && selection.head() == editor.buffer().read(cx).snapshot(cx).max_point()
        });
        match self.suggestion.take() {
            Some(suggestion) if at_end => {
                self.editor
                    .update(cx, |editor, cx| editor.insert(&suggestion, window, cx));
            }
            suggestion => {
                self.suggestion = suggestion;
                self.editor
                    .update(cx, |editor, cx| editor.move_right(&MoveRight, window, cx));
            }
        }
    }

    fn copy_or_clear(&mut self, _: &CopyOrClearInput, window: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selection(cx) {
            return;
        }
        self.history.borrow_mut().reset_navigation();
        self.set_text("", window, cx);
        self.update_suggestion(cx);
    }

    fn copy_input_or_terminal(
        &mut self,
        _: &CopyInputOrTerminal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.copy_selection(cx);
    }

    /// Copies the input's selection or else the terminal's; returns whether
    /// there was anything to copy.
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let input_selection = self.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<language::Point>(&snapshot);
            (!selection.is_empty()).then(|| {
                editor
                    .buffer()
                    .read(cx)
                    .snapshot(cx)
                    .text_for_range(selection.start..selection.end)
                    .collect::<String>()
            })
        });
        let text = input_selection.or_else(|| {
            self.terminal
                .read(cx)
                .last_content
                .selection_text
                .clone()
                .filter(|text| !text.is_empty())
        });
        match text {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    fn complete(&mut self, _: &CompleteInput, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.show_completions(&ShowCompletions, window, cx)
        });
    }

    fn search_history(
        &mut self,
        _: &SearchInputHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.searching_history.set(true);
        self.editor.update(cx, |editor, cx| {
            editor.show_completions(&ShowCompletions, window, cx)
        });
    }

    fn update_git_branch(&mut self, cwd: Option<&Path>, cx: &App) {
        let Some(cwd) = cwd else {
            self.git_branch = None;
            return;
        };
        let blocks = self.terminal.read(cx).shell_integration().blocks().len();
        // Re-read after each command, which may have switched branches.
        let key = cwd.join(format!("#{blocks}"));
        if self
            .git_branch
            .as_ref()
            .is_some_and(|(cached, _)| *cached == key)
        {
            return;
        }
        self.git_branch = Some((key, git_branch(cwd)));
    }

    fn editor_style(&self, cx: &App) -> EditorStyle {
        let settings = ThemeSettings::get_global(cx);
        let theme = cx.theme();
        EditorStyle {
            background: theme.colors().terminal_background,
            local_player: theme.players().local(),
            text: TextStyle {
                color: theme.colors().text,
                font_family: settings.buffer_font.family.clone(),
                font_features: settings.buffer_font.features.clone(),
                font_fallbacks: settings.buffer_font.fallbacks.clone(),
                font_size: settings.buffer_font_size(cx).into(),
                font_weight: settings.buffer_font.weight,
                line_height: relative(settings.buffer_line_height.value()),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

impl Focusable for TerminalInput {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for TerminalInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_history(cx);
        let terminal = self.terminal.read(cx);
        let integration = terminal.shell_integration();
        let running = integration
            .blocks()
            .last()
            .is_some_and(|block| block.is_running());
        let running_command = running
            .then(|| {
                integration
                    .blocks()
                    .last()
                    .and_then(|block| block.command.clone())
            })
            .flatten();
        let last_exit_code = integration
            .blocks()
            .iter()
            .rev()
            .find_map(|block| block.end_line.and(block.exit_code));
        let cwd = integration.working_directory().cloned();
        self.update_git_branch(cwd.as_deref(), cx);
        let branch = self
            .git_branch
            .as_ref()
            .and_then(|(_, branch)| branch.clone());

        let colors = cx.theme().colors();
        let settings = ThemeSettings::get_global(cx);
        let line_height = settings.buffer_font_size(cx) * settings.buffer_line_height.value();

        v_flex()
            .key_context("TerminalInput")
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::history_previous))
            .on_action(cx.listener(Self::history_next))
            .on_action(cx.listener(Self::accept_suggestion))
            .on_action(cx.listener(Self::copy_or_clear))
            .on_action(cx.listener(Self::copy_input_or_terminal))
            .on_action(cx.listener(Self::complete))
            .on_action(cx.listener(Self::search_history))
            .w_full()
            .px_2()
            .pt_1()
            .pb_1p5()
            .gap_0p5()
            .border_t_1()
            .border_color(colors.border_variant)
            .bg(colors.terminal_background)
            .child(
                h_flex()
                    .gap_3()
                    .when_some(cwd, |this, cwd| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::Folder)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new(display_path(&cwd))
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        )
                    })
                    .when_some(branch, |this, branch| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::GitBranch)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new(branch)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        )
                    })
                    .when_some(last_exit_code.filter(|code| *code != 0), |this, code| {
                        this.child(
                            Label::new(format!("exit {code}"))
                                .size(LabelSize::XSmall)
                                .color(Color::Error),
                        )
                    }),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap_1p5()
                    .child(div().h(line_height).flex().items_center().child(
                        Label::new(if running { "⏵" } else { "❯" }).color(if running {
                            Color::Muted
                        } else {
                            Color::Accent
                        }),
                    ))
                    .child(div().flex_1().min_h(line_height).map(|this| {
                        if running {
                            this.flex().items_center().child(
                                Label::new(match running_command {
                                    Some(command) => {
                                        format!("Running `{command}`, keys go to the program")
                                    }
                                    None => "Running, keys go to the program".to_string(),
                                })
                                .color(Color::Muted),
                            )
                        } else {
                            this.child(EditorElement::new(&self.editor, self.editor_style(cx)))
                        }
                    })),
            )
    }
}

impl TerminalView {
    /// Returns the input editor when it should show: the shell loaded Solo's
    /// integration and isn't running a full screen program. Also moves focus
    /// between the input and the terminal as commands start and finish.
    pub(crate) fn update_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<TerminalView>,
    ) -> Option<Entity<TerminalInput>> {
        let terminal = self.terminal.read(cx);
        let enabled = TerminalSettings::get_global(cx).input_editor
            && matches!(self.mode, TerminalMode::Standalone)
            && !self.read_only
            && terminal.shell_integration().shell().is_some()
            && !terminal.last_content.mode.contains(Modes::ALT_SCREEN)
            && !terminal.vi_mode_enabled();
        let running = terminal
            .shell_integration()
            .blocks()
            .last()
            .is_some_and(|block| block.is_running());

        let input_focused = self
            .input
            .as_ref()
            .is_some_and(|input| input.read(cx).is_focused(window, cx));
        let focus_terminal =
            input_focused && (!enabled || running || self.selected_command_block.is_some());
        if focus_terminal {
            let focus_handle = self.focus_handle.clone();
            window.defer(cx, move |window, cx| window.focus(&focus_handle, cx));
        }
        if !enabled {
            return None;
        }

        let terminal = self.terminal.clone();
        let input = self
            .input
            .get_or_insert_with(|| cx.new(|cx| TerminalInput::new(terminal, window, cx)))
            .clone();
        // Typing goes to the input while the shell waits for a command.
        if !running && self.selected_command_block.is_none() && self.focus_handle.is_focused(window)
        {
            let focus_handle = input.read(cx).editor.focus_handle(cx);
            window.defer(cx, move |window, cx| window.focus(&focus_handle, cx));
        }
        Some(input)
    }
}

/// The current git branch of the repository containing `dir`, or a short
/// commit id for a detached HEAD.
fn git_branch(dir: &Path) -> Option<String> {
    let mut dir = Some(dir);
    while let Some(current) = dir {
        let dot_git = current.join(".git");
        let git_dir = if dot_git.is_dir() {
            Some(dot_git)
        } else if dot_git.is_file() {
            // Worktrees and submodules point to their git dir.
            std::fs::read_to_string(&dot_git).ok().and_then(|contents| {
                let path = contents.strip_prefix("gitdir:")?.trim();
                Some(current.join(path))
            })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_string(),
                None => head.chars().take(7).collect(),
            });
        }
        dir = current.parent();
    }
    None
}

fn display_path(path: &Path) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    if let Some(home) = home
        && let Ok(relative) = path.strip_prefix(&home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display());
    }
    path.display().to_string()
}

/// Feeds the editor's completion menu from the shell and Solo's built-ins.
struct ShellCompletionProvider {
    terminal: gpui::WeakEntity<Terminal>,
    history: Rc<RefCell<CommandHistory>>,
    searching_history: Rc<Cell<bool>>,
}

/// Entries offered when searching the history; the menu filters them further.
const MAX_HISTORY_COMPLETIONS: usize = 2000;

impl ShellCompletionProvider {
    /// The whole history as completions replacing the whole input, so the menu
    /// fuzzy-filters it by what was typed.
    fn history_completions(&self, buffer: &Entity<Buffer>, cx: &App) -> CompletionResponse {
        let snapshot = buffer.read(cx).snapshot();
        let start = snapshot.anchor_before(0);
        let end = snapshot.anchor_after(snapshot.len());
        let completions = self
            .history
            .borrow()
            .newest_first()
            .take(MAX_HISTORY_COMPLETIONS)
            .map(|entry| project::Completion {
                replace_range: start..end,
                new_text: entry.to_string(),
                label: CodeLabel::plain(entry.lines().collect::<Vec<_>>().join(" ⏎ "), None),
                documentation: None,
                source: CompletionSource::Custom,
                icon_path: Some(IconName::HistoryRerun.path().into()),
                icon_color: None,
                match_start: Some(start),
                snippet_deduplication_key: None,
                insert_text_mode: None,
                confirm: None,
                group: None,
            })
            .collect();
        CompletionResponse {
            completions,
            display_options: CompletionDisplayOptions::default(),
            is_incomplete: false,
        }
    }
}

impl CompletionProvider for ShellCompletionProvider {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: language::Anchor,
        _trigger: editor::CompletionContext,
        _window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Vec<CompletionResponse>>> {
        if self.searching_history.replace(false) {
            return Task::ready(Ok(vec![self.history_completions(buffer, cx)]));
        }
        let Some(terminal) = self.terminal.upgrade() else {
            return Task::ready(Ok(Vec::new()));
        };
        let integration = terminal.read(cx).shell_integration();
        let shell = integration.shell().map(str::to_string);
        let cwd = integration.working_directory().cloned();
        let snapshot = buffer.read(cx).snapshot();
        let input = snapshot.text();
        let cursor = buffer_position.to_offset(&snapshot);
        let completions = complete(shell, input, cursor, cwd, cx);
        cx.spawn(async move |_, _| {
            let set = completions.await?;
            let start = snapshot.anchor_before(set.start.min(snapshot.len()));
            let end = snapshot.anchor_after(set.end.min(snapshot.len()));
            let completions = set
                .items
                .into_iter()
                .map(|item| project::Completion {
                    replace_range: start..end,
                    new_text: item.text,
                    label: CodeLabel::plain(item.label, None),
                    documentation: item
                        .detail
                        .map(|detail| CompletionDocumentation::SingleLine(detail.into())),
                    source: CompletionSource::Custom,
                    icon_path: Some(completion_icon(item.kind).path().into()),
                    icon_color: None,
                    match_start: Some(start),
                    snippet_deduplication_key: None,
                    insert_text_mode: None,
                    confirm: None,
                    group: None,
                })
                .collect();
            Ok(vec![CompletionResponse {
                completions,
                display_options: CompletionDisplayOptions::default(),
                is_incomplete: true,
            }])
        })
    }

    fn is_completion_trigger(
        &self,
        _buffer: &Entity<Buffer>,
        _position: language::Anchor,
        _text: &str,
        _trigger_in_words: bool,
        _cx: &mut Context<Editor>,
    ) -> bool {
        // Completions open with Tab, like in a shell.
        false
    }

    fn sort_completions(&self) -> bool {
        // Keep the shell's order: directories first, closest matches first.
        false
    }
}

fn completion_icon(kind: CompletionKind) -> IconName {
    match kind {
        CompletionKind::Command => IconName::Terminal,
        CompletionKind::Subcommand => IconName::ChevronRight,
        CompletionKind::Parameter => IconName::Dash,
        CompletionKind::Directory => IconName::Folder,
        CompletionKind::File => IconName::File,
        CompletionKind::Other => IconName::Sparkle,
    }
}
