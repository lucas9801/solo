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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use editor::{
    CompletionProvider, ContextMenuOptions, ContextMenuPlacement, Editor, EditorElement,
    EditorEvent, EditorStyle, HighlightKey, Inlay, MultiBufferOffset,
    actions::{Cancel, MoveRight, MoveToEnd, ShowCompletions},
};
use futures::future::Either;
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Subscription, Task, TextStyle, WeakEntity, Window, actions, div,
    relative,
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

use completion::{
    CompletionDepth, CompletionKind, CompletionSet, ShellFlavor, complete,
    should_complete_as_you_type, spec_suggestion,
};
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
        /// Stops generating a command for a `#` request.
        CancelCommandGeneration,
    ]
);

/// Id of the ghost suggestion inlay; there is at most one per input.
const SUGGESTION_INLAY_ID: usize = 0;

/// SOLO: completions open by themselves after this pause in typing, so
/// typing fast doesn't start a request per key.
const AS_YOU_TYPE_DELAY: Duration = Duration::from_millis(120);
/// How long completions opening as you type wait for PowerShell and
/// generators before showing what Solo already has; theirs follow.
const FULL_COMPLETIONS_WAIT: Duration = Duration::from_millis(250);

/// Whether the hint on how to use a menu that opened by itself has been
/// shown and dealt with, in this run of Solo.
static AUTO_MENU_HINT_DONE: AtomicBool = AtomicBool::new(false);

/// Computes completions; tests replace it.
type CompleteFn = Rc<dyn Fn(CompletionRequest, &App) -> Task<Result<CompletionSet>>>;

#[derive(Clone)]
struct CompletionRequest {
    shell: Option<String>,
    input: String,
    cursor: usize,
    cwd: Option<PathBuf>,
    depth: CompletionDepth,
}

fn default_completer() -> CompleteFn {
    Rc::new(|request: CompletionRequest, cx: &App| {
        complete(
            request.shell,
            request.input,
            request.cursor,
            request.cwd,
            request.depth,
            cx,
        )
    })
}

/// Generates a command for a `#` request; tests replace it.
type GenerateFn = Rc<dyn Fn(ai_command::Request, Arc<AtomicBool>, &App) -> Task<Result<String>>>;

fn default_generator() -> GenerateFn {
    Rc::new(
        |request: ai_command::Request, cancel: Arc<AtomicBool>, cx: &App| {
            let settings = TerminalSettings::get_global(cx);
            let options = ai_command::Options {
                agent: settings.ai_command_agent,
                claude_model: settings.ai_command_claude_model.clone(),
                codex_model: settings.ai_command_codex_model.clone(),
            };
            cx.background_spawn(
                async move { ai_command::generate_command(&request, &options, &cancel) },
            )
        },
    )
}

/// A command block sent along with a `#` request.
#[derive(Clone, Debug, PartialEq)]
struct AiBlock {
    ix: usize,
    command: String,
}

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
    /// SOLO: whether the suggestion comes from a command spec rather than
    /// the history; Tab accepts those too.
    suggestion_from_spec: bool,
    /// The input the suggestion was made for.
    suggestion_input: String,
    _spec_suggestion: Task<()>,
    completer: Rc<RefCell<CompleteFn>>,
    /// SOLO: whether the completion menu was opened on purpose (Tab, ctrl-r,
    /// workflows), in which Enter accepts, rather than as you type, in which
    /// Enter runs the command.
    menu_requested: Rc<Cell<bool>>,
    /// Whether to explain the menu that just opened by itself.
    show_auto_menu_hint: bool,
    /// Text just put in the input from the history. The editor reports the
    /// edit only after the action is done, so it's recognized by its text,
    /// rather than ending the history walk like edits by the user.
    replaced_text: Option<String>,
    git_branch: Option<(PathBuf, Option<String>)>,
    /// A command being generated from a `#` request.
    generating: Option<Task<()>>,
    /// Stops the agent generating the command.
    generation_cancel: Option<Arc<AtomicBool>>,
    generating_since: Option<Instant>,
    /// Redraws the time generating has taken.
    _generation_ticker: Task<()>,
    generator: GenerateFn,
    /// Command blocks the user attached to the next `#` request.
    ai_blocks: Vec<AiBlock>,
    /// The failed block the user took off the `#` request.
    dismissed_failure: Option<usize>,
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
        let completer = Rc::new(RefCell::new(default_completer()));
        let menu_requested = Rc::new(Cell::new(false));
        let this = cx.weak_entity();
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
                    "Type a command (Tab completes), or # and what you want to do to ask AI",
                    "输入命令（Tab 补全），或输入 # 加上想做的事来问 AI",
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
                completer: completer.clone(),
                input: this,
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
            suggestion_from_spec: false,
            suggestion_input: String::new(),
            _spec_suggestion: Task::ready(()),
            completer,
            menu_requested,
            show_auto_menu_hint: false,
            replaced_text: None,
            git_branch: None,
            generating: None,
            generation_cancel: None,
            generating_since: None,
            _generation_ticker: Task::ready(()),
            generator: default_generator(),
            ai_blocks: Vec::new(),
            dismissed_failure: None,
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
            // SOLO: a menu opened on purpose stays so while it shows; the
            // next one opens as you type.
            if !self.editor.read(cx).has_visible_completions_menu() {
                self.menu_requested.set(false);
                self.dismiss_auto_menu_hint(cx);
            }
            self.update_suggestion(cx);
            self.highlight_syntax(cx);
        }
    }

    /// SOLO: called when completions opened as you type.
    fn auto_menu_opened(&mut self, cx: &mut Context<Self>) {
        if !self.menu_requested.get() && !AUTO_MENU_HINT_DONE.load(Ordering::Relaxed) {
            self.show_auto_menu_hint = true;
            cx.notify();
        }
    }

    fn dismiss_auto_menu_hint(&mut self, cx: &mut Context<Self>) {
        if self.show_auto_menu_hint {
            self.show_auto_menu_hint = false;
            AUTO_MENU_HINT_DONE.store(true, Ordering::Relaxed);
            cx.notify();
        }
    }

    /// Marks the menu about to open as opened on purpose, so Enter accepts.
    fn request_menu(&mut self, cx: &mut Context<Self>) {
        if !self.menu_requested.replace(true) {
            cx.notify();
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
    /// when the cursor is at the end of the input. SOLO: without one, the
    /// most likely completion from the command's spec, worked out in the
    /// background.
    fn update_suggestion(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let possible = !self.history.borrow().is_navigating() && !text.contains('\n');
        let from_history = if possible {
            let context = self.suggestion_context(cx);
            self.history
                .borrow()
                .suggestion_in(&text, &context)
                .map(|entry| entry[text.len()..].to_string())
                .filter(|suffix| !suffix.contains('\n'))
        } else {
            None
        };
        if from_history.is_some() || !possible || text.trim().is_empty() {
            self._spec_suggestion = Task::ready(());
            self.set_suggestion(from_history, false, text, cx);
            return;
        }
        // Keep showing what's left of the spec suggestion while typing it.
        let kept = self
            .suggestion
            .as_ref()
            .filter(|_| self.suggestion_from_spec)
            .map(|suggestion| format!("{}{suggestion}", self.suggestion_input))
            .and_then(|full| {
                full.strip_prefix(text.as_str())
                    .filter(|rest| !rest.is_empty())
                    .map(str::to_string)
            });
        self.set_suggestion(kept, true, text.clone(), cx);
        let flavor = ShellFlavor::from_shell(self.terminal.read(cx).shell_integration().shell());
        let suggestion = cx.background_spawn({
            let text = text.clone();
            async move { spec_suggestion(&text, flavor) }
        });
        self._spec_suggestion = cx.spawn(async move |this, cx| {
            let suggestion = suggestion.await;
            this.update(cx, |this, cx| {
                if this.text(cx) == text {
                    this.set_suggestion(suggestion, true, text, cx);
                }
            })
            .ok();
        });
    }

    fn set_suggestion(
        &mut self,
        suggestion: Option<String>,
        from_spec: bool,
        input: String,
        cx: &mut Context<Self>,
    ) {
        self.suggestion_from_spec = from_spec && suggestion.is_some();
        self.suggestion_input = input;
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
            if self.generating.is_none() {
                self.generate_command(request.trim().to_string(), window, cx);
            }
            return;
        }
        // SOLO: Enter in a menu that opened as you type runs the command.
        self.editor.update(cx, |editor, cx| {
            if editor.has_visible_completions_menu() {
                editor.dismiss_menus_and_popups(true, window, cx);
            }
        });
        self.dismiss_auto_menu_hint(cx);
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
        let terminal = self.terminal.read(cx);
        let integration = terminal.shell_integration();
        let shell = integration.shell().map(str::to_string);
        let cwd = integration.working_directory().cloned();
        let context = self
            .ai_context_blocks(cx)
            .into_iter()
            .map(|(block, _)| ai_command::ContextBlock {
                command: terminal
                    .command_block_command(block.ix)
                    .unwrap_or(block.command),
                output: ai_command::tail_chars(
                    terminal
                        .command_block_output(block.ix)
                        .unwrap_or_default()
                        .trim_end(),
                    ai_command::MAX_OUTPUT_CHARS,
                ),
                exit_code: integration
                    .blocks()
                    .get(block.ix)
                    .and_then(|block| block.exit_code),
            })
            .collect();
        let request = ai_command::Request {
            request,
            shell,
            cwd,
            context,
        };
        self.history
            .borrow_mut()
            .push(format!("# {}", request.request));
        self.generate_error = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = (self.generator)(request, cancel.clone(), cx);
        self.generation_cancel = Some(cancel);
        self.generating_since = Some(Instant::now());
        self.generating = Some(cx.spawn_in(window, async move |this, cx| {
            let result = generation.await;
            this.update_in(cx, |this, window, cx| {
                this.stop_generating(cx);
                match result {
                    // Shown for review; Enter runs it.
                    Ok(command) => {
                        this.ai_blocks.clear();
                        this.set_text(&command, window, cx);
                        this.update_suggestion(cx);
                    }
                    Err(error) if error.is::<ai_command::Cancelled>() => {}
                    Err(error) => this.generate_error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        // The time taken so far, once a second.
        self._generation_ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let generating = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.generating.is_some()
                    })
                    .unwrap_or(false);
                if !generating {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn stop_generating(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.generation_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.generating = None;
        self.generating_since = None;
        self._generation_ticker = Task::ready(());
        cx.notify();
    }

    /// Esc stops generating a command; otherwise it does what it does in
    /// an editor.
    fn cancel_generation(
        &mut self,
        _: &CancelCommandGeneration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.generating.is_some() {
            self.stop_generating(cx);
            return;
        }
        self.editor
            .update(cx, |editor, cx| editor.cancel(&Cancel, window, cx));
    }

    /// SOLO: the command blocks that go with a `#` request: the ones the user
    /// attached, and the last failed command unless taken off. `true` marks
    /// the failed one added by itself.
    fn ai_context_blocks(&self, cx: &App) -> Vec<(AiBlock, bool)> {
        let mut blocks: Vec<(AiBlock, bool)> = Vec::new();
        if TerminalSettings::get_global(cx).ai_command_include_last_failure {
            let terminal = self.terminal.read(cx);
            let failed = terminal
                .shell_integration()
                .blocks()
                .iter()
                .rposition(|block| {
                    block.end_line.is_some() && block.exit_code.is_some_and(|code| code != 0)
                });
            if let Some(ix) = failed
                && self.dismissed_failure != Some(ix)
                && !self.ai_blocks.iter().any(|block| block.ix == ix)
                && let Some(command) = terminal.command_block_command(ix)
            {
                blocks.push((AiBlock { ix, command }, true));
            }
        }
        blocks.extend(self.ai_blocks.iter().cloned().map(|block| (block, false)));
        blocks
    }

    /// Adds a command block to the next `#` request.
    pub(crate) fn attach_block(&mut self, ix: usize, command: String, cx: &mut Context<Self>) {
        if !self.ai_blocks.iter().any(|block| block.ix == ix) {
            self.ai_blocks.push(AiBlock { ix, command });
        }
        if self.dismissed_failure == Some(ix) {
            self.dismissed_failure = None;
        }
        cx.notify();
    }

    fn remove_ai_block(&mut self, ix: usize, auto: bool, cx: &mut Context<Self>) {
        if auto {
            self.dismissed_failure = Some(ix);
        } else {
            self.ai_blocks.retain(|block| block.ix != ix);
        }
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
        let (text, cursor) = self
            .editor
            .update(cx, |editor, cx| text_and_cursor(editor, cx));
        // SOLO: Tab takes the suggestion from the spec, like a completion.
        if self.suggestion_from_spec
            && cursor == text.len()
            && let Some(mut suggestion) = self.suggestion.clone()
        {
            if !suggestion.ends_with(['/', '\\', '=']) {
                suggestion.push(' ');
            }
            self.editor
                .update(cx, |editor, cx| editor.insert(&suggestion, window, cx));
            return;
        }
        let completions = (self.completer.borrow())(
            CompletionRequest {
                shell,
                input: text.clone(),
                cursor,
                cwd,
                depth: CompletionDepth::default(),
            },
            cx,
        );
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
                        this.request_menu(cx);
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
        self.request_menu(cx);
        self.editor.update(cx, |editor, cx| {
            window.focus(&editor.focus_handle(cx), cx);
            editor.show_completions(&ShowCompletions, window, cx)
        });
    }

    /// Starts a `#` request in the input, keeping what was typed as its text.
    pub(crate) fn start_ai_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.request_menu(cx);
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

        let ai_request = !running && self.text(cx).trim_start().starts_with('#');
        let ai_blocks = if ai_request {
            self.ai_context_blocks(cx)
        } else {
            Vec::new()
        };
        let generating_seconds = self.generating_since.map(|since| since.elapsed().as_secs());

        v_flex()
            // SOLO: `menu_requested` lets Enter accept in a menu opened on
            // purpose; in one that opened as you type, Enter runs the command.
            .key_context(if self.menu_requested.get() {
                "TerminalInput menu_requested"
            } else {
                "TerminalInput"
            })
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel_generation))
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
                        let editor_focus = self.editor.focus_handle(cx);
                        this.child(
                            Label::new(if util::i18n::is_chinese() {
                                format!("退出码 {code}")
                            } else {
                                format!("exit {code}")
                            })
                            .size(LabelSize::XSmall)
                            .color(Color::Error),
                        )
                        // SOLO: what failed, to the agent working on the code.
                        .child(
                            Button::new(
                                "send-failures-to-agent",
                                util::i18n::tr("Send to Agent", "发给 agent"),
                            )
                            .label_size(LabelSize::XSmall)
                            .style(ButtonStyle::Subtle)
                            .tooltip(Tooltip::text(util::i18n::tr(
                                "Paste the errors or failed tests into the agent working here",
                                "把错误或失败的测试粘贴给在这里工作的 agent",
                            )))
                            .on_click(move |_, window, cx| {
                                // The terminal handles it, from its input.
                                window.focus(&editor_focus, cx);
                                window.dispatch_action(
                                    Box::new(crate::command_blocks::SendFailuresToAgent),
                                    cx,
                                );
                            }),
                        )
                    })
                    .when_some(generating_seconds, |this, seconds| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::Sparkle)
                                        .size(IconSize::XSmall)
                                        .color(Color::Accent),
                                )
                                .child(
                                    Label::new(if util::i18n::is_chinese() {
                                        format!("正在生成命令… {seconds} 秒（Esc 取消）")
                                    } else {
                                        format!("Generating a command… {seconds}s (Esc cancels)")
                                    })
                                    .size(LabelSize::XSmall)
                                    .color(Color::Accent),
                                ),
                        )
                    })
                    .when(self.show_auto_menu_hint, |this| {
                        this.child(
                            Label::new(util::i18n::tr(
                                "Tab accepts, Enter runs the command, Esc closes; \
                                 terminal.completions_as_you_type turns this off",
                                "Tab 接受，Enter 直接运行命令，Esc 关闭；\
                                 设置 terminal.completions_as_you_type 可关掉自动弹出",
                            ))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate(),
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
            // SOLO: the command blocks going with a `#` request, each one
            // can be taken off.
            .when(!ai_blocks.is_empty(), |this| {
                this.child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Label::new(util::i18n::tr("Sends:", "一起发送："))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .children(ai_blocks.into_iter().map(|(block, auto)| {
                            let AiBlock { ix, command } = block;
                            let mut label = command.lines().next().unwrap_or_default().to_string();
                            if label.chars().count() > 40 {
                                label = label.chars().take(39).collect::<String>() + "…";
                            }
                            if auto {
                                label = if util::i18n::is_chinese() {
                                    format!("失败：{label}")
                                } else {
                                    format!("failed: {label}")
                                };
                            }
                            h_flex()
                                .id(("ai-block", ix))
                                .gap_0p5()
                                .pl_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.element_background)
                                .tooltip(Tooltip::text(util::i18n::tr(
                                    "This command and the end of its output go to your \
                                     Claude Code or Codex with the request",
                                    "这条命令和它输出的最后一部分会随请求发给你的 Claude Code 或 Codex",
                                )))
                                .child(
                                    Icon::new(IconName::Terminal)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .child(Label::new(label).size(LabelSize::XSmall))
                                .child(
                                    IconButton::new(("remove-ai-block", ix), IconName::Close)
                                        .icon_size(IconSize::XSmall)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::text(util::i18n::tr(
                                            "Don't send",
                                            "不发送",
                                        )))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_ai_block(ix, auto, cx)
                                        })),
                                )
                        })),
                )
            })
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
///
/// SOLO: Read on the UI thread, so never on a network share (`\\server\...`,
/// which the shell's reported directory or a `.git` file could name): that
/// could hang and would connect to another machine.
pub(crate) fn git_branch(dir: &Path) -> Option<String> {
    if terminal::shell_integration::is_network_path(dir) {
        return None;
    }
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
        if git_dir
            .as_deref()
            .is_some_and(terminal::shell_integration::is_network_path)
        {
            return None;
        }
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
    completer: Rc<RefCell<CompleteFn>>,
    input: WeakEntity<TerminalInput>,
}

/// The editor's text and the byte offset of its newest cursor.
fn text_and_cursor(editor: &mut Editor, cx: &mut Context<Editor>) -> (String, usize) {
    let snapshot = editor.display_snapshot(cx);
    let head = editor
        .selections
        .newest::<language::Point>(&snapshot)
        .head();
    let buffer = editor.buffer().read(cx).snapshot(cx);
    let text = buffer.text();
    let cursor = buffer.point_to_offset(head).0;
    (text, cursor)
}

/// The menu's entries for `set`.
fn completion_response(
    set: CompletionSet,
    snapshot: &language::BufferSnapshot,
) -> CompletionResponse {
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
    CompletionResponse {
        completions,
        display_options: CompletionDisplayOptions::default(),
        is_incomplete: true,
    }
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
        window: &mut Window,
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
        if let Some(cached) = cached {
            return Task::ready(Ok(vec![completion_response(cached.set, &snapshot)]));
        }

        // SOLO: completing as you type (or refreshing an open menu). The
        // editor drops this task when the next key comes, so waiting first
        // only asks once typing pauses. Slow sources (PowerShell, spec
        // generators) don't hold up what Solo already knows.
        let completer = self.completer.borrow().clone();
        let last_completions = self.last_completions.clone();
        let terminal_input = self.input.clone();
        let request = CompletionRequest {
            shell,
            input,
            cursor,
            cwd,
            depth: CompletionDepth {
                quick: false,
                as_you_type: true,
            },
        };
        cx.spawn_in(window, async move |editor, cx| {
            cx.background_executor().timer(AS_YOU_TYPE_DELAY).await;
            let full = cx.update(|_, cx| completer(request.clone(), cx))?;
            let wait = cx.background_executor().timer(FULL_COMPLETIONS_WAIT);
            let set = match futures::future::select(full, wait).await {
                Either::Left((set, _)) => set?,
                Either::Right((_, full)) => {
                    let quick_request = CompletionRequest {
                        depth: CompletionDepth {
                            quick: true,
                            as_you_type: true,
                        },
                        ..request.clone()
                    };
                    let quick = cx
                        .update(|_, cx| completer(quick_request, cx))?
                        .await
                        .unwrap_or_default();
                    let showing_quick = !quick.items.is_empty();
                    // The rest replaces them when it comes, unless the input
                    // changed or the menu was closed.
                    cx.spawn(async move |cx| {
                        let Ok(set) = full.await else {
                            return;
                        };
                        if set.items.is_empty() {
                            return;
                        }
                        editor
                            .update_in(cx, |editor, window, cx| {
                                let (text, cursor) = text_and_cursor(editor, cx);
                                if text != request.input
                                    || cursor != request.cursor
                                    || (showing_quick && !editor.has_visible_completions_menu())
                                {
                                    return;
                                }
                                *last_completions.borrow_mut() = Some(CachedCompletions {
                                    input: text,
                                    cursor,
                                    set,
                                });
                                editor.show_completions(&ShowCompletions, window, cx);
                            })
                            .ok();
                    })
                    .detach();
                    quick
                }
            };
            if !set.items.is_empty() {
                terminal_input
                    .update(cx, |input, cx| input.auto_menu_opened(cx))
                    .ok();
            }
            Ok(vec![completion_response(set, &snapshot)])
        })
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: language::Anchor,
        text: &str,
        _trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool {
        // Completions open with Tab, like in a shell, unless they should open
        // as you type. SOLO: then only where they make sense (not on an
        // empty line, inside quotes or in a `#` request); a space after a
        // command with a spec lists what comes next.
        if !TerminalSettings::get_global(cx).completions_as_you_type || text.is_empty() {
            return false;
        }
        let snapshot = buffer.read(cx).snapshot();
        should_complete_as_you_type(&snapshot.text(), position.to_offset(&snapshot))
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

    #[test]
    fn test_git_branch_reads_local_repositories_only() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        std::fs::create_dir_all(repository.join("src")).unwrap();
        std::fs::write(repository.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(git_branch(&repository.join("src")).as_deref(), Some("main"));
        // A `.git` file pointing at a network share isn't followed.
        let linked = temp.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: //attacker/share/.git\n").unwrap();
        assert_eq!(git_branch(&linked), None);
        assert_eq!(git_branch(Path::new("//attacker/share/repo")), None);
    }

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

    /// Completions that answer `items` for the token at the end; the full
    /// request (PowerShell, generators) never answers when `hang` is set.
    fn set_completer(
        view: &Entity<TerminalView>,
        items: &'static [&'static str],
        hang: bool,
        cx: &mut VisualTestContext,
    ) {
        let completer: CompleteFn = Rc::new(move |request: CompletionRequest, cx: &App| {
            if hang && !request.depth.quick {
                return cx.background_spawn(futures::future::pending());
            }
            let start = request.input[..request.cursor]
                .rfind(' ')
                .map_or(0, |ix| ix + 1);
            Task::ready(Ok(CompletionSet {
                start,
                end: request.input.len(),
                items: items
                    .iter()
                    .map(|text| completion::CompletionItem {
                        text: text.to_string(),
                        label: text.to_string(),
                        kind: CompletionKind::Subcommand,
                        detail: None,
                    })
                    .collect(),
            }))
        });
        let input = input(view, cx);
        input.update(cx, |input, _| *input.completer.borrow_mut() = completer);
    }

    fn menu_visible(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> bool {
        let input = input(view, cx);
        input.read_with(cx, |input, cx| {
            input.editor.read(cx).has_visible_completions_menu()
        })
    }

    #[gpui::test]
    async fn test_spec_suggestion_without_history(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        let input = input(&view, cx);

        // Nothing in the history: the spec suggests the rest.
        cx.simulate_input("git ch");
        cx.run_until_parked();
        let suggestion = input.read_with(cx, |input, _| input.suggestion.clone());
        assert_eq!(suggestion.as_deref(), Some("eckout"));
        // Typing it keeps the rest showing.
        cx.simulate_input("e");
        let suggestion = input.read_with(cx, |input, _| input.suggestion.clone());
        assert_eq!(suggestion.as_deref(), Some("ckout"));
        cx.run_until_parked();

        // Tab takes it, like a completion.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "git checkout ");

        // The history comes first.
        cx.simulate_keystrokes("ctrl-a backspace");
        push_history(&view, &["git cherry-pick main"], cx);
        cx.simulate_input("git ch");
        cx.run_until_parked();
        let suggestion = input.read_with(cx, |input, _| input.suggestion.clone());
        assert_eq!(suggestion.as_deref(), Some("erry-pick main"));
    }

    #[gpui::test]
    async fn test_completions_open_as_you_type_without_blocking(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        // PowerShell never answers; what Solo knows shows anyway.
        set_completer(&view, &["status", "stash"], true, cx);
        take_input_log(&terminal, cx);

        cx.simulate_input("foo st");
        cx.run_until_parked();
        // Nothing opens before typing pauses.
        assert!(!menu_visible(&view, cx));
        cx.executor().advance_clock(AS_YOU_TYPE_DELAY);
        cx.run_until_parked();
        assert!(!menu_visible(&view, cx));
        cx.executor().advance_clock(FULL_COMPLETIONS_WAIT);
        cx.run_until_parked();
        assert!(menu_visible(&view, cx));

        // Typing goes on as usual.
        cx.simulate_input("a");
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "foo sta");

        // Enter runs the command rather than taking a completion.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(take_input_log(&terminal, cx), ["foo sta\r"]);
        assert_eq!(input_text(&view, cx), "");
        assert!(!menu_visible(&view, cx));
    }

    #[gpui::test]
    async fn test_no_completions_as_you_type_in_quotes_or_requests(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        set_completer(&view, &["status", "stash"], false, cx);

        for text in ["foo 'st", "# st"] {
            cx.simulate_input(text);
            cx.executor().advance_clock(AS_YOU_TYPE_DELAY);
            cx.run_until_parked();
            cx.executor().advance_clock(FULL_COMPLETIONS_WAIT);
            cx.run_until_parked();
            assert!(!menu_visible(&view, cx), "{text}");
            cx.simulate_keystrokes("ctrl-a backspace");
        }
    }

    #[gpui::test]
    async fn test_enter_accepts_in_menu_opened_with_tab(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        set_completer(&view, &["status", "stash"], false, cx);
        take_input_log(&terminal, cx);

        cx.simulate_input("foo st");
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert!(menu_visible(&view, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        // The menu's first entry (as it sorts them) is taken.
        let text = input_text(&view, cx);
        assert!(text == "foo status" || text == "foo stash", "{text}");
        assert!(take_input_log(&terminal, cx).is_empty());
    }

    /// A generator that answers when the test says so, and records the
    /// requests it got.
    #[allow(clippy::type_complexity)]
    fn set_generator(
        view: &Entity<TerminalView>,
        cx: &mut VisualTestContext,
    ) -> (
        Rc<RefCell<Vec<ai_command::Request>>>,
        Rc<RefCell<Vec<futures::channel::oneshot::Sender<String>>>>,
    ) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let answers = Rc::new(RefCell::new(Vec::new()));
        let generator: GenerateFn = Rc::new({
            let requests = requests.clone();
            let answers = answers.clone();
            move |request, _cancel, cx: &App| {
                requests.borrow_mut().push(request);
                let (send, receive) = futures::channel::oneshot::channel::<String>();
                answers.borrow_mut().push(send);
                cx.background_spawn(async move { Ok(receive.await?) })
            }
        });
        let input = input(view, cx);
        input.update(cx, |input, _| input.generator = generator);
        (requests, answers)
    }

    #[gpui::test]
    async fn test_cancelled_generation_fills_nothing(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        let (_, answers) = set_generator(&view, cx);
        let input = input(&view, cx);

        cx.simulate_input("# list files");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.generating.is_some()));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.generating.is_none()));

        // The answer coming after all doesn't replace the request.
        let answer = answers.borrow_mut().pop().unwrap();
        answer.send("ls".to_string()).ok();
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "# list files");

        // Without cancelling, the command fills the input for review.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let answer = answers.borrow_mut().pop().unwrap();
        answer.send("Get-ChildItem".to_string()).ok();
        cx.run_until_parked();
        assert_eq!(input_text(&view, cx), "Get-ChildItem");
    }

    #[gpui::test]
    async fn test_request_carries_the_last_failure(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        write(
            &terminal,
            b"cargo build\r\n\x1b]633;E;cargo build;test-nonce\x07\x1b]133;C\x07\
              error[E0425]: cannot find value `x` in this scope\r\n\
              \x1b]133;D;101\x07\x1b]133;A\x07$ \x1b]133;B\x07",
            cx,
        );
        let (requests, answers) = set_generator(&view, cx);

        cx.simulate_input("# fix that error");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let request = requests.borrow().last().cloned().unwrap();
        assert_eq!(request.request, "fix that error");
        assert_eq!(request.context.len(), 1);
        assert_eq!(request.context[0].command.trim(), "cargo build");
        assert!(request.context[0].output.contains("E0425"));
        assert_eq!(request.context[0].exit_code, Some(101));
        answers.borrow_mut().pop().unwrap().send("x".into()).ok();
        cx.run_until_parked();

        // Taken off, it isn't sent.
        let input = input(&view, cx);
        input.update(cx, |input, cx| {
            let (block, auto) = input.ai_context_blocks(cx).pop().unwrap();
            assert!(auto);
            input.remove_ai_block(block.ix, auto, cx);
        });
        cx.simulate_keystrokes("ctrl-a backspace");
        cx.simulate_input("# fix that error");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(requests.borrow().last().unwrap().context.is_empty());
    }

    #[gpui::test]
    async fn test_attaching_a_selected_block(cx: &mut TestAppContext) {
        let (terminal, view, cx) = terminal_with_view(cx).await;
        write(&terminal, SHELL_PROMPT, cx);
        write(
            &terminal,
            b"git status\r\n\x1b]633;E;git status;test-nonce\x07\x1b]133;C\x07\
              On branch main\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
            cx,
        );
        let (requests, _answers) = set_generator(&view, cx);

        // Select the block and press ctrl-i: a `#` request with it attached.
        cx.simulate_keystrokes("ctrl-up");
        draw(cx);
        assert!(view.read_with(cx, |view, _| view.selected_command_block().is_some()));
        cx.simulate_keystrokes("ctrl-i");
        draw(cx);
        assert!(input_focused(&view, cx));
        assert_eq!(input_text(&view, cx), "# ");
        cx.simulate_input("explain the branch");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let request = requests.borrow().last().cloned().unwrap();
        assert_eq!(request.context.len(), 1);
        assert_eq!(request.context[0].command.trim(), "git status");
        assert!(request.context[0].output.contains("On branch main"));
    }
}
