//! Completions from command specs: Warp's command signatures (vendored, MIT
//! licensed, see `vendor/warp_command_signatures`), which describe the
//! subcommands, options and arguments of ~500 commands, plus generators that
//! produce argument values by running a command (git branches, npm scripts,
//! docker containers, ...).
//!
//! [`plan`] walks the typed tokens through a command's spec and works out what
//! to suggest for the token under the cursor. Static suggestions come back
//! directly; generators are returned as commands for the caller to run in the
//! user's shell, whose output [`GeneratorRun::results`] turns into suggestions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use warp_command_signatures::{
    ArgumentType, DynamicCompletionData, GeneratorName, GeneratorProcess, IconType, Opt, Priority,
    Signature, Suggestion, TemplateFilter, TemplateType, dynamic_command_signature_data,
    signature_by_name,
};

use super::completion::{CompletionItem, CompletionKind, ShellFlavor};

/// The spec of `command`, by program name (`git`, `git.exe`, `C:\..\git.exe`).
pub fn signature(command: &str) -> Option<Arc<Signature>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Signature>>>>> = OnceLock::new();
    let name = command_name(command)?;
    let cache = CACHE.get_or_init(Default::default);
    if let Some(cached) = cache.lock().unwrap().get(&name) {
        return cached.clone();
    }
    let signature = signature_by_name(&name)
        .or_else(|| {
            // PowerShell cmdlets are specced with their casing (`Get-ChildItem`).
            let file_name = Path::new(command)
                .file_stem()?
                .to_string_lossy()
                .into_owned();
            (file_name != name)
                .then(|| signature_by_name(&file_name))
                .flatten()
        })
        .map(Arc::new);
    cache.lock().unwrap().insert(name, signature.clone());
    signature
}

/// Lowercased program name without directory or `.exe`-like extension.
pub fn command_name(command: &str) -> Option<String> {
    let command = command.trim_matches(['"', '\'']);
    let path = Path::new(command);
    let name = if path.extension().is_some_and(|ext| {
        matches!(
            ext.to_string_lossy().to_lowercase().as_str(),
            "exe" | "cmd" | "bat" | "ps1" | "com"
        )
    }) {
        path.file_stem()?
    } else {
        path.file_name()?
    };
    let name = name.to_string_lossy().to_lowercase();
    (!name.is_empty()).then_some(name)
}

/// One-line descriptions of all specced commands, for the command position.
/// Parsing every spec takes a moment, so this is `None` until a background
/// thread finishes the first time it is asked for.
pub fn command_descriptions() -> Option<&'static HashMap<String, String>> {
    static DESCRIPTIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
    static STARTED: std::sync::Once = std::sync::Once::new();
    if let Some(descriptions) = DESCRIPTIONS.get() {
        return Some(descriptions);
    }
    STARTED.call_once(|| {
        std::thread::Builder::new()
            .name("solo-command-descriptions".into())
            .spawn(|| {
                let descriptions = warp_command_signatures::commands()
                    .into_iter()
                    .filter_map(|signature| {
                        Some((signature.name.to_lowercase(), signature.description?))
                    })
                    .collect();
                DESCRIPTIONS.set(descriptions).ok();
            })
            .ok();
    });
    None
}

fn dynamic_data(command: &str) -> Option<&'static DynamicCompletionData> {
    static DATA: OnceLock<HashMap<String, DynamicCompletionData>> = OnceLock::new();
    DATA.get_or_init(dynamic_command_signature_data)
        .get(command)
}

/// What to suggest for the token under the cursor.
#[derive(Default)]
pub struct Plan {
    /// Static suggestions: subcommands, options and argument values.
    pub items: Vec<CompletionItem>,
    /// Commands to run for generated argument values.
    pub generators: Vec<GeneratorRun>,
    /// Paths to suggest, if the argument takes them.
    pub paths: Option<PathPlan>,
    /// The argument is itself a command (`sudo <command>`).
    pub commands: bool,
    /// Text before the cursor's token that suggestions don't replace, e.g.
    /// `--output=` when completing the value of `--output=`.
    pub value_prefix_len: usize,
}

pub struct PathPlan {
    pub folders_only: bool,
    filter: Option<TemplateFilter>,
}

impl PathPlan {
    /// Applies the spec's filter (e.g. only `*.json`) to a path suggestion.
    pub fn filter(&self, item: CompletionItem) -> Option<CompletionItem> {
        let Some(filter) = &self.filter else {
            return Some(item);
        };
        let kind = if item.kind == CompletionKind::Directory {
            warp_command_signatures::PathSuggestionType::Folder
        } else {
            warp_command_signatures::PathSuggestionType::File
        };
        let suggestion = Suggestion::new(item.text.clone());
        filter
            .filter(suggestion, kind)
            .map(|suggestion| CompletionItem {
                text: suggestion.exact_string,
                ..item
            })
    }
}

pub struct GeneratorRun {
    /// Shell command to run in the working directory.
    pub command: String,
    on_complete: fn(&str) -> warp_command_signatures::GeneratorResults,
}

impl GeneratorRun {
    pub fn results(&self, output: &str) -> Vec<CompletionItem> {
        let results = (self.on_complete)(output);
        let mut suggestions = results.suggestions;
        if !results.is_ordered {
            suggestions.sort_by(|a, b| a.exact_string.cmp(&b.exact_string));
        }
        sort_by_priority(&mut suggestions, |suggestion| suggestion.priority);
        suggestions
            .into_iter()
            .filter(|suggestion| !suggestion.is_hidden)
            .map(|suggestion| suggestion_item(suggestion, CompletionKind::Value))
            .collect()
    }
}

/// Works out the suggestions for the last of `tokens` (the one under the
/// cursor, possibly empty), given the ones before it.
pub fn plan(tokens: &[&str], flavor: ShellFlavor) -> Option<Plan> {
    let (current, before) = tokens.split_last()?;
    let (command, args) = before.split_first()?;
    let mut root = signature(command)?;
    let mut root_name = command_name(command)?;
    let mut path: Vec<usize> = Vec::new();
    let mut state = WalkState::default();

    let mut ix = 0;
    while ix < args.len() {
        let token = args[ix];
        ix += 1;
        let signature = resolve(&root, &path);
        if let Some(pending) = state.pending_option.take() {
            let (option, arg_ix) = pending;
            let option = &signature.options()[option];
            if arg_ix + 1 < option.arguments().len() {
                state.pending_option = Some((option_index(signature, option), arg_ix + 1));
            }
            continue;
        }
        if token == "--" {
            state.options_done = true;
            continue;
        }
        if !state.options_done && token.len() > 1 && token.starts_with('-') {
            let (name, value) = match token.split_once('=') {
                Some((name, _)) => (name, true),
                None => (token, false),
            };
            if let Some(option) = find_option(signature, name) {
                state.used_options.push(option.exact_string.clone());
                if !value
                    && option
                        .arguments()
                        .first()
                        .is_some_and(|argument| argument.is_required())
                {
                    state.pending_option = Some((option_index(signature, option), 0));
                }
            }
            continue;
        }
        if state.arg_index == 0
            && let Some(sub) = signature
                .subcommands()
                .iter()
                .position(|sub| names_match(&sub.name, token, signature))
        {
            path.push(sub);
            state = WalkState::default();
            continue;
        }
        let arguments = signature.arguments();
        let argument = arguments
            .get(state.arg_index)
            .or_else(|| arguments.last().filter(|argument| argument.is_variadic()));
        match argument {
            // `sudo git ...`: continue with the nested command's spec.
            Some(argument) if argument.is_command() => {
                let nested = signature_from_token(token)?;
                root = nested;
                root_name = command_name(token)?;
                path.clear();
                state = WalkState::default();
            }
            Some(argument) if argument.is_variadic() => {}
            _ => state.arg_index += 1,
        }
    }

    let signature = resolve(&root, &path);
    let data = dynamic_data(&root_name);
    let mut plan = Plan::default();
    let shell = match flavor {
        ShellFlavor::PowerShell => warp_command_signatures::Shell::Powershell,
        ShellFlavor::Posix => warp_command_signatures::Shell::Posix,
    };
    let add_argument = |plan: &mut Plan, argument: &warp_command_signatures::Argument| {
        if argument.is_command() {
            plan.commands = true;
        }
        for argument_type in &argument.argument_types {
            match argument_type {
                ArgumentType::Suggestion(suggestion) => {
                    if suggestion.is_hidden && suggestion.exact_string != *current {
                        continue;
                    }
                    plan.items
                        .push(suggestion_item(suggestion.clone(), CompletionKind::Value));
                }
                ArgumentType::Template(template) => {
                    let folders_only = matches!(template.type_name, TemplateType::Folders { .. });
                    let filter = template
                        .filter_name
                        .as_ref()
                        .and_then(|name| data.and_then(|data| data.filters().get(name)).cloned());
                    match &mut plan.paths {
                        Some(paths) => paths.folders_only &= folders_only,
                        None => {
                            plan.paths = Some(PathPlan {
                                folders_only,
                                filter,
                            })
                        }
                    }
                }
                ArgumentType::Generator(name) => {
                    if let Some(run) = generator_run(data, name, tokens, shell) {
                        plan.generators.push(run);
                    }
                }
                ArgumentType::Alias(_) => {}
            }
        }
    };

    if let Some((option, arg_ix)) = state.pending_option {
        let option = &signature.options()[option];
        if let Some(argument) = option.arguments().get(arg_ix) {
            add_argument(&mut plan, argument);
        }
        return Some(plan);
    }

    if !state.options_done && current.starts_with('-') {
        // `--output=<value>` completes the value.
        if let Some((name, _)) = current.split_once('=')
            && let Some(option) = find_option(signature, name)
            && let Some(argument) = option.arguments().first()
        {
            plan.value_prefix_len = name.len() + 1;
            add_argument(&mut plan, argument);
            return Some(plan);
        }
        let mut options: Vec<&Opt> = signature
            .options()
            .iter()
            .filter(|option| !state.used_options.contains(&option.exact_string))
            .collect();
        sort_by_priority(&mut options, |option| option.priority);
        for option in options {
            for name in option.names() {
                plan.items.push(CompletionItem {
                    text: name.to_string(),
                    label: option_label(name, option),
                    kind: CompletionKind::Parameter,
                    detail: option.description.clone(),
                });
            }
        }
        return Some(plan);
    }

    if state.arg_index == 0 {
        let mut subcommands: Vec<&Signature> = signature.subcommands().iter().collect();
        sort_by_priority(&mut subcommands, |sub| sub.priority);
        for sub in subcommands {
            plan.items.push(CompletionItem {
                text: sub.name.clone(),
                label: sub.name.clone(),
                kind: CompletionKind::Subcommand,
                detail: sub.description.clone(),
            });
        }
    }
    let arguments = signature.arguments();
    if let Some(argument) = arguments
        .get(state.arg_index)
        .or_else(|| arguments.last().filter(|argument| argument.is_variadic()))
    {
        add_argument(&mut plan, argument);
    }
    Some(plan)
}

#[derive(Default)]
struct WalkState {
    /// Index of the next positional argument.
    arg_index: usize,
    /// An option still waiting for its argument: (option index, argument index).
    pending_option: Option<(usize, usize)>,
    /// After `--`, everything is positional.
    options_done: bool,
    used_options: Vec<Vec<String>>,
}

fn resolve<'a>(root: &'a Signature, path: &[usize]) -> &'a Signature {
    path.iter()
        .fold(root, |signature, &ix| &signature.subcommands()[ix])
}

fn signature_from_token(token: &str) -> Option<Arc<Signature>> {
    signature(token)
}

fn option_index(signature: &Signature, option: &Opt) -> usize {
    signature
        .options()
        .iter()
        .position(|candidate| std::ptr::eq(candidate, option))
        .unwrap_or_default()
}

fn find_option<'a>(signature: &'a Signature, name: &str) -> Option<&'a Opt> {
    let directives = &signature.parser_directives;
    let exact = signature.options().iter().find(|option| {
        option
            .names()
            .any(|candidate| names_equal(candidate, name, directives.always_case_insensitive))
    });
    if exact.is_some() || !directives.flags_match_unique_prefix {
        return exact;
    }
    // PowerShell accepts unambiguous prefixes: `-Rec` for `-Recurse`.
    let lower = name.to_lowercase();
    let mut matches = signature.options().iter().filter(|option| {
        option
            .names()
            .any(|candidate| candidate.to_lowercase().starts_with(&lower))
    });
    let first = matches.next();
    matches.next().is_none().then_some(first).flatten()
}

fn names_match(spec_name: &str, token: &str, parent: &Signature) -> bool {
    names_equal(
        spec_name,
        token,
        parent.parser_directives.always_case_insensitive,
    )
}

fn names_equal(a: &str, b: &str, case_insensitive: bool) -> bool {
    if case_insensitive || cfg!(windows) && a.starts_with(char::is_uppercase) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn option_label(name: &str, option: &Opt) -> String {
    match option
        .arguments()
        .first()
        .and_then(|argument| argument.display_name.as_deref())
    {
        Some(argument) => format!("{name} <{argument}>"),
        None => name.to_string(),
    }
}

fn generator_run(
    data: Option<&DynamicCompletionData>,
    name: &GeneratorName,
    tokens: &[&str],
    shell: warp_command_signatures::Shell,
) -> Option<GeneratorRun> {
    let generator = data?.generators().get(name)?;
    let command = match &generator.process {
        GeneratorProcess::ShellCommand(builder) => builder.build(shell).into_owned(),
        GeneratorProcess::CommandFromTokens(command_from_tokens) => {
            let trailing_whitespace = tokens.last().is_some_and(|token| token.is_empty());
            let tokens: Vec<&str> = tokens
                .iter()
                .copied()
                .filter(|token| !token.is_empty())
                .collect();
            command_from_tokens(&tokens, trailing_whitespace, &[])
                .build(shell)
                .into_owned()
        }
    };
    Some(GeneratorRun {
        command,
        on_complete: generator.on_complete_callback,
    })
}

fn suggestion_item(suggestion: Suggestion, default_kind: CompletionKind) -> CompletionItem {
    let kind = match suggestion.icon {
        Some(IconType::GitBranch) => CompletionKind::Branch,
        Some(IconType::File) => CompletionKind::File,
        Some(IconType::Folder) => CompletionKind::Directory,
        Some(IconType::DockerContainer | IconType::DockerImage) => CompletionKind::Container,
        Some(IconType::KubePod | IconType::KubeCluster) => CompletionKind::Container,
        None => default_kind,
    };
    CompletionItem {
        label: suggestion
            .display_name
            .clone()
            .unwrap_or_else(|| suggestion.exact_string.clone()),
        text: suggestion.exact_string,
        kind,
        detail: suggestion.description,
    }
}

/// Stable sort, most important first.
fn sort_by_priority<T>(items: &mut [T], priority: impl Fn(&T) -> Priority) {
    items.sort_by_key(|item| std::cmp::Reverse(priority(item)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(plan: &Plan) -> Vec<&str> {
        plan.items.iter().map(|item| item.text.as_str()).collect()
    }

    #[test]
    fn test_subcommands_and_options() {
        let plan = plan(&["git", ""], ShellFlavor::Posix).unwrap();
        assert!(texts(&plan).contains(&"checkout"));
        assert!(texts(&plan).contains(&"commit"));

        let plan = super::plan(&["git", "commit", "--a"], ShellFlavor::Posix).unwrap();
        assert!(texts(&plan).contains(&"--amend"));
        let amend = plan
            .items
            .iter()
            .find(|item| item.text == "--amend")
            .unwrap();
        assert!(amend.detail.is_some());
    }

    #[test]
    fn test_generators() {
        let plan = plan(&["git", "checkout", ""], ShellFlavor::Posix).unwrap();
        assert!(!plan.generators.is_empty());
        assert!(plan.generators[0].command.contains("git"));
    }

    #[test]
    fn test_option_arguments() {
        // `git commit -m` takes a message, not subcommands or files.
        let plan = plan(&["git", "commit", "-m", ""], ShellFlavor::Posix).unwrap();
        assert!(!texts(&plan).contains(&"checkout"));
        // `git -C <path>` takes folders.
        let plan = super::plan(&["git", "-C", ""], ShellFlavor::Posix).unwrap();
        assert!(plan.paths.as_ref().is_some_and(|paths| paths.folders_only));
    }

    #[test]
    fn test_nested_command() {
        let plan = plan(&["sudo", "git", ""], ShellFlavor::Posix).unwrap();
        assert!(texts(&plan).contains(&"checkout"));
    }

    #[test]
    fn test_unknown_command() {
        assert!(plan(&["no-such-command-xyz", ""], ShellFlavor::Posix).is_none());
    }

    #[test]
    fn test_command_name() {
        assert_eq!(
            command_name(r"C:\Program Files\Git\cmd\git.exe").as_deref(),
            Some("git")
        );
        assert_eq!(command_name("npm.cmd").as_deref(), Some("npm"));
        assert_eq!(
            command_name("docker-compose").as_deref(),
            Some("docker-compose")
        );
    }
}
