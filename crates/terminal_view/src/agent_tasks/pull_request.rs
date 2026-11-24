//! SOLO: a task's pull request on GitHub, through the `gh` CLI: opening it
//! from the task's branch, and gathering what its checks and reviewers said,
//! to send to the task's agent. Everything here runs `gh` or `git` and
//! blocks, so call it from a background task.
#![allow(
    clippy::disallowed_methods,
    reason = "runs gh and git synchronously, only from background tasks"
)]

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use util::i18n::{is_chinese, tr};

use super::git::{self, AgentTask};

/// How many `gh` processes run at once, whoever started them.
const MAX_GH_PROCESSES: usize = 2;
/// How long a `gh` run for the periodic check may take before it's killed, so
/// that a hung one doesn't hold one of the two places forever.
const POLL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long any other `gh` run (opening a pull request, looking one up) may
/// take; a hung one would hold one of the two places.
const MANUAL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
}

impl PullRequest {
    pub fn is_open(&self) -> bool {
        self.state == "OPEN"
    }
}

/// A check that failed, with where to read its log.
#[derive(Clone, Debug, PartialEq)]
pub struct FailedCheck {
    pub name: String,
    pub link: Option<String>,
    /// The commit it ran on: the same check failing on a new commit is new.
    pub commit: String,
}

impl FailedCheck {
    /// What tells this failure from others.
    pub fn key(&self) -> String {
        format!("{}@{}", self.name, self.commit)
    }
}

/// A comment on the pull request or on a line of its diff.
#[derive(Clone, Debug, PartialEq)]
pub struct PrComment {
    /// What tells new comments from those already sent.
    pub id: String,
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrFeedback {
    pub failed_checks: Vec<FailedCheck>,
    pub comments: Vec<PrComment>,
}

impl PrFeedback {
    pub fn is_empty(&self) -> bool {
        self.failed_checks.is_empty() && self.comments.is_empty()
    }
}

/// `gh` isn't installed, or isn't logged in.
#[derive(Debug)]
pub struct GhUnavailable(pub String);

impl std::fmt::Display for GhUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GhUnavailable {}

/// The repository has no remote on a GitHub host, so there's nothing to
/// ask.
#[derive(Debug)]
pub struct NoGithubRemote(pub String);

impl std::fmt::Display for NoGithubRemote {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NoGithubRemote {}

fn is_no_github_remote(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("no git remotes found")
        || error.contains("known github host")
        || error.contains("not a github host")
}

fn gh_missing() -> GhUnavailable {
    GhUnavailable(
        tr(
            "failed to run gh, GitHub's CLI; install it and run `gh auth login`",
            "无法运行 GitHub 的命令行工具 gh；请安装它并运行 `gh auth login`",
        )
        .to_string(),
    )
}

struct ProcessGate {
    running: Mutex<usize>,
    released: Condvar,
}

static PROCESS_GATE: ProcessGate = ProcessGate {
    running: Mutex::new(0),
    released: Condvar::new(),
};

struct ProcessPermit;

impl ProcessGate {
    fn acquire(&'static self) -> ProcessPermit {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        while *running >= MAX_GH_PROCESSES {
            running = self
                .released
                .wait(running)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *running += 1;
        ProcessPermit
    }
}

impl Drop for ProcessPermit {
    fn drop(&mut self) {
        let mut running = PROCESS_GATE
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *running = running.saturating_sub(1);
        PROCESS_GATE.released.notify_one();
    }
}

fn read_pipe(
    pipe: Option<impl std::io::Read + Send + 'static>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe
            && let Err(error) = pipe.read_to_end(&mut bytes)
        {
            log::warn!("failed to read gh's output: {error}");
        }
        bytes
    })
}

/// Runs `gh` in `directory`, once fewer than [`MAX_GH_PROCESSES`] run.
fn run_gh<I, S>(directory: &Path, arguments: I, timeout: Option<Duration>) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments: Vec<OsString> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    let _permit = PROCESS_GATE.acquire();
    let mut child = util::command::new_std_command("gh")
        .current_dir(directory)
        .args(&arguments)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow::Error::new(gh_missing())
            } else {
                anyhow::Error::new(error).context(tr(
                    "failed to run gh, GitHub's CLI; install it and run `gh auth login`",
                    "无法运行 GitHub 的命令行工具 gh；请安装它并运行 `gh auth login`",
                ))
            }
        })?;
    let stdout = read_pipe(child.stdout.take());
    let stderr = read_pipe(child.stderr.take());
    let status = match timeout {
        None => child.wait()?,
        Some(timeout) => {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().context("failed to stop gh")?;
                    child.wait()?;
                    bail!("gh didn't finish in {} seconds", timeout.as_secs());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn gh<I, S>(directory: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_gh(directory, arguments, Some(MANUAL_TIMEOUT))
}

fn gh_polled<I, S>(directory: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_gh(directory, arguments, Some(POLL_TIMEOUT))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// What gh prints when it isn't logged in.
fn is_not_logged_in(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("gh auth login")
        || error.contains("not logged in")
        || error.contains("authentication required")
}

/// A failed `gh` run's error; one for want of a login is [`GhUnavailable`].
fn gh_failure(what: &str, output: &Output) -> anyhow::Error {
    let error = stderr(output);
    if is_not_logged_in(&error) {
        return anyhow::Error::new(GhUnavailable(format!("{what}: {error}")));
    }
    if is_no_github_remote(&error) {
        return anyhow::Error::new(NoGithubRemote(format!("{what}: {error}")));
    }
    anyhow::anyhow!("{what}: {error}")
}

/// Whether the task's branch was pushed: it has an upstream, or a remote
/// branch of its name exists. Otherwise there can't be a pull request.
pub fn branch_is_pushed(worktree: &Path, branch: &str) -> Result<bool> {
    let upstream = util::command::new_std_command("git")
        .arg("-C")
        .arg(worktree)
        .args(["config", "--get", &format!("branch.{branch}.remote")])
        .output()
        .context("failed to run git")?;
    if upstream.status.success() && !stdout(&upstream).trim().is_empty() {
        return Ok(true);
    }
    let remote_branches = util::command::new_std_command("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            &format!("refs/remotes/*/{branch}"),
        ])
        .output()
        .context("failed to run git")?;
    Ok(remote_branches.status.success() && !stdout(&remote_branches).trim().is_empty())
}

/// The pull request of `branch`, if it has one.
pub fn find_pull_request(worktree: &Path, branch: &str) -> Result<Option<PullRequest>> {
    let output = gh(
        worktree,
        ["pr", "view", branch, "--json", "number,url,state"],
    )?;
    if !output.status.success() {
        let error = stderr(&output);
        if error.to_lowercase().contains("no pull requests found") {
            return Ok(None);
        }
        return Err(gh_failure("gh pr view failed", &output));
    }
    Ok(Some(
        serde_json::from_str(&stdout(&output)).context("unexpected output from gh pr view")?,
    ))
}

/// Pushes the task's branch and opens a pull request into the branch it
/// started from, titled and described from its commits.
pub fn open_pull_request(task: &AgentTask) -> Result<PullRequest> {
    let worktree = task
        .worktree_path
        .as_deref()
        .context(tr("the task's worktree is gone", "任务的工作树已不存在"))?;
    if !git::uncommitted_changes(worktree)?.is_empty() {
        if is_chinese() {
            bail!(
                "{} 有未提交的改动；请先提交（或让 agent 提交）",
                task.branch
            );
        }
        bail!(
            "{} has uncommitted changes; commit them (or have the agent do it) first",
            task.branch
        );
    }
    if git::unmerged_commits(task)?.is_empty() {
        if is_chinese() {
            bail!("{} 没有 {} 之外的提交", task.branch, task.base_branch);
        }
        bail!(
            "{} has no commits that {} doesn't have",
            task.branch,
            task.base_branch
        );
    }
    let push = util::command::new_std_command("git")
        .arg("-C")
        .arg(worktree)
        .args(["push", "--set-upstream", "origin", &task.branch])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("failed to run git")?;
    if !push.status.success() {
        bail!(
            "{}: {}",
            tr("git push failed", "git push 失败"),
            stderr(&push)
        );
    }
    let create = gh(
        worktree,
        [
            "pr",
            "create",
            "--base",
            &task.base_branch,
            "--head",
            &task.branch,
            "--fill",
        ],
    )?;
    if !create.status.success() {
        bail!(
            "{}: {}",
            tr("gh pr create failed", "gh pr create 失败"),
            stderr(&create)
        );
    }
    find_pull_request(worktree, &task.branch)?
        .context("the pull request was opened but isn't found")
}

#[derive(Deserialize)]
struct Check {
    name: String,
    /// `pass`, `fail`, `pending`, `skipping` or `cancel`.
    bucket: String,
    #[serde(default)]
    link: Option<String>,
}

/// One of the pull request's checks.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckRun {
    pub name: String,
    pub bucket: String,
    pub link: Option<String>,
}

impl CheckRun {
    pub fn is_pending(&self) -> bool {
        self.bucket == "pending"
    }

    pub fn is_failed(&self) -> bool {
        self.bucket == "fail"
    }
}

pub(crate) fn check_runs(json: &str) -> Result<Vec<CheckRun>> {
    let checks: Vec<Check> =
        serde_json::from_str(json).context("unexpected output from gh pr checks")?;
    Ok(checks
        .into_iter()
        .map(|check| CheckRun {
            name: check.name,
            bucket: check.bucket,
            link: check.link.filter(|link| !link.is_empty()),
        })
        .collect())
}

fn failed_runs(checks: &[CheckRun], commit: &str) -> Vec<FailedCheck> {
    checks
        .iter()
        .filter(|check| check.is_failed())
        .map(|check| FailedCheck {
            name: check.name.clone(),
            link: check.link.clone(),
            commit: commit.to_string(),
        })
        .collect()
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

#[derive(Deserialize)]
struct Comment {
    #[serde(default)]
    id: String,
    /// Null for an account that's been deleted.
    author: Option<Author>,
    body: String,
}

#[derive(Deserialize, Default)]
struct Conversation {
    #[serde(default)]
    comments: Vec<Comment>,
    #[serde(default)]
    reviews: Vec<Comment>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct LineComment {
    #[serde(default)]
    id: u64,
    user: Option<User>,
    body: String,
    path: String,
    line: Option<u64>,
    original_line: Option<u64>,
}

fn comment_from(kind: &str, comment: Comment) -> PrComment {
    let author = comment
        .author
        .map_or_else(|| "ghost".to_string(), |author| author.login);
    // Without an id, what it says still tells it from the others.
    let id = if comment.id.is_empty() {
        format!("{kind}:{author}:{}", comment.body)
    } else {
        comment.id
    };
    PrComment {
        id,
        author,
        body: comment.body,
        path: None,
        line: None,
    }
}

fn comments_of(conversation: Conversation, line_comments: &str) -> Result<Vec<PrComment>> {
    let line_comments: Vec<LineComment> =
        serde_json::from_str(line_comments).context("unexpected output from gh api")?;
    let mut comments: Vec<PrComment> = conversation
        .comments
        .into_iter()
        .map(|comment| comment_from("comment", comment))
        .chain(
            conversation
                .reviews
                .into_iter()
                .map(|review| comment_from("review", review)),
        )
        .collect();
    comments.extend(line_comments.into_iter().map(|comment| {
        PrComment {
            id: format!("line:{}", comment.id),
            author: comment
                .user
                .map_or_else(|| "ghost".to_string(), |user| user.login),
            body: comment.body,
            path: Some(comment.path),
            line: comment.line.or(comment.original_line),
        }
    }));
    comments.retain(|comment| !comment.body.trim().is_empty());
    Ok(comments)
}

/// The comments and review summaries on the pull request (`gh pr view
/// --json comments,reviews`), then those on lines of its diff (`gh api
/// .../pulls/N/comments`), skipping empty ones.
#[cfg(test)]
pub(crate) fn pr_comments(conversation: &str, line_comments: &str) -> Result<Vec<PrComment>> {
    let conversation: Conversation =
        serde_json::from_str(conversation).context("unexpected output from gh pr view")?;
    comments_of(conversation, line_comments)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    number: u64,
    url: String,
    state: String,
    #[serde(default)]
    head_ref_oid: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or empty.
    #[serde(default)]
    review_decision: Option<String>,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    is_draft: bool,
    /// `CLEAN`, `UNSTABLE`, `HAS_HOOKS`, `BLOCKED`, `BEHIND`, `DIRTY`,
    /// `DRAFT` or `UNKNOWN`.
    #[serde(default)]
    merge_state_status: Option<String>,
    #[serde(flatten)]
    conversation: Conversation,
}

/// Everything the periodic check learns of a pull request.
#[derive(Clone, Debug, PartialEq)]
pub struct PrSnapshot {
    pub pull_request: PullRequest,
    pub head_commit: String,
    pub checks: Vec<CheckRun>,
    pub review_decision: Option<String>,
    pub mergeable: Option<String>,
    pub is_draft: bool,
    pub merge_state: Option<String>,
    pub comments: Vec<PrComment>,
}

impl PrSnapshot {
    /// From the output of `gh pr view --json
    /// number,url,state,headRefOid,reviewDecision,mergeable,isDraft,mergeStateStatus,comments,reviews`,
    /// `gh pr checks --json name,bucket,link` and `gh api
    /// .../pulls/N/comments`. The last two are left out for a pull request
    /// that isn't open.
    pub fn from_json(
        view: &str,
        checks: Option<&str>,
        line_comments: Option<&str>,
    ) -> Result<Self> {
        let view: PrView =
            serde_json::from_str(view).context("unexpected output from gh pr view")?;
        Ok(Self {
            pull_request: PullRequest {
                number: view.number,
                url: view.url,
                state: view.state,
            },
            head_commit: view.head_ref_oid,
            checks: checks.map(check_runs).transpose()?.unwrap_or_default(),
            review_decision: view.review_decision.filter(|decision| !decision.is_empty()),
            mergeable: view.mergeable,
            is_draft: view.is_draft,
            merge_state: view
                .merge_state_status
                .filter(|state| !state.is_empty() && state != "UNKNOWN"),
            comments: comments_of(view.conversation, line_comments.unwrap_or("[]"))?,
        })
    }

    pub fn has_pending_checks(&self) -> bool {
        self.checks.iter().any(CheckRun::is_pending)
    }

    pub fn failed_checks(&self) -> Vec<FailedCheck> {
        failed_runs(&self.checks, &self.head_commit)
    }

    /// Everything the checks and reviewers said.
    pub fn feedback(&self) -> PrFeedback {
        PrFeedback {
            failed_checks: self.failed_checks(),
            comments: self.comments.clone(),
        }
    }
}

const VIEW_FIELDS: &str = "number,url,state,headRefOid,reviewDecision,mergeable,isDraft,mergeStateStatus,comments,reviews";

/// Reads a pull request (found by its branch or number) with its checks and
/// comments, with at most one `gh` process at a time. `None` if there's none.
pub fn fetch_snapshot(worktree: &Path, selector: &str) -> Result<Option<PrSnapshot>> {
    let view = gh_polled(worktree, ["pr", "view", selector, "--json", VIEW_FIELDS])?;
    if !view.status.success() {
        if stderr(&view)
            .to_lowercase()
            .contains("no pull requests found")
        {
            return Ok(None);
        }
        return Err(gh_failure("gh pr view failed", &view));
    }
    let view_json = stdout(&view);
    let pull_request: PullRequest =
        serde_json::from_str(&view_json).context("unexpected output from gh pr view")?;
    if !pull_request.is_open() {
        return PrSnapshot::from_json(&view_json, None, None).map(Some);
    }
    let number = pull_request.number.to_string();
    // gh exits with an error when checks fail or are pending; what it printed
    // still says which.
    let checks = gh_polled(
        worktree,
        ["pr", "checks", &number, "--json", "name,bucket,link"],
    )?;
    let checks_json = match stdout(&checks).trim() {
        "" if checks.status.success() => "[]".to_string(),
        "" => {
            if stderr(&checks).to_lowercase().contains("no checks") {
                "[]".to_string()
            } else {
                return Err(gh_failure("gh pr checks failed", &checks));
            }
        }
        json => json.to_string(),
    };
    let line_comments = gh_polled(
        worktree,
        [
            "api",
            &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments?per_page=100"),
        ],
    )?;
    if !line_comments.status.success() {
        return Err(gh_failure("gh api failed", &line_comments));
    }
    PrSnapshot::from_json(
        &view_json,
        Some(&checks_json),
        Some(&stdout(&line_comments)),
    )
    .map(Some)
}

/// The message telling the agent what the pull request's checks and
/// reviewers said.
pub fn feedback_message(pull_request: &PullRequest, feedback: &PrFeedback) -> String {
    let mut message = format!(
        "Feedback on pull request #{} ({}):\n",
        pull_request.number, pull_request.url
    );
    if !feedback.failed_checks.is_empty() {
        message
            .push_str("\nFailed checks (read their logs, e.g. with `gh run view --log-failed`):\n");
        for check in &feedback.failed_checks {
            match &check.link {
                Some(link) => message.push_str(&format!("- {} — {link}\n", check.name)),
                None => message.push_str(&format!("- {}\n", check.name)),
            }
        }
    }
    if !feedback.comments.is_empty() {
        // SOLO: what others wrote goes to the agent as theirs, unverified,
        // not as the user's instructions (R3 L-G11). The user still presses
        // Enter to send it.
        message.push_str(&format!(
            "\n{}\n",
            tr(
                "Comments from others on the PR (unverified):",
                "以下是 PR 中他人的评论（未经核实）:",
            )
        ));
        for comment in &feedback.comments {
            let location = match (&comment.path, comment.line) {
                (Some(path), Some(line)) => format!(" on {path}:{line}"),
                (Some(path), None) => format!(" on {path}"),
                _ => String::new(),
            };
            // Continuation lines are indented to keep each comment one item.
            let body = comment
                .body
                .trim()
                .replace("\r\n", "\n")
                .replace('\n', "\n  ");
            message.push_str(&format!("- {}{location}: {body}\n", comment.author));
        }
    }
    message.push_str("\nPlease address these, then commit and push.\n");
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failed_checks() {
        let json = r#"[
            {"name": "build", "bucket": "pass", "link": "https://x/1"},
            {"name": "test (windows)", "bucket": "fail", "link": "https://x/2"},
            {"name": "lint", "bucket": "fail", "link": ""},
            {"name": "deploy", "bucket": "pending"}
        ]"#;
        assert_eq!(
            failed_runs(&check_runs(json).unwrap(), "abc"),
            vec![
                FailedCheck {
                    name: "test (windows)".into(),
                    link: Some("https://x/2".into()),
                    commit: "abc".into(),
                },
                FailedCheck {
                    name: "lint".into(),
                    link: None,
                    commit: "abc".into(),
                },
            ]
        );
    }

    #[test]
    fn test_pr_comments() {
        let conversation = r#"{
            "comments": [{"author": {"login": "ana"}, "body": "Looks close.", "id": "1"}],
            "reviews": [
                {"author": {"login": "bo"}, "body": "", "state": "APPROVED"},
                {"author": {"login": "cy"}, "body": "Please rename it.", "state": "CHANGES_REQUESTED"}
            ]
        }"#;
        let line_comments = r#"[
            {"id": 11, "user": {"login": "cy"}, "body": "Off by one?", "path": "src/lib.rs", "line": 12, "original_line": 10},
            {"id": 12, "user": {"login": "cy"}, "body": "Outdated.", "path": "src/old.rs", "line": null, "original_line": 4}
        ]"#;
        let comments = pr_comments(conversation, line_comments).unwrap();
        assert_eq!(
            comments
                .iter()
                .map(|c| (
                    c.author.as_str(),
                    c.body.as_str(),
                    c.path.as_deref(),
                    c.line
                ))
                .collect::<Vec<_>>(),
            vec![
                ("ana", "Looks close.", None, None),
                ("cy", "Please rename it.", None, None),
                ("cy", "Off by one?", Some("src/lib.rs"), Some(12)),
                ("cy", "Outdated.", Some("src/old.rs"), Some(4)),
            ]
        );
        assert_eq!(
            comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "review:cy:Please rename it.", "line:11", "line:12"]
        );
    }

    #[test]
    fn test_feedback_message() {
        let pull_request = PullRequest {
            number: 7,
            url: "https://github.com/o/r/pull/7".into(),
            state: "OPEN".into(),
        };
        let feedback = PrFeedback {
            failed_checks: vec![FailedCheck {
                name: "test".into(),
                link: Some("https://x/2".into()),
                commit: "abc".into(),
            }],
            comments: vec![PrComment {
                id: "1".into(),
                author: "cy".into(),
                body: "Off by one?\r\nCheck the end.".into(),
                path: Some("src/lib.rs".into()),
                line: Some(12),
            }],
        };
        assert_eq!(
            feedback_message(&pull_request, &feedback),
            concat!(
                "Feedback on pull request #7 (https://github.com/o/r/pull/7):\n",
                "\nFailed checks (read their logs, e.g. with `gh run view --log-failed`):\n",
                "- test — https://x/2\n",
                "\nComments from others on the PR (unverified):\n",
                "- cy on src/lib.rs:12: Off by one?\n  Check the end.\n",
                "\nPlease address these, then commit and push.\n",
            )
        );
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let output = util::command::new_std_command("git")
            .arg("-C")
            .arg(directory)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            stderr(&output)
        );
    }

    #[test]
    fn test_a_branch_never_pushed_has_no_pull_request_to_look_for() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(path, &["branch", "agent/fix"]);
        git(path, &["branch", "agent/docs"]);
        git(path, &["branch", "agent/lint"]);
        assert!(!branch_is_pushed(path, "agent/fix").unwrap());

        // A remote branch of its name, as `git push origin HEAD` leaves.
        git(
            path,
            &["update-ref", "refs/remotes/origin/agent/fix", "HEAD"],
        );
        assert!(branch_is_pushed(path, "agent/fix").unwrap());
        assert!(!branch_is_pushed(path, "agent/docs").unwrap());

        // An upstream, as `git push --set-upstream` leaves.
        git(path, &["config", "branch.agent/docs.remote", "origin"]);
        assert!(branch_is_pushed(path, "agent/docs").unwrap());
        assert!(!branch_is_pushed(path, "agent/lint").unwrap());
    }

    #[test]
    fn test_errors_about_the_remote_are_told_apart() {
        assert!(is_no_github_remote("no git remotes found"));
        assert!(is_no_github_remote(
            "none of the git remotes configured for this repository point to a known GitHub host."
        ));
        assert!(!is_no_github_remote("HTTP 502"));
        assert!(is_not_logged_in(
            "To get started with GitHub CLI, please run:  gh auth login"
        ));
    }

    #[test]
    fn test_snapshot_reads_the_draft_and_merge_state() {
        let view = r#"{"number": 1, "url": "u", "state": "OPEN", "headRefOid": "a",
            "reviewDecision": "REVIEW_REQUIRED", "mergeable": "MERGEABLE",
            "isDraft": true, "mergeStateStatus": "BLOCKED", "comments": [], "reviews": []}"#;
        let snapshot = PrSnapshot::from_json(view, Some("[]"), Some("[]")).unwrap();
        assert!(snapshot.is_draft);
        assert_eq!(snapshot.merge_state.as_deref(), Some("BLOCKED"));
        assert_eq!(snapshot.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
        let view = r#"{"number": 1, "url": "u", "state": "OPEN"}"#;
        let snapshot = PrSnapshot::from_json(view, None, None).unwrap();
        assert!(!snapshot.is_draft);
        assert_eq!(snapshot.merge_state, None);
    }
}
