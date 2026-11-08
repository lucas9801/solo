//! SOLO: a task's pull request on GitHub, through the `gh` CLI: opening it
//! from the task's branch, and gathering what its checks and reviewers said,
//! to send to the task's agent.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Output;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use super::git::{self, AgentTask};

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
}

/// A check that failed, with where to read its log.
#[derive(Clone, Debug, PartialEq)]
pub struct FailedCheck {
    pub name: String,
    pub link: Option<String>,
}

/// A comment on the pull request or on a line of its diff.
#[derive(Clone, Debug, PartialEq)]
pub struct PrComment {
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

fn gh<I, S>(directory: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    util::command::new_std_command("gh")
        .current_dir(directory)
        .args(arguments)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .output()
        .context("failed to run gh, GitHub's CLI; install it and run `gh auth login`")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
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
        bail!("{error}");
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
        .context("the task's worktree is gone")?;
    if !git::uncommitted_changes(worktree)?.is_empty() {
        bail!(
            "{} has uncommitted changes; commit them (or have the agent do it) first",
            task.branch
        );
    }
    if git::unmerged_commits(task)?.is_empty() {
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
        bail!("git push failed: {}", stderr(&push));
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
        bail!("gh pr create failed: {}", stderr(&create));
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

pub(crate) fn failed_checks(json: &str) -> Result<Vec<FailedCheck>> {
    let checks: Vec<Check> =
        serde_json::from_str(json).context("unexpected output from gh pr checks")?;
    Ok(checks
        .into_iter()
        .filter(|check| check.bucket == "fail")
        .map(|check| FailedCheck {
            name: check.name,
            link: check.link.filter(|link| !link.is_empty()),
        })
        .collect())
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

#[derive(Deserialize)]
struct Comment {
    author: Author,
    body: String,
}

#[derive(Deserialize)]
struct Review {
    author: Author,
    body: String,
}

#[derive(Deserialize)]
struct Conversation {
    #[serde(default)]
    comments: Vec<Comment>,
    #[serde(default)]
    reviews: Vec<Review>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct LineComment {
    user: User,
    body: String,
    path: String,
    line: Option<u64>,
    original_line: Option<u64>,
}

/// The comments and review summaries on the pull request (`gh pr view
/// --json comments,reviews`), then those on lines of its diff (`gh api
/// .../pulls/N/comments`), skipping empty ones.
pub(crate) fn pr_comments(conversation: &str, line_comments: &str) -> Result<Vec<PrComment>> {
    let conversation: Conversation =
        serde_json::from_str(conversation).context("unexpected output from gh pr view")?;
    let line_comments: Vec<LineComment> =
        serde_json::from_str(line_comments).context("unexpected output from gh api")?;
    let mut comments: Vec<PrComment> = conversation
        .comments
        .into_iter()
        .map(|comment| (comment.author.login, comment.body))
        .chain(
            conversation
                .reviews
                .into_iter()
                .map(|review| (review.author.login, review.body)),
        )
        .map(|(author, body)| PrComment {
            author,
            body,
            path: None,
            line: None,
        })
        .collect();
    comments.extend(line_comments.into_iter().map(|comment| PrComment {
        author: comment.user.login,
        body: comment.body,
        path: Some(comment.path),
        line: comment.line.or(comment.original_line),
    }));
    comments.retain(|comment| !comment.body.trim().is_empty());
    Ok(comments)
}

/// What the pull request's checks and reviewers said.
pub fn pull_request_feedback(worktree: &Path, number: u64) -> Result<PrFeedback> {
    let number = number.to_string();
    // gh exits with an error when checks fail or are pending; what it printed
    // still says which.
    let checks = gh(
        worktree,
        ["pr", "checks", &number, "--json", "name,bucket,link"],
    )?;
    let failed_checks = match stdout(&checks).trim() {
        "" if checks.status.success() => Vec::new(),
        "" => {
            let error = stderr(&checks);
            if error.to_lowercase().contains("no checks") {
                Vec::new()
            } else {
                bail!("gh pr checks failed: {error}");
            }
        }
        json => failed_checks(json)?,
    };
    let conversation = gh(
        worktree,
        ["pr", "view", &number, "--json", "comments,reviews"],
    )?;
    if !conversation.status.success() {
        bail!("gh pr view failed: {}", stderr(&conversation));
    }
    let line_comments = gh(
        worktree,
        [
            "api",
            &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments"),
        ],
    )?;
    if !line_comments.status.success() {
        bail!("gh api failed: {}", stderr(&line_comments));
    }
    Ok(PrFeedback {
        failed_checks,
        comments: pr_comments(&stdout(&conversation), &stdout(&line_comments))?,
    })
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
        message.push_str("\nComments:\n");
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
            failed_checks(json).unwrap(),
            vec![
                FailedCheck {
                    name: "test (windows)".into(),
                    link: Some("https://x/2".into())
                },
                FailedCheck {
                    name: "lint".into(),
                    link: None
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
            {"user": {"login": "cy"}, "body": "Off by one?", "path": "src/lib.rs", "line": 12, "original_line": 10},
            {"user": {"login": "cy"}, "body": "Outdated.", "path": "src/old.rs", "line": null, "original_line": 4}
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
            }],
            comments: vec![PrComment {
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
                "\nComments:\n",
                "- cy on src/lib.rs:12: Off by one?\n  Check the end.\n",
                "\nPlease address these, then commit and push.\n",
            )
        );
    }
}
