//! SOLO: a Warp-style input editor at the bottom of the terminal.
//!
//! While the shell waits at its prompt, commands are typed in a real editor
//! (selections, multiple cursors, undo) with completions from the shell, a
//! history that filters by what was typed, and ghost suggestions from it.
//! Enter sends the command to the shell. While a command runs, keys go
//! straight to it as usual.

mod ai_command;
mod completion;
mod history;
mod powershell;
mod spec;
mod syntax;
pub(crate) mod workflows;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use editor::{
    CompletionProvider, ContextMenuOptions, ContextMenuPlacement, Editor, EditorElement,
    EditorEvent, EditorStyle, HighlightKey, Inlay, MultiBufferOffset,
    actions::{MoveRight, MoveToEnd, ShowCompletions},
};
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Subscription, Task, TextStyle, Window, actions, div, relative,
};
use language::{Buffer, CodeLabel, LanguageRegistry, ToOffset as _};
use project::{
    CompletionDisplayOptions, CompletionResponse, CompletionSource, InlayId,
    lsp_store::CompletionDocumentation,
};
use settings::Settings;
use terminal::{Modes, Terminal, terminal_settings::TerminalSettings};

use crate::{TerminalMode, TerminalView};
use theme::ActiveTheme;
use theme_settings::ThemeSettings;
use ui::{
    Icon, IconButton, IconName, IconSize, Label, LabelSize, Tooltip, h_flex, prelude::*, v_flex,
};
use zed_actions::editor::{MoveDown, MoveUp};

use completion::{CompletionKind, ShellFlavor, complete};
use history::{CommandHistory, SuggestionContext, load_shell_history};

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
        /// Lists the saved workflows to fill the input with one.
        SearchWorkflows,
        /// Opens the file with the saved workflows.
        OpenWorkflowsFile,
        /// Selects the first `{{argument}}` of the workflow just put in the
        /// input; Tab then selects the next ones.
        SelectWorkflowArgument,
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
    /// Makes the next completion request list the workflows instead.
    searching_workflows: Rc<Cell<bool>>,
    /// Whether the input holds a workflow whose `{{arguments}}` Tab selects,
    /// rather than completing.
    filling_workflow: bool,
    /// Completions just computed for Tab, reused when the menu opens.
    last_completions: Rc<RefCell<Option<CachedCompletions>>>,
    /// Shell whose history file has been loaded.
    history_shell: Option<String>,
    /// Suffix of the history entry shown after the cursor.
    suggestion: Option<String>,
    /// Text just put in the input from the history. The editor reports the
    /// edit only after the action is done, so it's recognized by its text,
    /// rather than ending the history walk like edits by the user.
    replaced_text: Option<String>,
    git_branch: Option<(PathBuf, Option<String>)>,
    /// A command being generated from a `#` request.
    generating: Option<Task<()>>,
    /// Why generating the last command failed.
    generate_error: Option<String>,
    languages: Option<Arc<LanguageRegistry>>,
    _load_history: Task<()>,
    _load_language: Task<()>,
    _complete: Task<()>,
    _subscriptions: Vec<Subscription>,
}

pub enum TerminalInputEvent {
    Submitted,
}

impl EventEmitter<TerminalInputEvent> for TerminalInput {}

impl TerminalInput {
    pub fn new(
        terminal: Entity<Terminal>,
        languages: Option<Arc<LanguageRegistry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let history = Rc::new(RefCell::new(CommandHistory::default()));
        let searching_history = Rc::new(Cell::new(false));
        let searching_workflows = Rc::new(Cell::new(false));
        let last_completions = Rc::new(RefCell::new(None));
        let editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(1, 12, window, cx);
            editor.set_show_gutter(false, cx);
            // The input sits at the bottom, so the menu opens upwards, sized
            // by the window rather than the one-line editor.
            editor.set_context_menu_options(ContextMenuOptions {
                min_entries_visible: 12,
                max_entries_visible: 12,
                placement: Some(ContextMenuPlacement::Above),
            });
            editor.set_placeholder_text(
                util::i18n::tr(
                    "Type a command, or # and what you want to do to ask AI",
                    "输入命令，或输入 # 加上想做的事来问 AI",
                ),
                window,
                cx,
            );
            editor.set_completion_provider(Some(Rc::new(ShellCompletionProvider {
                terminal: terminal.downgrade(),
                history: history.clone(),
                searching_history: searching_history.clone(),
                searching_workflows: searching_workflows.clone(),
                last_completions: last_completions.clone(),
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
            searching_workflows,
            filling_workflow: false,
            last_completions,
            history_shell: None,
            suggestion: None,
            replaced_text: None,
            git_branch: None,
            generating: None,
            generate_error: None,
            languages,
            _load_history: Task::ready(()),
            _load_language: Task::ready(()),
            _complete: Task::ready(()),
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
        self.load_language(shell.as_deref(), cx);
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

    /// Highlights commands with the shell's grammar, where Zed has one.
    fn load_language(&mut self, shell: Option<&str>, cx: &mut Context<Self>) {
        let Some(languages) = self.languages.clone() else {
            return;
        };
        if ShellFlavor::from_shell(shell) != ShellFlavor::Posix {
            return;
        }
        let Some(buffer) = self.editor.read(cx).buffer().read(cx).as_singleton() else {
            return;
        };
        self._load_language = cx.spawn(async move |_, cx| {
            if let Ok(language) = languages.language_for_name("Shell Script").await {
                buffer.update(cx, |buffer, cx| buffer.set_language(Some(language), cx));
            }
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
            let text = self.text(cx);
            if self.replaced_text.as_ref() != Some(&text) {
                self.replaced_text = None;
                self.history.borrow_mut().reset_navigation();
            }
            self.update_suggestion(cx);
            self.highlight_syntax(cx);
        }
    }

    /// Highlights PowerShell commands, which Zed has no grammar for. Other
    /// shells use the Shell Script language set on the buffer.
    fn highlight_syntax(&mut self, cx: &mut Context<Self>) {
        let shell = self.terminal.read(cx).shell_integration().shell();
        if ShellFlavor::from_shell(shell) != ShellFlavor::PowerShell {
            return;
        }
        let text = self.text(cx);
        let tokens = syntax::powershell_tokens(&text);
        let syntax_theme = cx.theme().syntax().clone();
        self.editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            for (ix, kind) in syntax::TokenKind::ALL.into_iter().enumerate() {
                let key = HighlightKey::TerminalInputSyntax(ix);
                let ranges = tokens
                    .iter()
                    .filter(|(_, token_kind)| *token_kind == kind)
                    .map(|(range, _)| {
                        snapshot.anchor_after(MultiBufferOffset(range.start))
                            ..snapshot.anchor_before(MultiBufferOffset(range.end))
                    })
                    .collect::<Vec<_>>();
                let style = syntax_theme
                    .highlight_id(kind.theme_name())
                    .and_then(|id| syntax_theme.get(id as usize).copied());
                match style {
                    Some(style) if !ranges.is_empty() => {
                        editor.highlight_text_key(key, ranges, style, false, cx)
                    }
                    _ => editor.clear_highlights(key, cx),
                }
            }
        });
    }

    fn text(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.filling_workflow = false;
        self.replaced_text = Some(text.to_string());
        self.editor.update(cx, |editor, cx| {
            editor.set_text(text, window, cx);
            editor.move_to_end(&MoveToEnd, window, cx);
        });
    }

    /// Shows the rest of the newest matching history entry after the cursor,
    /// when the cursor is at the end of the input.
    fn update_suggestion(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let suggestion = if self.history.borrow().is_navigating() || text.contains('\n') {
            None
        } else {
            let context = self.suggestion_context(cx);
            self.history
                .borrow()
                .suggestion_in(&text, &context)
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

    /// SOLO: where the shell is and what ran there, from the command blocks.
    fn suggestion_context(&self, cx: &App) -> SuggestionContext {
        let integration = self.terminal.read(cx).shell_integration();
        let cwd = integration.working_directory().cloned();
        let mut context = SuggestionContext::default();
        for block in integration.blocks() {
            let Some(command) = block.command.as_ref().map(|command| command.trim_end()) else {
                continue;
            };
            if block.end_line.is_some() {
                if block.exit_code.is_some_and(|code| code != 0) {
                    context.failed.insert(command.to_string());
                } else {
                    context.failed.remove(command);
                }
            }
            if cwd.is_some() && block.working_directory == cwd {
                context.run_here.insert(command.to_string());
            }
        }
        context.cwd = cwd.filter(|cwd| cwd.is_absolute());
        context
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
        // `# what to do` asks AI for a command instead of running the text.
        if let Some(request) = text.trim_start().strip_prefix('#')
            && !request.trim().is_empty()
        {
            self.generate_command(request.trim().to_string(), window, cx);
            return;
        }
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

    fn generate_command(&mut self, request: String, window: &mut Window, cx: &mut Context<Self>) {
        let integration = self.terminal.read(cx).shell_integration();
        let shell = integration.shell().map(str::to_string);
        let cwd = integration.working_directory().cloned();
        self.history.borrow_mut().push(format!("# {request}"));
        self.generate_error = None;
        let generation = cx.background_spawn(async move {
            ai_command::generate_command(&request, shell.as_deref(), cwd.as_deref())
        });
        self.generating = Some(cx.spawn_in(window, async move |this, cx| {
            let result = generation.await;
            this.update_in(cx, |this, window, cx| {
                this.generating = None;
                match result {
                    // Shown for review; Enter runs it.
                    Ok(command) => {
                        this.set_text(&command, window, cx);
                        this.update_suggestion(cx);
                    }
                    Err(error) => this.generate_error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
        match self.suggestion.clone() {
            // The suggestion stays set until the edit updates it, which takes
            // its ghost text away.
            Some(suggestion) if at_end => {
                self.editor
                    .update(cx, |editor, cx| editor.insert(&suggestion, window, cx));
            }
            _ => {
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

    /// Like Warp: a single match is filled in right away, more open the menu.
    fn complete(&mut self, _: &CompleteInput, window: &mut Window, cx: &mut Context<Self>) {
        // A workflow's `{{argument}}`s are selected one after another.
        if self.filling_workflow {
            if self.select_next_placeholder(window, cx) {
                return;
            }
            self.filling_workflow = false;
        }
        let integration = self.terminal.read(cx).shell_integration();
        let shell = integration.shell().map(str::to_string);
        let cwd = integration.working_directory().cloned();
        let (text, cursor) = self.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let head = editor
                .selections
                .newest::<language::Point>(&snapshot)
                .head();
            let buffer = editor.buffer().read(cx).snapshot(cx);
            let text = buffer.text();
            let cursor = buffer.point_to_offset(head).0;
            (text, cursor)
        });
        let completions = complete(shell, text.clone(), cursor, cwd, cx);
        self._complete = cx.spawn_in(window, async move |this, cx| {
            let Ok(set) = completions.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                // Bail if the text changed while completing.
                if this.text(cx) != text {
                    return;
                }
                match single_match(&set, &text, cursor) {
                    Some(item) => {
                        let item = item.clone();
                        this.insert_completion(&text, &set, &item, window, cx)
                    }
                    None => {
                        *this.last_completions.borrow_mut() = Some(CachedCompletions {
                            input: text,
                            cursor,
                            set,
                        });
                        this.editor.update(cx, |editor, cx| {
                            editor.show_completions(&ShowCompletions, window, cx)
                        })
                    }
                }
            })
            .ok();
        });
    }

    /// Replaces the completed token with `item`, followed by a space unless
    /// it is a directory to continue into.
    fn insert_completion(
        &mut self,
        text: &str,
        set: &completion::CompletionSet,
        item: &completion::CompletionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = set.start.min(text.len());
        let end = set.end.clamp(start, text.len());
        let mut inserted = item.text.clone();
        if !inserted.ends_with(['/', '\\']) && !text[end..].starts_with(' ') {
            inserted.push(' ');
        }
        let new_text = format!("{}{}{}", &text[..start], inserted, &text[end..]);
        let point = point_at(&new_text, start + inserted.len());
        self.editor.update(cx, |editor, cx| {
            editor.set_text(new_text.as_str(), window, cx);
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([point..point]);
            });
        });
        self.history.borrow_mut().reset_navigation();
        self.update_suggestion(cx);
    }

    fn select_workflow_argument(
        &mut self,
        _: &SelectWorkflowArgument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filling_workflow = self.select_next_placeholder(window, cx);
    }

    fn select_next_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let head = editor
                .selections
                .newest::<language::Point>(&snapshot)
                .head();
            let buffer = editor.buffer().read(cx).snapshot(cx);
            let text = buffer.text();
            let cursor = buffer.point_to_offset(head).0;
            let Some(range) = workflows::next_placeholder(&text, cursor) else {
                return false;
            };
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections
                    .select_ranges([point_at(&text, range.start)..point_at(&text, range.end)]);
            });
            true
        })
    }

    fn search_workflows(
        &mut self,
        _: &SearchWorkflows,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.searching_workflows.set(true);
        self.editor.update(cx, |editor, cx| {
            window.focus(&editor.focus_handle(cx), cx);
            editor.show_completions(&ShowCompletions, window, cx)
        });
    }

    /// Starts a `#` request in the input, keeping what was typed as its text.
    fn start_ai_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if !text.trim_start().starts_with('#') {
            self.set_text(&format!("# {}", text.trim_start()), window, cx);
        }
        let focus_handle = self.editor.focus_handle(cx);
        window.focus(&focus_handle, cx);
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
            syntax: theme.syntax().clone(),
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
            // SOLO: the suggested rest of the command is dimmed, unlike
            // what was typed.
            edit_prediction_styles: {
                let mut styles = editor::make_suggestion_styles(cx);
                styles.insertion.color = Some(theme.colors().text_placeholder);
                styles
            },
            inlay_hints_style: editor::make_inlay_hints_style(cx),
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
            .on_action(cx.listener(Self::search_workflows))
            .on_action(cx.listener(Self::select_workflow_argument))
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
                            Label::new(if util::i18n::is_chinese() {
                                format!("退出码 {code}")
                            } else {
                                format!("exit {code}")
                            })
                            .size(LabelSize::XSmall)
                            .color(Color::Error),
                        )
                    })
                    .when(self.generating.is_some(), |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::Sparkle)
                                        .size(IconSize::XSmall)
                                        .color(Color::Accent),
                                )
                                .child(
                                    Label::new(util::i18n::tr(
                                        "Generating a command with Claude Code…",
                                        "正在用 Claude Code 生成命令…",
                                    ))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Accent),
                                ),
                        )
                    })
                    .when_some(self.generate_error.clone(), |this, error| {
                        this.child(
                            Label::new(error)
                                .size(LabelSize::XSmall)
                                .color(Color::Error)
                                .truncate(),
                        )
                    })
                    .when(!running, |this| {
                        this.child(div().flex_1()).child(
                            h_flex()
                                .gap_0p5()
                                .child(
                                    IconButton::new("ask-ai", IconName::Sparkle)
                                        .icon_size(IconSize::XSmall)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::text(util::i18n::tr(
                                            "Ask AI for a command (#)",
                                            "让 AI 生成命令 (#)",
                                        )))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.start_ai_request(window, cx)
                                        })),
                                )
                                .child(
                                    IconButton::new("workflows", IconName::BoltOutlined)
                                        .icon_size(IconSize::XSmall)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::for_action_title(
                                            "Workflows",
                                            &SearchWorkflows,
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.search_workflows(&SearchWorkflows, window, cx)
                                        })),
                                ),
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
                                    Some(command) if util::i18n::is_chinese() => {
                                        format!("正在运行 `{command}`，按键直接发给程序")
                                    }
                                    Some(command) => {
                                        format!("Running `{command}`, keys go to the program")
                                    }
                                    None => util::i18n::tr(
                                        "Running, keys go to the program",
                                        "正在运行，按键直接发给程序",
                                    )
                                    .to_string(),
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
    pub(crate) fn input_focused(&self, window: &Window, cx: &App) -> bool {
        self.input
            .as_ref()
            .is_some_and(|input| input.read(cx).is_focused(window, cx))
    }

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
        let languages = self
            .project
            .upgrade()
            .map(|project| project.read(cx).languages().clone());
        let input = self
            .input
            .get_or_insert_with(|| cx.new(|cx| TerminalInput::new(terminal, languages, window, cx)))
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
pub(crate) fn git_branch(dir: &Path) -> Option<String> {
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
    searching_workflows: Rc<Cell<bool>>,
    last_completions: Rc<RefCell<Option<CachedCompletions>>>,
}

struct CachedCompletions {
    input: String,
    cursor: usize,
    set: completion::CompletionSet,
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

impl ShellCompletionProvider {
    /// Saved workflows, replacing the whole input, then an entry to edit them.
    fn workflow_completions(&self, buffer: &Entity<Buffer>, cx: &App) -> CompletionResponse {
        let shell = self.terminal.upgrade().and_then(|terminal| {
            terminal
                .read(cx)
                .shell_integration()
                .shell()
                .map(str::to_string)
        });
        let snapshot = buffer.read(cx).snapshot();
        let start = snapshot.anchor_before(0);
        let end = snapshot.anchor_after(snapshot.len());
        let completion =
            |label: String,
             new_text: String,
             documentation: Option<String>,
             icon: IconName,
             confirm: Option<CompletionConfirm>| project::Completion {
                replace_range: start..end,
                new_text,
                label: CodeLabel::plain(label, None),
                documentation: documentation
                    .map(|text| CompletionDocumentation::MultiLinePlainText(text.into())),
                source: CompletionSource::Custom,
                icon_path: Some(icon.path().into()),
                icon_color: None,
                match_start: Some(start),
                snippet_deduplication_key: None,
                insert_text_mode: None,
                confirm,
                group: None,
            };
        let mut completions: Vec<_> = workflows::load(ShellFlavor::from_shell(shell.as_deref()))
            .into_iter()
            .map(|workflow| {
                let documentation = match &workflow.description {
                    Some(description) => format!("{}\n\n{description}", workflow.command),
                    None => workflow.command.clone(),
                };
                // Select the first argument once the command is in.
                let confirm = workflows::next_placeholder(&workflow.command, 0).map(|_| {
                    Arc::new(|_, window: &mut Window, cx: &mut App| {
                        window.dispatch_action(Box::new(SelectWorkflowArgument), cx);
                        false
                    }) as CompletionConfirm
                });
                completion(
                    workflow.name,
                    workflow.command,
                    Some(documentation),
                    IconName::BoltOutlined,
                    confirm,
                )
            })
            .collect();
        completions.push(completion(
            util::i18n::tr("Edit workflows…", "编辑 Workflows…").to_string(),
            snapshot.text(),
            Some(if util::i18n::is_chinese() {
                format!(
                    "打开 {} 来添加、修改或删除 Workflow。",
                    workflows::workflows_file().display()
                )
            } else {
                format!(
                    "Opens {} to add, change or remove workflows.",
                    workflows::workflows_file().display()
                )
            }),
            IconName::Pencil,
            Some(Arc::new(|_, window: &mut Window, cx: &mut App| {
                window.dispatch_action(Box::new(OpenWorkflowsFile), cx);
                false
            })),
        ));
        CompletionResponse {
            completions,
            display_options: CompletionDisplayOptions::default(),
            is_incomplete: false,
        }
    }
}

type CompletionConfirm =
    Arc<dyn Send + Sync + Fn(project::CompletionIntent, &mut Window, &mut App) -> bool>;

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
        if self.searching_workflows.replace(false) {
            return Task::ready(Ok(vec![self.workflow_completions(buffer, cx)]));
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
        let cached = self
            .last_completions
            .borrow_mut()
            .take()
            .filter(|cached| cached.input == input && cached.cursor == cursor);
        let completions = match cached {
            Some(cached) => Task::ready(Ok(cached.set)),
            None => complete(shell, input, cursor, cwd, cx),
        };
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
        text: &str,
        _trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool {
        // Completions open with Tab, like in a shell, unless they should open
        // as you type.
        TerminalSettings::get_global(cx).completions_as_you_type
            && text.chars().any(|ch| !ch.is_whitespace())
    }

    fn sort_completions(&self) -> bool {
        // Keep the shell's order: directories first, closest matches first.
        false
    }
}

/// The only item that the typed token is a prefix of, if there is exactly one.
fn single_match<'a>(
    set: &'a completion::CompletionSet,
    text: &str,
    cursor: usize,
) -> Option<&'a completion::CompletionItem> {
    let typed = text.get(set.start..cursor.max(set.start))?.to_lowercase();
    let mut matches = set
        .items
        .iter()
        .filter(|item| item.text.to_lowercase().starts_with(&typed));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn completion_icon(kind: CompletionKind) -> IconName {
    match kind {
        CompletionKind::Command => IconName::Terminal,
        CompletionKind::Subcommand => IconName::ChevronRight,
        CompletionKind::Parameter => IconName::Dash,
        CompletionKind::Value => IconName::Circle,
        CompletionKind::Branch => IconName::GitBranch,
        CompletionKind::Container => IconName::Box,
        CompletionKind::Directory => IconName::Folder,
        CompletionKind::File => IconName::File,
        CompletionKind::Other => IconName::Sparkle,
    }
}

fn point_at(text: &str, offset: usize) -> language::Point {
    let before = &text[..offset];
    let row = before.matches('\n').count() as u32;
    let column = (offset - before.rfind('\n').map_or(0, |ix| ix + 1)) as u32;
    language::Point::new(row, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use terminal::TerminalBuilder;
    use util::paths::PathStyle;

    /// A shell with no history file, so the user's own history stays out.
    const SHELL_PROMPT: &[u8] =
        b"\x1b]633;P;Shell=solo-test-shell;test-nonce\x07\x1b]133;A\x07$ \x1b]133;B\x07";

    async fn terminal_with_view(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Terminal>,
        Entity<TerminalView>,
        &mut VisualTestContext,
    ) {
        let (project, workspace) = crate::tests::init_test(cx).await;
        cx.update(|cx| {
            cx.bind_keys(
                settings::KeymapFile::load_asset_allow_partial_failure(
                    settings::DEFAULT_KEYMAP_PATH,
                    cx,
                )
                .unwrap(),
            )
        });
        let terminal = cx.new(|cx| {
            TerminalBuilder::new_display_only(
                Default::default(),
                settings::AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                PathStyle::local(),
            )
            .subscribe(cx)
        });
        terminal.update(cx, |terminal, _| terminal.set_shell_nonce("test-nonce"));
        let (view, cx) = cx.add_window_view(|window, cx| {
            TerminalView::new(
                terminal.clone(),
                workspace.downgrade(),
                None,
                project.downgrade(),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            let focus_handle = view.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
        });
        (terminal, view, cx)
    }

    fn write(terminal: &Entity<Terminal>, bytes: &[u8], cx: &mut VisualTestContext) {
        terminal.update(cx, |terminal, cx| terminal.write_raw_output(bytes, cx));
        draw(cx);
    }

    /// Draws twice: once to update the input, once more after the focus
    /// change that defers.
    fn draw(cx: &mut VisualTestContext) {
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        cx.run_until_parked();
    }

    fn input(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Entity<TerminalInput> {
        view.read_with(cx, |view, _| view.input.clone())
            .expect("the input editor should show")
    }

    fn input_text(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> String {
        let input = input(view, cx);
        input.read_with(cx, |input, cx| input.text(cx))
    }

    fn input_focused(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| view.read(cx).input_focused(window, cx))
    }

    fn take_input_log(terminal: &Entity<Terminal>, cx: &mut VisualTestContext) -> Vec<String> {
        terminal
            .update(cx, |terminal, _| terminal.take_input_log())
            .into_iter()
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .collect()
    }

    fn push_history(view: &Entity<TerminalView>, entries: &[&str], cx: &mut VisualTestContext) {
        let input = input(view, cx);
        input.read_with(cx, |input, _| {
            for entry in entries {
                input.history.borrow_mut().push(entry.to_string());
            }
        });
    }

    #[gpui::test]
    async fn test_input_shows_while_shell_waits_at_prompt(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;

        // Without Solo's shell integration there is no input editor.
        write(&terminal, b"$ ", cx);
        assert!(view.read_with(cx, |view, _| view.input.is_none()));

        write(&terminal, SHELL_PROMPT, cx);
        assert!(view.read_with(cx, |view, _| view.input.is_some()));
        assert!(input_focused(&view, cx));

        // A full screen program takes the whole terminal.
        write(
            &terminal,
            b"vim\r\n\x1b]633;E;vim;test-nonce\x07\x1b]133;C\x07\x1b[?1049h",
            cx,
        );
        assert!(!input_focused(&view, cx));
        let shows = cx.update(|window, cx| {
            view.update(cx, |view, cx| view.update_input(window, cx).is_some())
        });
        assert!(!shows);
    }

    #[gpui::test]
    async fn test_keys_go_to_running_program(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        assert!(input_focused(&view, cx));

        write(
            &terminal,
            b"python\r\n\x1b]633;E;python;test-nonce\x07\x1b]133;C\x07>>> ",
            cx,
        );
        assert!(!input_focused(&view, cx));
        assert!(cx.update(|window, cx| view.read(cx).focus_handle.is_focused(window)));
        take_input_log(&terminal, cx);
        cx.simulate_input("x");
        cx.run_until_parked();
        assert_eq!(take_input_log(&terminal, cx).concat(), "x");
        assert_eq!(input_text(&view, cx), "");

        // Back at the prompt, typing goes to the input again.
        write(
            &terminal,
            b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
            cx,
        );
        assert!(input_focused(&view, cx));
        cx.simulate_input("ls");
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "ls");
        assert!(take_input_log(&terminal, cx).is_empty());
    }

    #[gpui::test]
    async fn test_enter_sends_the_command(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        take_input_log(&terminal, cx);

        cx.simulate_input("ls -la");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(take_input_log(&terminal, cx), ["ls -la\r"]);
        assert_eq!(input_text(&view, cx), "");

        // Each line of a multi-line command is entered on its own.
        cx.simulate_input("echo a");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("echo b");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(take_input_log(&terminal, cx), ["echo a\recho b\r"]);

        // Sent commands go to the history, newest first.
        let input = input(&view, cx);
        let history = input.read_with(cx, |input, _| {
            input
                .history
                .borrow()
                .newest_first()
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        assert_eq!(history, ["echo a\necho b", "ls -la"]);
    }

    #[gpui::test]
    async fn test_up_filters_history_by_typed_prefix(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        push_history(&view, &["git status", "ls", "git log", "cargo test"], cx);

        // The newest entry starting with what was typed, skipping others.
        cx.simulate_input("git");
        cx.simulate_keystrokes("up");
        assert_eq!(input_text(&view, cx), "git log");

        // With nothing typed, up shows the newest entry.
        cx.simulate_keystrokes("ctrl-a backspace");
        assert_eq!(input_text(&view, cx), "");
        cx.simulate_keystrokes("up");
        assert_eq!(input_text(&view, cx), "cargo test");
    }

    #[gpui::test]
    async fn test_up_and_down_walk_history_by_typed_prefix(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        push_history(&view, &["git status", "ls", "git log", "cargo test"], cx);

        cx.simulate_input("git");
        cx.simulate_keystrokes("up");
        assert_eq!(input_text(&view, cx), "git log");
        cx.simulate_keystrokes("up");
        assert_eq!(input_text(&view, cx), "git status");
        // Nothing older matches; the text stays.
        cx.simulate_keystrokes("up");
        assert_eq!(input_text(&view, cx), "git status");

        cx.simulate_keystrokes("down");
        assert_eq!(input_text(&view, cx), "git log");
        // Past the newest match, the typed text comes back.
        cx.simulate_keystrokes("down");
        assert_eq!(input_text(&view, cx), "git");
    }

    #[gpui::test]
    async fn test_accepting_ghost_suggestion(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        push_history(&view, &["git stash", "git status --short"], cx);
        let input = input(&view, cx);
        let suggestion =
            |cx: &mut VisualTestContext| input.read_with(cx, |input, _| input.suggestion.clone());

        // The newest entry starting with what was typed.
        cx.simulate_input("git stas");
        cx.run_until_parked();
        assert_eq!(suggestion(cx).as_deref(), Some("h"));
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        assert_eq!(suggestion(cx).as_deref(), Some("tus --short"));

        // Right at the end accepts it.
        cx.simulate_keystrokes("right");
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "git status --short");
        assert_eq!(suggestion(cx), None);
        // Its ghost text is gone too.
        let shown = input.update(cx, |input, cx| {
            input
                .editor
                .update(cx, |editor, cx| editor.display_text(cx))
        });
        assert_eq!(shown, "git status --short");

        // Elsewhere, right only moves the cursor.
        cx.simulate_keystrokes("ctrl-a backspace");
        cx.simulate_input("git sta");
        cx.simulate_keystrokes("left right");
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "git sta");
        assert_eq!(suggestion(cx).as_deref(), Some("tus --short"));
        cx.simulate_keystrokes("right");
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "git status --short");
    }
}
