//! SOLO: the git side of agent tasks. Everything here runs `git` and blocks,
//! so call it from a background task.
//!
//! A task is a branch with its own linked worktree. The branch's config
//! remembers the task's name and the branch it started from
//! (`branch.<branch>.solotask` and `branch.<branch>.solobase`), so tasks are
//! found again from the repository alone, e.g. after a restart, and
//! `git branch -D` forgets them along with the branch.
#![allow(
    clippy::disallowed_methods,
    reason = "runs git synchronously, only from background tasks"
)]

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use project::git_store::worktrees_directory_for_repo;
use util::{
    i18n::{is_chinese, tr},
    paths::PathStyle,
};

const BRANCH_PREFIX: &str = "agent/";
const MAX_SLUG_CHARS: usize = 40;
const MAX_UNTRACKED_FILES_IN_DIFF: usize = 200;
const BASE_KEY: &str = "solobase";
const NAME_KEY: &str = "solotask";

/// An agent task: a branch checked out in its own worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTask {
    /// What the user called the task.
    pub name: String,
    pub branch: String,
    /// The branch the task started from, and merges back into.
    pub base_branch: String,
    /// The task's worktree, `None` if it's gone (removed outside Solo).
    pub worktree_path: Option<PathBuf>,
    /// The repository's main worktree.
    pub repository_root: PathBuf,
}

/// The repository around a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryInfo {
    /// The main worktree, where new worktrees are placed next to.
    pub main_root: PathBuf,
    /// The worktree the directory is in, which may be a linked one.
    pub current_root: PathBuf,
    /// The branch checked out there, `None` with a detached HEAD.
    pub current_branch: Option<String>,
}

/// The paths and names a new task gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskPlan {
    pub name: String,
    pub branch: String,
    pub base_branch: String,
    pub worktree_path: PathBuf,
}

/// Files with uncommitted changes, as `git status --short` lists them.
pub type UncommittedChanges = Vec<String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    /// The task has no commits the base branch doesn't have.
    NothingToMerge,
    /// The merge stopped on conflicts in these files and was aborted.
    Conflicts(Vec<String>),
}

/// Turns a task name into something fit for a branch and directory name:
/// letters and digits (lowercased, any script) separated by single dashes.
pub fn task_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in name.chars() {
        if character.is_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.extend(character.to_lowercase());
        } else {
            pending_dash = true;
        }
        if slug.chars().count() >= MAX_SLUG_CHARS {
            break;
        }
    }
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

pub fn branch_for_slug(slug: &str) -> String {
    format!("{BRANCH_PREFIX}{slug}")
}

/// `slug`, or `slug-2`, `slug-3`, ... whichever `is_taken` says is free.
pub fn unique_slug(slug: &str, is_taken: impl Fn(&str) -> bool) -> String {
    if !is_taken(slug) {
        return slug.to_string();
    }
    (2..)
        .map(|suffix| format!("{slug}-{suffix}"))
        .find(|candidate| !is_taken(candidate))
        .unwrap_or_else(|| slug.to_string())
}

/// Where task worktrees go: `git.worktree_directory`, resolved against the
/// main worktree like Zed's own "Create Worktree" does (by default
/// `../worktrees/<repository>`).
pub fn worktrees_directory(main_root: &Path, worktree_directory_setting: &str) -> Result<PathBuf> {
    worktrees_directory_for_repo(main_root, worktree_directory_setting, PathStyle::local())
}

fn git<I, S>(directory: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    util::command::new_std_command("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("failed to run git")
}

/// Runs git and returns its trimmed output, failing with its error output.
fn git_ok<I, S>(directory: &Path, arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(directory, arguments)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Git prints paths with forward slashes, also on Windows.
fn native_path(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.replace('/', "\\"))
    } else {
        PathBuf::from(path)
    }
}

pub fn repository_info(directory: &Path) -> Result<RepositoryInfo> {
    let current_root = native_path(&git_ok(directory, ["rev-parse", "--show-toplevel"])?);
    let common_dir = native_path(&git_ok(
        directory,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let main_root = common_dir
        .parent()
        .filter(|_| common_dir.file_name() == Some(OsStr::new(".git")))
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("bare repositories aren't supported"))?;
    let current_branch = git_ok(directory, ["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|branch| !branch.is_empty());
    Ok(RepositoryInfo {
        main_root,
        current_root,
        current_branch,
    })
}

fn branch_exists(repository: &Path, branch: &str) -> bool {
    git(
        repository,
        [
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok_and(|output| output.status.success())
}

/// Picks the branch and worktree path for a new task named `name`, avoiding
/// existing branches and directories.
pub fn plan_task(
    repository: &RepositoryInfo,
    name: &str,
    worktree_directory_setting: &str,
) -> Result<TaskPlan> {
    let name = name.trim();
    if name.is_empty() {
        bail!(tr("Give the task a name", "请输入任务名"));
    }
    let Some(base_branch) = repository.current_branch.clone() else {
        bail!(tr(
            "HEAD is detached; check out the branch the task should start from",
            "HEAD 处于分离状态，请先切换到任务要基于的分支",
        ));
    };
    let directory = worktrees_directory(&repository.main_root, worktree_directory_setting)?;
    let slug = unique_slug(&task_slug(name), |candidate| {
        branch_exists(&repository.main_root, &branch_for_slug(candidate))
            || directory.join(candidate).exists()
    });
    Ok(TaskPlan {
        name: name.to_string(),
        branch: branch_for_slug(&slug),
        base_branch,
        worktree_path: directory.join(slug),
    })
}

/// Creates the task's branch from its base branch, checked out in a new
/// worktree.
pub fn create_task(repository: &RepositoryInfo, plan: &TaskPlan) -> Result<AgentTask> {
    if let Some(parent) = plan.worktree_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    git_ok(
        &repository.main_root,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(&plan.branch),
            plan.worktree_path.as_os_str(),
            OsStr::new(&plan.base_branch),
        ],
    )
    .context("git worktree add failed")?;

    let remember = |key: &str, value: &str| {
        git_ok(
            &repository.main_root,
            ["config", &format!("branch.{}.{key}", plan.branch), value],
        )
    };
    if let Err(error) =
        remember(BASE_KEY, &plan.base_branch).and_then(|_| remember(NAME_KEY, &plan.name))
    {
        let task = AgentTask {
            name: plan.name.clone(),
            branch: plan.branch.clone(),
            base_branch: plan.base_branch.clone(),
            worktree_path: Some(plan.worktree_path.clone()),
            repository_root: repository.main_root.clone(),
        };
        if let Err(rollback_error) = delete_task(&task) {
            log::error!("failed to roll back the agent task worktree: {rollback_error:#}");
        }
        return Err(error.context("failed to save the task in the git config"));
    }

    Ok(AgentTask {
        name: plan.name.clone(),
        branch: plan.branch.clone(),
        base_branch: plan.base_branch.clone(),
        worktree_path: Some(plan.worktree_path.clone()),
        repository_root: repository.main_root.clone(),
    })
}

/// The worktrees of the repository with the branch checked out in each.
fn worktrees(repository: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let output = git_ok(repository, ["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&output))
}

fn parse_worktree_list(output: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut worktrees = Vec::new();
    let mut current: Option<(PathBuf, Option<String>)> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.extend(current.take());
            current = Some((native_path(path), None));
        } else if let Some(reference) = line.strip_prefix("branch ")
            && let Some((_, branch)) = current.as_mut()
        {
            *branch = Some(
                reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .to_string(),
            );
        }
    }
    worktrees.extend(current);
    worktrees
}

/// Parses `git config --get-regexp` output for the task keys into
/// `(branch, key, value)`.
fn parse_task_config(output: &str) -> Vec<(String, String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            let rest = key.strip_prefix("branch.")?;
            let (branch, variable) = rest.rsplit_once('.')?;
            Some((branch.to_string(), variable.to_string(), value.to_string()))
        })
        .collect()
}

/// The agent tasks of the repository whose main worktree is
/// `repository_root`.
pub fn list_tasks(repository_root: &Path) -> Result<Vec<AgentTask>> {
    let config = git(
        repository_root,
        [
            "config",
            "--get-regexp",
            &format!(r"^branch\..*\.({BASE_KEY}|{NAME_KEY})$"),
        ],
    )?;
    // Exit code 1: no such keys.
    if !config.status.success() && config.status.code() != Some(1) {
        bail!("{}", String::from_utf8_lossy(&config.stderr).trim());
    }
    let config = String::from_utf8_lossy(&config.stdout);
    let entries = parse_task_config(&config);
    let worktrees = worktrees(repository_root)?;

    let mut tasks: Vec<AgentTask> = Vec::new();
    for (branch, variable, _) in &entries {
        if variable != BASE_KEY || tasks.iter().any(|task| &task.branch == branch) {
            continue;
        }
        if !branch_exists(repository_root, branch) {
            continue;
        }
        let value_of = |key: &str| {
            entries
                .iter()
                .find(|(entry_branch, variable, _)| entry_branch == branch && variable == key)
                .map(|(_, _, value)| value.clone())
        };
        let Some(base_branch) = value_of(BASE_KEY) else {
            continue;
        };
        tasks.push(AgentTask {
            name: value_of(NAME_KEY).unwrap_or_else(|| branch.clone()),
            branch: branch.clone(),
            base_branch,
            worktree_path: worktrees
                .iter()
                .find(|(_, worktree_branch)| worktree_branch.as_deref() == Some(branch))
                .map(|(path, _)| path.clone()),
            repository_root: repository_root.to_path_buf(),
        });
    }
    Ok(tasks)
}

pub fn uncommitted_changes(worktree: &Path) -> Result<UncommittedChanges> {
    let output = git_ok(worktree, ["status", "--short", "--untracked-files=all"])?;
    Ok(output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// One line per commit the task has that its base branch doesn't.
pub fn unmerged_commits(task: &AgentTask) -> Result<Vec<String>> {
    let output = git_ok(
        &task.repository_root,
        [
            "log",
            "--oneline",
            "--no-decorate",
            &format!("{}..{}", task.base_branch, task.branch),
        ],
    )?;
    Ok(output.lines().map(str::to_string).collect())
}

/// Where the task's base branch is checked out, which is where merging
/// happens.
pub fn base_worktree(task: &AgentTask) -> Result<Option<PathBuf>> {
    Ok(worktrees(&task.repository_root)?
        .into_iter()
        .find(|(_, branch)| branch.as_deref() == Some(task.base_branch.as_str()))
        .map(|(path, _)| path))
}

/// Everything the task changed since it left its base branch, uncommitted
/// changes and new files included, as a unified diff.
pub fn diff_against_base(task: &AgentTask) -> Result<String> {
    let worktree = task
        .worktree_path
        .as_deref()
        .ok_or_else(|| anyhow!(tr("The task's worktree is gone", "任务的工作区已不存在")))?;
    let merge_base = git_ok(worktree, ["merge-base", &task.base_branch, "HEAD"])?;
    let mut diff = git_ok(
        worktree,
        ["diff", "--no-color", "--no-ext-diff", &merge_base],
    )?;
    let untracked = git_ok(worktree, ["ls-files", "--others", "--exclude-standard"])?;
    for file in untracked.lines().take(MAX_UNTRACKED_FILES_IN_DIFF) {
        // `--no-index` exits with 1 when the files differ, which they do.
        let output = git(
            worktree,
            ["diff", "--no-color", "--no-index", "--", "/dev/null", file],
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        if !text.is_empty() {
            if !diff.is_empty() && !diff.ends_with('\n') {
                diff.push('\n');
            }
            diff.push_str(&text);
        }
    }
    Ok(diff)
}

/// Merges the task's branch into its base branch, in the worktree where the
/// base branch is checked out. Conflicts abort the merge rather than leave it
/// half done there.
pub fn merge_task(task: &AgentTask) -> Result<MergeOutcome> {
    if unmerged_commits(task)?.is_empty() {
        return Ok(MergeOutcome::NothingToMerge);
    }
    let Some(target) = base_worktree(task)? else {
        if is_chinese() {
            bail!(
                "{} 没有在任何工作区检出，请先检出它再合并",
                task.base_branch
            );
        }
        bail!(
            "{} isn't checked out in any worktree; check it out to merge into it",
            task.base_branch
        );
    };
    let message = format!("Merge agent task \"{}\" ({})", task.name, task.branch);
    let output = git(
        &target,
        [
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &message,
            &task.branch,
        ],
    )?;
    if output.status.success() {
        return Ok(MergeOutcome::Merged);
    }
    let conflicts = git_ok(&target, ["diff", "--name-only", "--diff-filter=U"])?;
    let conflicts: Vec<String> = conflicts.lines().map(str::to_string).collect();
    if conflicts.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!("git merge failed: {}", format!("{stderr}\n{stdout}").trim());
    }
    git_ok(&target, ["merge", "--abort"]).context("failed to abort the conflicting merge")?;
    Ok(MergeOutcome::Conflicts(conflicts))
}

/// Removes the task's worktree, uncommitted changes and all, and its branch.
pub fn delete_task(task: &AgentTask) -> Result<()> {
    let repository = &task.repository_root;
    if let Some(worktree) = &task.worktree_path
        && worktree.exists()
    {
        let removed = git_ok(
            repository,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                worktree.as_os_str(),
            ],
        );
        if let Err(error) = removed {
            // Programs that just ended in it may still hold files for a moment
            // on Windows.
            log::warn!("git worktree remove failed, deleting the directory: {error:#}");
            remove_dir_with_retries(worktree)?;
        }
    }
    git_ok(repository, ["worktree", "prune"])?;
    git_ok(repository, ["branch", "-D", &task.branch])
        .with_context(|| format!("failed to delete the branch {}", task.branch))?;
    Ok(())
}

fn remove_dir_with_retries(directory: &Path) -> Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::remove_dir_all(directory) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt >= 5 => {
                return Err(anyhow!(error))
                    .with_context(|| format!("failed to delete {}", directory.display()));
            }
            Err(_) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200 * attempt));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn slugs_keep_letters_and_digits() {
        assert_eq!(task_slug("Fix the login bug"), "fix-the-login-bug");
        assert_eq!(
            task_slug("  --Add OAuth2!!  support "),
            "add-oauth2-support"
        );
        assert_eq!(task_slug("修复 登录/页面"), "修复-登录-页面");
        assert_eq!(task_slug("../../etc"), "etc");
        assert_eq!(task_slug("!!!"), "task");
        assert_eq!(task_slug(""), "task");
        assert_eq!(task_slug(&"a".repeat(100)).chars().count(), MAX_SLUG_CHARS);
    }

    #[test]
    fn branches_are_prefixed() {
        assert_eq!(branch_for_slug("fix-login"), "agent/fix-login");
    }

    #[test]
    fn unique_slugs_count_up() {
        assert_eq!(unique_slug("fix", |_| false), "fix");
        assert_eq!(unique_slug("fix", |slug| slug == "fix"), "fix-2");
        assert_eq!(
            unique_slug("fix", |slug| ["fix", "fix-2", "fix-3"].contains(&slug)),
            "fix-4"
        );
    }

    #[test]
    fn worktrees_go_next_to_the_repository_by_default() {
        let root = Path::new(if cfg!(windows) {
            "C:\\code\\solo"
        } else {
            "/code/solo"
        });
        assert_eq!(
            worktrees_directory(root, "../worktrees").unwrap(),
            root.parent().unwrap().join("worktrees").join("solo")
        );
        assert_eq!(
            worktrees_directory(root, ".solo-worktrees").unwrap(),
            root.join(".solo-worktrees")
        );
        assert!(worktrees_directory(root, "../../elsewhere").is_err());
    }

    #[test]
    fn parses_worktree_lists() {
        let output = "worktree /code/solo\nHEAD 1234\nbranch refs/heads/main\n\n\
                      worktree /code/worktrees/solo/fix\nHEAD 5678\nbranch refs/heads/agent/fix\n\n\
                      worktree /code/detached\nHEAD 9abc\ndetached\n";
        assert_eq!(
            parse_worktree_list(output),
            vec![
                (native_path("/code/solo"), Some("main".to_string())),
                (
                    native_path("/code/worktrees/solo/fix"),
                    Some("agent/fix".to_string())
                ),
                (native_path("/code/detached"), None),
            ]
        );
    }

    #[test]
    fn parses_task_config() {
        let output = "branch.agent/fix.solobase main\nbranch.agent/fix.solotask Fix the bug";
        assert_eq!(
            parse_task_config(output),
            vec![
                ("agent/fix".into(), "solobase".into(), "main".into()),
                ("agent/fix".into(), "solotask".into(), "Fix the bug".into()),
            ]
        );
    }

    fn run_git(directory: &Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit_all(directory: &Path, message: &str) {
        run_git(directory, &["add", "-A"]);
        run_git(directory, &["commit", "-q", "-m", message]);
    }

    /// A repository with one commit on `main`, in `<temp>/projects/repo`.
    fn new_repository() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects").join("repo");
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]);
        run_git(&root, &["config", "user.name", "test"]);
        run_git(&root, &["config", "user.email", "test@example.com"]);
        run_git(&root, &["config", "core.autocrlf", "false"]);
        fs::write(root.join("shared.txt"), "one\ntwo\nthree\n").unwrap();
        commit_all(&root, "initial");
        (temp, root)
    }

    fn same_path(left: &Path, right: &Path) -> bool {
        fs::canonicalize(left).unwrap() == fs::canonicalize(right).unwrap()
    }

    #[test]
    fn two_tasks_merge_one_discard_the_other() {
        let (_temp, root) = new_repository();
        let info = repository_info(&root).unwrap();
        assert!(same_path(&info.main_root, &root));
        assert_eq!(info.current_branch.as_deref(), Some("main"));

        let first_plan = plan_task(&info, "Add greeting", "../worktrees").unwrap();
        let first = create_task(&info, &first_plan).unwrap();
        let second_plan = plan_task(&info, "Add greeting", "../worktrees").unwrap();
        assert_eq!(first.branch, "agent/add-greeting");
        assert_eq!(second_plan.branch, "agent/add-greeting-2");
        let second = create_task(&info, &second_plan).unwrap();

        let first_dir = first.worktree_path.clone().unwrap();
        let second_dir = second.worktree_path.clone().unwrap();
        assert!(same_path(
            first_dir.parent().unwrap(),
            &root.parent().unwrap().join("worktrees").join("repo")
        ));

        // Each task works in its own directory.
        fs::write(first_dir.join("greeting.txt"), "hello\n").unwrap();
        fs::write(second_dir.join("shared.txt"), "one\nTWO\nthree\n").unwrap();
        assert!(!root.join("greeting.txt").exists());
        assert!(!second_dir.join("greeting.txt").exists());
        assert_eq!(
            fs::read_to_string(first_dir.join("shared.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );

        // A linked worktree resolves to the same repository.
        let from_task = repository_info(&first_dir).unwrap();
        assert!(same_path(&from_task.main_root, &root));
        assert_eq!(
            from_task.current_branch.as_deref(),
            Some("agent/add-greeting")
        );

        let mut tasks = list_tasks(&root).unwrap();
        tasks.sort_by(|left, right| left.branch.cmp(&right.branch));
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].name, "Add greeting");
        assert_eq!(tasks[0].base_branch, "main");
        assert!(same_path(
            tasks[0].worktree_path.as_ref().unwrap(),
            &first_dir
        ));

        // The diff covers new files that aren't committed yet.
        let diff = diff_against_base(&first).unwrap();
        assert!(diff.contains("greeting.txt"), "{diff}");
        assert!(diff.contains("+hello"), "{diff}");
        assert_eq!(uncommitted_changes(&first_dir).unwrap().len(), 1);

        assert_eq!(merge_task(&first).unwrap(), MergeOutcome::NothingToMerge);
        commit_all(&first_dir, "greet");
        assert_eq!(unmerged_commits(&first).unwrap().len(), 1);
        let diff = diff_against_base(&first).unwrap();
        assert!(diff.contains("+hello"), "{diff}");
        assert_eq!(merge_task(&first).unwrap(), MergeOutcome::Merged);
        assert_eq!(
            fs::read_to_string(root.join("greeting.txt")).unwrap(),
            "hello\n"
        );
        assert!(unmerged_commits(&first).unwrap().is_empty());

        assert_eq!(uncommitted_changes(&second_dir).unwrap().len(), 1);
        delete_task(&second).unwrap();
        assert!(!second_dir.exists());
        assert!(!branch_exists(&root, &second.branch));
        assert_eq!(
            fs::read_to_string(root.join("shared.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );

        delete_task(&first).unwrap();
        assert!(list_tasks(&root).unwrap().is_empty());
        assert_eq!(run_git(&root, &["branch", "--show-current"]), "main");
    }

    #[test]
    fn conflicting_merges_are_aborted() {
        let (_temp, root) = new_repository();
        let info = repository_info(&root).unwrap();
        let task = create_task(&info, &plan_task(&info, "Edit", "../worktrees").unwrap()).unwrap();
        let worktree = task.worktree_path.clone().unwrap();

        fs::write(worktree.join("shared.txt"), "one\ntask\nthree\n").unwrap();
        commit_all(&worktree, "task edit");
        fs::write(root.join("shared.txt"), "one\nmain\nthree\n").unwrap();
        commit_all(&root, "main edit");

        assert_eq!(
            merge_task(&task).unwrap(),
            MergeOutcome::Conflicts(vec!["shared.txt".to_string()])
        );
        assert_eq!(
            fs::read_to_string(root.join("shared.txt")).unwrap(),
            "one\nmain\nthree\n"
        );
        assert!(uncommitted_changes(&root).unwrap().is_empty());
        delete_task(&task).unwrap();
    }

    #[test]
    fn tasks_need_a_branch_to_start_from() {
        let (_temp, root) = new_repository();
        run_git(&root, &["checkout", "-q", "--detach"]);
        let info = repository_info(&root).unwrap();
        assert_eq!(info.current_branch, None);
        assert!(plan_task(&info, "Anything", "../worktrees").is_err());
        let info = RepositoryInfo {
            current_branch: Some("main".into()),
            ..info
        };
        assert!(plan_task(&info, "   ", "../worktrees").is_err());
    }
}
