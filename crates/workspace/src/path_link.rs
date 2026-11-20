use crate::Workspace;
use gpui::{App, AppContext, Entity, Task, WeakEntity};
use itertools::Itertools;
use project::{Entry, Worktree};
use std::path::{Path, PathBuf};
use util::{
    paths::{PathStyle, PathWithPosition, normalize_lexically},
    rel_path::RelPath,
};

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum OpenTargetFoundBy {
    WorktreeExact,
    WorktreeScan,
    BackgroundPathResolution,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BackgroundPathChecks {
    LocalFileSystem,
    ProjectPathResolution,
}

/// How a path-like string gets matched against files by [`resolve_open_target`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PathMatching {
    /// Only exact matches (absolute, `cwd`-relative, or worktree-root-relative), so broken document links resolve to `None`.
    Exact,
    /// Additionally guess terminal-output paths: strip git diff `a/`/`b/` prefixes and scan worktrees for trailing path matches.
    Heuristic,
}

#[derive(Debug, Clone)]
pub enum OpenTarget {
    Worktree(
        PathWithPosition,
        Entry,
        #[cfg(any(test, feature = "test-support"))] OpenTargetFoundBy,
    ),
    Path(
        PathWithPosition,
        bool,
        #[cfg(any(test, feature = "test-support"))] OpenTargetFoundBy,
    ),
}

impl OpenTarget {
    pub fn is_file(&self) -> bool {
        match self {
            OpenTarget::Worktree(_, entry, ..) => entry.is_file(),
            OpenTarget::Path(_, is_dir, ..) => !is_dir,
        }
    }

    pub fn is_dir(&self) -> bool {
        match self {
            OpenTarget::Worktree(_, entry, ..) => entry.is_dir(),
            OpenTarget::Path(_, is_dir, ..) => *is_dir,
        }
    }

    pub fn path(&self) -> &PathWithPosition {
        match self {
            OpenTarget::Worktree(path, ..) => path,
            OpenTarget::Path(path, ..) => path,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn found_by(&self) -> OpenTargetFoundBy {
        match self {
            OpenTarget::Worktree(.., found_by) => *found_by,
            OpenTarget::Path(.., found_by) => *found_by,
        }
    }
}

/// SOLO: where a file of the name a path link ends with stands among the
/// others of that name, best first (see [`CandidateRank::sort_key`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CandidateRank {
    /// It's in the terminal's folder (or the folder the agent in it said
    /// it works in).
    pub under_working_directory: bool,
    /// How near it is to the paths written around the link: the number of
    /// leading folders it shares with each, summed.
    pub context_score: usize,
    /// How many leading folders it shares with the terminal's folder.
    pub shared_with_working_directory: usize,
    /// It's in a folder of someone else's code (`ThirdParty`, `vendor`, ...).
    pub third_party: bool,
    /// Whether the file has the line the link names: `None` until checked
    /// (it's read from disk, so only the first few candidates are).
    pub has_row: Option<bool>,
    /// How many components its path has.
    pub components: usize,
}

impl CandidateRank {
    /// What decides the order, greatest first: in the terminal's folder,
    /// near the paths around the link, near the terminal's folder, not
    /// someone else's code, has the line, short.
    pub fn sort_key(&self) -> (bool, usize, usize, bool, bool, std::cmp::Reverse<usize>) {
        (
            self.under_working_directory,
            self.context_score,
            self.shared_with_working_directory,
            !self.third_party,
            self.has_row != Some(false),
            std::cmp::Reverse(self.components),
        )
    }

    /// The part of [`Self::sort_key`] that tells where the agent is and
    /// what it wrote about: a candidate better here than the next one is
    /// the one meant. The rest only breaks ties, so a tie here is a guess.
    fn decisive_key(&self) -> (bool, usize, usize) {
        (
            self.under_working_directory,
            self.context_score,
            self.shared_with_working_directory,
        )
    }
}

/// SOLO: a file a path link may mean, and why it ranks where it does.
#[derive(Clone, Debug)]
pub struct LinkCandidate {
    pub target: OpenTarget,
    pub rank: CandidateRank,
}

/// SOLO: what a path link resolves to: the target it opens, and when files
/// of its name were searched for, all of them best first.
#[derive(Clone, Debug)]
pub struct ResolvedLink {
    /// What clicking the link opens: the first candidate.
    pub target: OpenTarget,
    /// Every file the link may mean, best first; just `target` when only
    /// one may be meant.
    pub candidates: Vec<LinkCandidate>,
    /// The first candidate doesn't clearly win over the second ("猜不准"):
    /// only tie-breakers (someone else's code, the line, the length) put it
    /// first, so the user is better asked which one.
    pub ambiguous: bool,
}

impl ResolvedLink {
    fn single(target: OpenTarget) -> Self {
        Self {
            candidates: vec![LinkCandidate {
                target: target.clone(),
                rank: CandidateRank::default(),
            }],
            target,
            ambiguous: false,
        }
    }

    /// Orders `candidates` by their ranks and decides whether the first is
    /// a guess. `None` without candidates.
    pub fn from_candidates(mut candidates: Vec<LinkCandidate>) -> Option<Self> {
        // Stable: equal ranks keep the order they were found in.
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.rank.sort_key()));
        let first = candidates.first()?;
        let ambiguous = candidates
            .get(1)
            .is_some_and(|second| second.rank.decisive_key() == first.rank.decisive_key());
        Some(Self {
            target: first.target.clone(),
            candidates,
            ambiguous,
        })
    }
}

/// SOLO: folders of someone else's code, whose files of a name rank after
/// the project's own.
const THIRD_PARTY_FOLDERS: &[&str] = &[
    "thirdparty",
    "third_party",
    "extras",
    "external",
    "vendor",
    "node_modules",
];

fn is_third_party(path: &RelPath) -> bool {
    path.components().any(|component| {
        THIRD_PARTY_FOLDERS
            .iter()
            .any(|folder| component.eq_ignore_ascii_case(folder))
    })
}

pub fn sanitize_path_text(text: &str) -> &str {
    let start = first_unbalanced_open_paren(text).unwrap_or(0);
    let mut sanitized = &text[start..];
    let (open_parens, mut close_parens) =
        sanitized
            .chars()
            .fold((0, 0), |(opens, closes), character| match character {
                '(' => (opens + 1, closes),
                ')' => (opens, closes + 1),
                _ => (opens, closes),
            });

    while let Some(last_char) = sanitized.chars().last() {
        let should_remove = match last_char {
            '.' | ',' | ':' | ';' => true,
            '(' => true,
            ')' if close_parens > open_parens => {
                close_parens -= 1;
                true
            }
            _ => false,
        };

        if should_remove {
            sanitized = &sanitized[..sanitized.len() - last_char.len_utf8()];
        } else {
            break;
        }
    }

    sanitized
}

/// Returns the byte offset just past the first unbalanced `(` in `text`, or
/// `None` if all parentheses are balanced.
pub fn first_unbalanced_open_paren(text: &str) -> Option<usize> {
    let mut balance: i32 = 0;
    let mut first_unmatched = None;
    for (index, character) in text.char_indices() {
        match character {
            '(' => {
                if balance == 0 {
                    first_unmatched = Some(index + character.len_utf8());
                }
                balance += 1;
            }
            ')' => {
                balance -= 1;
                if balance <= 0 {
                    balance = 0;
                    first_unmatched = None;
                }
            }
            _ => {}
        }
    }
    first_unmatched.filter(|_| balance > 0)
}

pub fn resolve_open_target(
    workspace: &WeakEntity<Workspace>,
    matching: PathMatching,
    maybe_path: &str,
    cwd: Option<&Path>,
    cx: &App,
) -> Task<Option<OpenTarget>> {
    let resolved = resolve_open_target_internal(workspace, matching, maybe_path, cwd, &[], cx, None);
    cx.background_spawn(async move { Some(resolved.await?.target) })
}

/// SOLO: [`resolve_open_target`] for a path written among `context_paths`,
/// with every file it may mean: when several files have the name it ends
/// with, they're ranked (see [`CandidateRank`]), so the one nearest to
/// those paths comes first (`register_types.cpp` next to
/// `dynamic_bone_collider_3d.h` is `modules/dynamicbone/register_types.cpp`).
pub fn resolve_link_near(
    workspace: &WeakEntity<Workspace>,
    matching: PathMatching,
    maybe_path: &str,
    cwd: Option<&Path>,
    context_paths: &[String],
    cx: &App,
) -> Task<Option<ResolvedLink>> {
    resolve_open_target_internal(
        workspace,
        matching,
        maybe_path,
        cwd,
        context_paths,
        cx,
        None,
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn resolve_link_with_fs_checks(
    workspace: &WeakEntity<Workspace>,
    matching: PathMatching,
    maybe_path: &str,
    cwd: Option<&Path>,
    context_paths: &[String],
    cx: &App,
    background_path_checks: BackgroundPathChecks,
) -> Task<Option<ResolvedLink>> {
    resolve_open_target_internal(
        workspace,
        matching,
        maybe_path,
        cwd,
        context_paths,
        cx,
        Some(background_path_checks),
    )
}

fn resolve_open_target_internal(
    workspace: &WeakEntity<Workspace>,
    matching: PathMatching,
    maybe_path: &str,
    cwd: Option<&Path>,
    context_paths: &[String],
    cx: &App,
    background_path_checks: Option<BackgroundPathChecks>,
) -> Task<Option<ResolvedLink>> {
    let Some(workspace) = workspace.upgrade() else {
        return Task::ready(None);
    };

    let context_paths = context_paths.to_vec();
    let mut potential_paths = Vec::new();
    let original_path = PathWithPosition::from_path(PathBuf::from(maybe_path));
    let path_with_position = PathWithPosition::parse_str(maybe_path);
    // SOLO: `res://` and `%APPDATA%` paths, which are only looked up on disk.
    let special_paths = matches!(matching, PathMatching::Heuristic)
        .then(|| special_paths(&path_with_position, cwd))
        .unwrap_or_default();
    // SOLO: a position without a file, like the `:161` of "(:161)", names no
    // file; an empty path would be the worktree's root.
    if path_with_position.path.as_os_str().is_empty() || maybe_path.starts_with(':') {
        return Task::ready(None);
    }
    let worktree_candidates = workspace
        .read(cx)
        .worktrees(cx)
        .sorted_by_key(|worktree| {
            let worktree_root = worktree.read(cx).abs_path();
            match cwd.and_then(|cwd| worktree_root.strip_prefix(cwd).ok()) {
                Some(cwd_child) => cwd_child.components().count(),
                None => usize::MAX,
            }
        })
        .collect::<Vec<_>>();

    let prefixes_to_strip: &[&str] = match matching {
        PathMatching::Heuristic => &["a", "b", "."],
        PathMatching::Exact => &["."],
    };
    for prefix_str in prefixes_to_strip {
        if let Some(stripped) = original_path.path.strip_prefix(prefix_str).ok() {
            potential_paths.push(PathWithPosition {
                path: stripped.to_owned(),
                row: original_path.row,
                column: original_path.column,
            });
        }
        if let Some(stripped) = path_with_position.path.strip_prefix(prefix_str).ok() {
            potential_paths.push(PathWithPosition {
                path: stripped.to_owned(),
                row: path_with_position.row,
                column: path_with_position.column,
            });
        }
    }

    let insert_both_paths = original_path != path_with_position;
    potential_paths.insert(0, original_path);
    if insert_both_paths {
        potential_paths.insert(1, path_with_position);
    }

    let mut worktree_paths_to_check = Vec::new();
    let mut is_cwd_in_worktree = false;
    let mut open_target = None;
    'worktree_loop: for worktree in &worktree_candidates {
        let worktree_root = worktree.read(cx).abs_path();
        let mut paths_to_check = Vec::with_capacity(potential_paths.len());
        let relative_cwd = cwd
            .and_then(|cwd| cwd.strip_prefix(&worktree_root).ok())
            .and_then(|cwd| RelPath::new(cwd, PathStyle::local()).ok())
            .and_then(|cwd_stripped| {
                (cwd_stripped.as_ref() != RelPath::empty()).then(|| {
                    is_cwd_in_worktree = true;
                    cwd_stripped
                })
            });

        for path_with_position in &potential_paths {
            let path_to_check = if worktree_root.ends_with(&path_with_position.path) {
                let root_path_with_position = PathWithPosition {
                    path: worktree_root.to_path_buf(),
                    row: path_with_position.row,
                    column: path_with_position.column,
                };
                match worktree.read(cx).root_entry() {
                    Some(root_entry) => {
                        open_target = Some(OpenTarget::Worktree(
                            root_path_with_position,
                            root_entry.clone(),
                            #[cfg(any(test, feature = "test-support"))]
                            OpenTargetFoundBy::WorktreeExact,
                        ));
                        break 'worktree_loop;
                    }
                    None => root_path_with_position,
                }
            } else {
                PathWithPosition {
                    path: path_with_position
                        .path
                        .strip_prefix(&worktree_root)
                        .unwrap_or(&path_with_position.path)
                        .to_owned(),
                    row: path_with_position.row,
                    column: path_with_position.column,
                }
            };

            let normalized_path = if path_to_check.path.is_relative() {
                relative_cwd.as_ref().and_then(|relative_cwd| {
                    let joined = relative_cwd
                        .as_ref()
                        .as_std_path()
                        .join(&path_to_check.path);
                    normalize_lexically(&joined).ok().and_then(|path| {
                        RelPath::new(&path, PathStyle::local())
                            .ok()
                            .map(std::borrow::Cow::into_owned)
                    })
                })
            } else {
                None
            };
            let original_path = RelPath::new(&path_to_check.path, PathStyle::local()).ok();

            if !worktree.read(cx).is_single_file()
                && let Some(entry) = normalized_path
                    .as_ref()
                    .and_then(|path| worktree.read(cx).entry_for_path(path))
                    .or_else(|| {
                        original_path
                            .as_ref()
                            .and_then(|path| worktree.read(cx).entry_for_path(path.as_ref()))
                    })
            {
                open_target = Some(OpenTarget::Worktree(
                    PathWithPosition {
                        path: worktree.read(cx).absolutize(&entry.path),
                        row: path_to_check.row,
                        column: path_to_check.column,
                    },
                    entry.clone(),
                    #[cfg(any(test, feature = "test-support"))]
                    OpenTargetFoundBy::WorktreeExact,
                ));
                break 'worktree_loop;
            }

            paths_to_check.push(path_to_check);
        }

        if !paths_to_check.is_empty() {
            worktree_paths_to_check.push((worktree.clone(), paths_to_check));
        }
    }

    if open_target.is_some() {
        if is_cwd_in_worktree {
            return Task::ready(open_target.map(ResolvedLink::single));
        }
    }

    let project = workspace.read(cx).project().clone();
    // SOLO: a terminal (or the agent in it) working in a folder no worktree
    // holds has its bare file names (`object.h:163`) looked for there too.
    let outside_folder_search = (matching == PathMatching::Heuristic
        && project.read(cx).is_local())
    .then(|| cwd.map(Path::to_path_buf))
    .flatten()
    .filter(|cwd| {
        !worktree_candidates
            .iter()
            .any(|worktree| cwd.starts_with(worktree.read(cx).abs_path()))
    })
    .map(|cwd| {
        let names = potential_paths
            .iter()
            .filter(|path| path.path.is_relative() && looks_like_a_path(&path.path))
            .cloned()
            .collect::<Vec<_>>();
        (cwd, names, project.read(cx).fs().clone())
    });

    let background_path_checks = background_path_checks.unwrap_or_else(|| {
        if project.read(cx).is_local() {
            BackgroundPathChecks::LocalFileSystem
        } else {
            BackgroundPathChecks::ProjectPathResolution
        }
    });

    let background_resolution_task = match background_path_checks {
        BackgroundPathChecks::LocalFileSystem => {
            let mut fs_paths_to_check =
                local_paths_to_check(&potential_paths, matching, cwd, &worktree_candidates, cx);
            let fs = project.read(cx).fs().clone();
            cx.background_spawn(async move {
                // SOLO: a `res://` path is in the Godot project the terminal
                // is in: the nearest folder up with a `project.godot`.
                for (project_file, path) in special_paths {
                    let applies = match project_file {
                        None => true,
                        Some(project_file) => fs
                            .metadata(&project_file)
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|metadata| !metadata.is_dir),
                    };
                    if applies {
                        fs_paths_to_check.push(path);
                    }
                }
                for mut path_to_check in fs_paths_to_check {
                    if let Some(fs_path_to_check) = fs.canonicalize(&path_to_check.path).await.ok()
                        && let Some(metadata) = fs.metadata(&fs_path_to_check).await.ok().flatten()
                    {
                        if open_target
                            .as_ref()
                            .map(|open_target| open_target.path().path != fs_path_to_check)
                            .unwrap_or(true)
                        {
                            path_to_check.path = fs_path_to_check;
                            return Some(OpenTarget::Path(
                                path_to_check,
                                metadata.is_dir,
                                #[cfg(any(test, feature = "test-support"))]
                                OpenTargetFoundBy::BackgroundPathResolution,
                            ));
                        }

                        break;
                    }
                }

                open_target
            })
        }
        BackgroundPathChecks::ProjectPathResolution => {
            let paths_to_check = project_paths_to_check(&potential_paths, cwd);
            cx.spawn(async move |cx| {
                for mut path_to_check in paths_to_check {
                    let path = path_to_check.path.to_string_lossy();
                    let resolve_task = project.update(cx, |project, cx| {
                        project.resolve_abs_path(path.as_ref(), cx)
                    });

                    if let Some(resolved_path) = resolve_task.await
                        && let Some(resolved_abs_path) = {
                            let is_dir = resolved_path.is_dir();
                            resolved_path
                                .into_abs_path()
                                .map(|resolved_abs_path| (resolved_abs_path, is_dir))
                        }
                    {
                        let (resolved_abs_path, is_dir) = resolved_abs_path;
                        let resolved_abs_path = PathBuf::from(resolved_abs_path);
                        if open_target
                            .as_ref()
                            .map(|open_target| open_target.path().path != resolved_abs_path)
                            .unwrap_or(true)
                        {
                            path_to_check.path = resolved_abs_path;
                            return Some(OpenTarget::Path(
                                path_to_check,
                                is_dir,
                                #[cfg(any(test, feature = "test-support"))]
                                OpenTargetFoundBy::BackgroundPathResolution,
                            ));
                        }

                        break;
                    }
                }

                open_target
            })
        }
    };

    let working_directory = cwd.map(Path::to_path_buf);
    cx.spawn(async move |cx| {
        if let Some(open_target) = background_resolution_task.await {
            return Some(ResolvedLink::single(open_target));
        }

        // An inexact path must not resolve to an unrelated file sharing its trailing components.
        if matching == PathMatching::Exact {
            return None;
        }

        let in_worktrees = scan_worktrees(
            cx,
            worktree_paths_to_check,
            context_paths,
            working_directory,
        )
        .await;
        if in_worktrees.is_some() {
            return in_worktrees;
        }
        let (cwd, names, fs) = outside_folder_search?;
        cx.background_spawn(async move { find_under(fs.as_ref(), &cwd, &names).await })
            .await
            .map(ResolvedLink::single)
    })
}

/// SOLO: paths written relative to something other than a folder Solo
/// knows: Godot's `res://` (the Godot project the terminal is in) and
/// `%APPDATA%`-style variables. Each comes with the file that must exist for
/// it to apply (`project.godot`), if any. Only checked on disk, never
/// searched for.
fn special_paths(
    path: &PathWithPosition,
    cwd: Option<&Path>,
) -> Vec<(Option<PathBuf>, PathWithPosition)> {
    let text = path.path.to_string_lossy();
    let mut paths = Vec::new();
    if let Some(rest) = text.strip_prefix("res://") {
        let rest = rest.trim_start_matches(['/', '\\']);
        for folder in cwd.into_iter().flat_map(Path::ancestors) {
            paths.push((
                Some(folder.join("project.godot")),
                PathWithPosition {
                    path: folder.join(rest),
                    row: path.row,
                    column: path.column,
                },
            ));
        }
    } else if let Some(expanded) = expand_environment_variable(&text) {
        paths.push((
            None,
            PathWithPosition {
                path: expanded,
                row: path.row,
                column: path.column,
            },
        ));
    }
    paths
}

/// SOLO: the variables a path written by an agent may start with.
const PATH_VARIABLES: &[&str] = &["APPDATA", "LOCALAPPDATA", "USERPROFILE"];

/// SOLO: `text` with a leading `%APPDATA%` (or another of
/// [`PATH_VARIABLES`]) replaced by its value.
fn expand_environment_variable(text: &str) -> Option<PathBuf> {
    let rest = text.strip_prefix('%')?;
    let (name, rest) = rest.split_once('%')?;
    let name = PATH_VARIABLES
        .iter()
        .find(|variable| variable.eq_ignore_ascii_case(name))?;
    let value = std::env::var_os(name)?;
    let rest = rest.trim_start_matches(['/', '\\']);
    Some(PathBuf::from(value).join(rest))
}

/// SOLO: how many entries, and for how long, a terminal's own folder outside
/// the worktrees is searched for a file name.
const MAX_OUTSIDE_ENTRIES: usize = 50_000;
const MAX_OUTSIDE_TIME: std::time::Duration = std::time::Duration::from_secs(2);

/// SOLO: the first file under `root` (breadth first, skipping build and
/// dependency folders) whose path ends with one of `names`.
async fn find_under(
    fs: &dyn fs::Fs,
    root: &Path,
    names: &[PathWithPosition],
) -> Option<OpenTarget> {
    use futures::StreamExt as _;
    if names.is_empty() {
        return None;
    }
    let started = std::time::Instant::now();
    let mut seen = 0;
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(mut entries) = fs.read_dir(&dir).await else {
            continue;
        };
        while let Some(entry) = entries.next().await {
            seen += 1;
            if seen > MAX_OUTSIDE_ENTRIES || started.elapsed() > MAX_OUTSIDE_TIME {
                return None;
            }
            let Ok(path) = entry else { continue };
            let Some(metadata) = fs.metadata(&path).await.ok().flatten() else {
                continue;
            };
            if metadata.is_dir {
                let skip = path.file_name().is_some_and(|name| {
                    matches!(
                        name.to_str(),
                        Some(".git" | "node_modules" | "target" | ".cache" | ".venv")
                    )
                });
                if !skip && !metadata.is_symlink {
                    queue.push_back(path);
                }
                continue;
            }
            if let Some(name) = names.iter().find(|name| path.ends_with(&name.path)) {
                return Some(OpenTarget::Path(
                    PathWithPosition {
                        path,
                        row: name.row,
                        column: name.column,
                    },
                    false,
                    #[cfg(any(test, feature = "test-support"))]
                    OpenTargetFoundBy::BackgroundPathResolution,
                ));
            }
        }
    }
    None
}

/// The worktree scan for a path's trailing components. SOLO: every file it
/// may mean is ranked (see [`CandidateRank`]).
async fn scan_worktrees(
    cx: &mut gpui::AsyncApp,
    worktree_paths_to_check: Vec<(Entity<Worktree>, Vec<PathWithPosition>)>,
    context_paths: Vec<String>,
    working_directory: Option<PathBuf>,
) -> Option<ResolvedLink> {
    {

        let worktree_paths_to_check = worktree_paths_to_check
            .into_iter()
            .map(|(worktree, paths_to_check)| {
                let paths_to_check = paths_to_check
                    .into_iter()
                    .filter_map(|path_with_position| {
                        // SOLO: a bare word ("err") matches some file or
                        // directory of that name deep in a big project
                        // tree; only search for what looks like a path.
                        if !looks_like_a_path(&path_with_position.path) {
                            return None;
                        }
                        let path = RelPath::new(&path_with_position.path, PathStyle::local())
                            .ok()?
                            .into_owned();
                        Some((path_with_position, path))
                    })
                    .collect::<Vec<_>>();
                let snapshot = worktree.read_with(cx, |worktree, _| worktree.snapshot());
                let context_paths = context_paths
                    .iter()
                    .filter_map(|path| {
                        let path = Path::new(path);
                        let path = path.strip_prefix(snapshot.abs_path()).unwrap_or(path);
                        let path = path.strip_prefix(".").unwrap_or(path);
                        Some(RelPath::new(path, PathStyle::local()).ok()?.into_owned())
                    })
                    .collect::<Vec<_>>();
                (snapshot, paths_to_check, context_paths)
            })
            .collect::<Vec<_>>();
        cx.background_spawn(async move {
            let mut found = Vec::new();
            for (snapshot, paths_to_check, context_paths) in worktree_paths_to_check {
                let traversal = snapshot.traverse_from_path(true, true, false, RelPath::empty());
                let mut candidates = Vec::new();
                let mut context_entries = vec![Vec::new(); context_paths.len()];
                for entry in traversal {
                    if candidates.len() < MAX_SCAN_CANDIDATES
                        && let Some(path_with_position) =
                            paths_to_check
                                .iter()
                                .find_map(|(path_with_position, path)| {
                                    entry.path.ends_with(path).then_some(path_with_position)
                                })
                    {
                        candidates.push((entry.clone(), path_with_position));
                    }
                    for (context_path, entries) in context_paths.iter().zip(&mut context_entries) {
                        if entries.len() < MAX_SCAN_CANDIDATES && entry.path.ends_with(context_path)
                        {
                            entries.push(entry.path.clone());
                        }
                    }
                }
                let working_directory = working_directory
                    .as_deref()
                    .and_then(|cwd| cwd.strip_prefix(snapshot.abs_path()).ok())
                    .and_then(|cwd| RelPath::new(cwd, PathStyle::local()).ok())
                    .map(std::borrow::Cow::into_owned);
                for (entry, path_with_position) in candidates {
                    let context_score: usize = context_entries
                        .iter()
                        .map(|entries| {
                            entries
                                .iter()
                                .map(|path| shared_directories(&entry.path, path))
                                .max()
                                .unwrap_or(0)
                        })
                        .sum();
                    let rank = CandidateRank {
                        under_working_directory: working_directory
                            .as_ref()
                            .is_some_and(|cwd| entry.path.starts_with(cwd)),
                        context_score,
                        shared_with_working_directory: working_directory
                            .as_ref()
                            .map_or(0, |cwd| shared_folders(&entry.path, cwd)),
                        third_party: is_third_party(&entry.path),
                        has_row: None,
                        components: entry.path.components().count(),
                    };
                    found.push(LinkCandidate {
                        target: OpenTarget::Worktree(
                            PathWithPosition {
                                path: snapshot.absolutize(&entry.path),
                                row: path_with_position.row,
                                column: path_with_position.column,
                            },
                            entry,
                            #[cfg(any(test, feature = "test-support"))]
                            OpenTargetFoundBy::WorktreeScan,
                        ),
                        rank,
                    });
                }
            }
            ResolvedLink::from_candidates(found)
        })
        .await
    }
}

/// SOLO: how many files of the name a scanned path ends with are compared
/// with the paths around it.
const MAX_SCAN_CANDIDATES: usize = 100;

/// SOLO: how many leading directories two worktree paths have in common.
fn shared_directories(a: &RelPath, b: &RelPath) -> usize {
    let (Some(a), Some(b)) = (a.parent(), b.parent()) else {
        return 0;
    };
    a.components()
        .zip(b.components())
        .take_while(|(a, b)| a == b)
        .count()
}

/// SOLO: how many leading folders a worktree file shares with the folder
/// `directory`.
fn shared_folders(file: &RelPath, directory: &RelPath) -> usize {
    let Some(parent) = file.parent() else {
        return 0;
    };
    parent
        .components()
        .zip(directory.components())
        .take_while(|(a, b)| a == b)
        .count()
}

/// Whether `path` has a directory or an extension, i.e. isn't a bare word.
fn looks_like_a_path(path: &Path) -> bool {
    path.components().count() > 1 || path.extension().is_some()
}

/// SOLO: how many levels up from the terminal's folder paths are tried
/// relative to the folders next to it.
const SIBLING_FOLDER_LEVELS: usize = 3;

fn local_paths_to_check(
    potential_paths: &[PathWithPosition],
    matching: PathMatching,
    cwd: Option<&Path>,
    worktree_candidates: &[Entity<Worktree>],
    cx: &App,
) -> Vec<PathWithPosition> {
    cwd.iter()
        .flat_map(|cwd| {
            potential_paths.iter().filter_map(|path_to_check| {
                path_to_check.path.is_relative().then(|| PathWithPosition {
                    path: cwd.join(&path_to_check.path),
                    row: path_to_check.row,
                    column: path_to_check.column,
                })
            })
        })
        // SOLO: a path written relative to a folder next to the terminal's,
        // or next to one a few levels up (`DBBench_GodotTestbed/main.gd`
        // in a terminal in `godot_engine`). Only paths with a folder: a bare
        // file name there would be a guess.
        .chain(
            (matching == PathMatching::Heuristic)
                .then_some(cwd)
                .flatten()
                .into_iter()
                .flat_map(|cwd| cwd.ancestors().skip(1).take(SIBLING_FOLDER_LEVELS))
                .flat_map(|folder| {
                    potential_paths.iter().filter_map(move |path_to_check| {
                        (path_to_check.path.is_relative()
                            && path_to_check.path.components().count() > 1)
                            .then(|| PathWithPosition {
                                path: folder.join(&path_to_check.path),
                                row: path_to_check.row,
                                column: path_to_check.column,
                            })
                    })
                }),
        )
        .chain(potential_paths.iter().flat_map(|path_to_check| {
            let mut paths_to_check = Vec::new();
            let maybe_path = &path_to_check.path;
            if maybe_path.starts_with("~") {
                if let Some(home_path) =
                    maybe_path
                        .strip_prefix("~")
                        .ok()
                        .and_then(|stripped_maybe_path| {
                            Some(dirs::home_dir()?.join(stripped_maybe_path))
                        })
                {
                    paths_to_check.push(PathWithPosition {
                        path: home_path,
                        row: path_to_check.row,
                        column: path_to_check.column,
                    });
                }
            } else {
                if maybe_path.is_absolute() || matching == PathMatching::Heuristic {
                    paths_to_check.push(PathWithPosition {
                        path: maybe_path.clone(),
                        row: path_to_check.row,
                        column: path_to_check.column,
                    });
                }
                if maybe_path.is_relative() {
                    for worktree in worktree_candidates {
                        if !worktree.read(cx).is_single_file() {
                            paths_to_check.push(PathWithPosition {
                                path: worktree.read(cx).abs_path().join(maybe_path),
                                row: path_to_check.row,
                                column: path_to_check.column,
                            });
                        }
                    }
                }
            }
            paths_to_check
        }))
        .collect()
}

fn project_paths_to_check(
    potential_paths: &[PathWithPosition],
    cwd: Option<&Path>,
) -> Vec<PathWithPosition> {
    cwd.iter()
        .flat_map(|cwd| {
            potential_paths
                .iter()
                .filter_map(|path_to_check| normalize_absolute_candidate(cwd, path_to_check))
        })
        .chain(potential_paths.iter().filter_map(|path_to_check| {
            let maybe_path = &path_to_check.path;
            (maybe_path.starts_with("~") || maybe_path.is_absolute()).then(|| path_to_check.clone())
        }))
        .collect()
}

fn normalize_absolute_candidate(
    cwd: &Path,
    path_to_check: &PathWithPosition,
) -> Option<PathWithPosition> {
    path_to_check.path.is_relative().then(|| {
        normalize_lexically(&cwd.join(&path_to_check.path))
            .ok()
            .map(|path| PathWithPosition {
                path,
                row: path_to_check.row,
                column: path_to_check.column,
            })
    })?
}

/// SOLO: a file a path link means that only another project tab has.
#[derive(Clone, Debug)]
pub struct OtherTabTarget {
    pub workspace: WeakEntity<Workspace>,
    /// The tab's name: its first folder's.
    pub tab_name: String,
    /// The file's folder relative to the tab's folder, for "tab/folder".
    pub relative_folder: String,
    pub target: OpenTarget,
}

/// SOLO: how long other tabs' files found for a path stay remembered.
const OTHER_TABS_CACHE_DURATION: std::time::Duration = std::time::Duration::from_secs(10);
/// SOLO: how many files of a name are kept from all other tabs.
const MAX_OTHER_TAB_TARGETS: usize = 50;

#[derive(Default)]
struct OtherTabsCache(
    std::collections::HashMap<
        (gpui::EntityId, String),
        (std::time::Instant, Vec<OtherTabTarget>),
    >,
);

impl gpui::Global for OtherTabsCache {}

/// SOLO: the files `maybe_path` may mean in the window's other project tabs,
/// for a path the tab `workspace` doesn't have. Only tabs loaded already
/// (opened since Solo started) are searched, in the file lists they hold;
/// nothing is read from disk and no tab is loaded for this. With `budget`
/// (hovering), the search gives up after that long and finds nothing.
/// Results are remembered for a few seconds.
pub fn find_in_other_tabs(
    workspace: &WeakEntity<Workspace>,
    maybe_path: &str,
    budget: Option<std::time::Duration>,
    cx: &mut App,
) -> Task<Vec<OtherTabTarget>> {
    let Some(this) = workspace.upgrade() else {
        return Task::ready(Vec::new());
    };
    let path_with_position = PathWithPosition::parse_str(maybe_path);
    if !path_with_position.path.is_relative()
        || !looks_like_a_path(&path_with_position.path)
        || maybe_path.starts_with(':')
    {
        return Task::ready(Vec::new());
    }
    let Ok(path) = RelPath::new(&path_with_position.path, PathStyle::local()) else {
        return Task::ready(Vec::new());
    };
    let path = path.into_owned();
    let key = (this.entity_id(), maybe_path.to_string());
    let cache = cx.default_global::<OtherTabsCache>();
    cache
        .0
        .retain(|_, (found_at, _)| found_at.elapsed() < OTHER_TABS_CACHE_DURATION);
    if let Some((_, targets)) = cache.0.get(&key) {
        return Task::ready(targets.clone());
    }
    let others = this
        .read(cx)
        .multi_workspace()
        .and_then(|multi_workspace| multi_workspace.upgrade())
        .map(|multi_workspace| {
            multi_workspace
                .read(cx)
                .workspaces()
                .filter(|other| other.entity_id() != this.entity_id())
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let snapshots = others
        .iter()
        .flat_map(|other| {
            let tab_name = other
                .read(cx)
                .worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).root_name_str().to_string())
                .unwrap_or_default();
            let weak = other.downgrade();
            other
                .read(cx)
                .worktrees(cx)
                .filter(|worktree| !worktree.read(cx).is_single_file())
                .map(|worktree| (weak.clone(), tab_name.clone(), worktree.read(cx).snapshot()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if snapshots.is_empty() {
        return Task::ready(Vec::new());
    }
    let search = cx.background_spawn(async move {
        let started = std::time::Instant::now();
        let mut targets = Vec::new();
        for (workspace, tab_name, snapshot) in snapshots {
            for entry in snapshot.files(false, 0) {
                if budget.is_some_and(|budget| started.elapsed() > budget) {
                    return None;
                }
                if !entry.path.ends_with(&path) {
                    continue;
                }
                let relative_folder = entry
                    .path
                    .parent()
                    .map(|folder| folder.as_unix_str().to_string())
                    .unwrap_or_default();
                targets.push(OtherTabTarget {
                    workspace: workspace.clone(),
                    tab_name: tab_name.clone(),
                    relative_folder,
                    target: OpenTarget::Worktree(
                        PathWithPosition {
                            path: snapshot.absolutize(&entry.path),
                            row: path_with_position.row,
                            column: path_with_position.column,
                        },
                        entry.clone(),
                        #[cfg(any(test, feature = "test-support"))]
                        OpenTargetFoundBy::WorktreeScan,
                    ),
                });
                if targets.len() == MAX_OTHER_TAB_TARGETS {
                    return Some(targets);
                }
            }
        }
        Some(targets)
    });
    cx.spawn(async move |cx| {
        // Over the budget: nothing, and nothing remembered.
        let Some(targets) = search.await else {
            return Vec::new();
        };
        cx.update(|cx| {
            cx.default_global::<OtherTabsCache>()
                .0
                .insert(key, (std::time::Instant::now(), targets.clone()));
        });
        targets
    })
}
