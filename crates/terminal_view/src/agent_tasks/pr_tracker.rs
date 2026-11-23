//! SOLO: follows the pull requests of agent tasks. While Solo runs it asks
//! `gh` about each task's open pull request every minute (while checks run)
//! or five minutes: the checks, the review, whether it can be merged, and the
//! comments. The agent board and the task list show the result; a desktop
//! notification says when something needs the user, and what was already sent
//! to an agent isn't new again, also after a restart.
//!
//! Everything `gh` runs in the background, two processes at most.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, Task, WeakEntity,
};
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use terminal::terminal_settings::TerminalSettings;
use ui::{Color, IconName};
use util::ResultExt as _;
use workspace::agent_activity::AgentState;

use super::{
    AgentTaskEntry,
    git::AgentTask,
    pull_request::{
        self, FailedCheck, GhUnavailable, PrComment, PrFeedback, PrSnapshot, PullRequest,
    },
    task_key,
};
use crate::TerminalView;

/// How often the tracker looks whether a pull request is due.
const TICK: Duration = Duration::from_secs(15);
/// How often a pull request is read while its checks run.
pub(crate) const CHECKING_INTERVAL: Duration = Duration::from_secs(60);
/// How often otherwise (also while looking for a task's pull request).
pub(crate) const QUIET_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How many pull requests are read at once; each read runs its `gh`
/// processes one after the other.
const MAX_FETCHES: usize = 2;
/// How many ids of each kind are remembered per task.
const MAX_REMEMBERED: usize = 500;

/// How long until a pull request is read again; `None` once it's merged or
/// closed.
pub(crate) fn poll_interval(snapshot: Option<&PrSnapshot>) -> Option<Duration> {
    match snapshot {
        None => Some(QUIET_INTERVAL),
        Some(snapshot) if !snapshot.pull_request.is_open() => None,
        Some(snapshot) if snapshot.has_pending_checks() => Some(CHECKING_INTERVAL),
        Some(_) => Some(QUIET_INTERVAL),
    }
}

/// What was sent to the task's agent, and what the user was told, so that
/// each failure and comment is new only once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct TaskRecord {
    #[serde(default)]
    sent_checks: Vec<String>,
    #[serde(default)]
    sent_comments: Vec<String>,
    #[serde(default)]
    notified_checks: Vec<String>,
    #[serde(default)]
    notified_comments: Vec<String>,
    /// Whether the pull request being ready to merge was told already; it's
    /// told again when it was not ready in between.
    #[serde(default)]
    ready_notified: bool,
}

fn remember(list: &mut Vec<String>, id: String) {
    if !list.contains(&id) {
        list.push(id);
        if list.len() > MAX_REMEMBERED {
            list.remove(0);
        }
    }
}

impl TaskRecord {
    pub(crate) fn mark_sent(&mut self, feedback: &PrFeedback) {
        for check in &feedback.failed_checks {
            remember(&mut self.sent_checks, check.key());
        }
        for comment in &feedback.comments {
            remember(&mut self.sent_comments, comment.id.clone());
        }
    }

    fn is_new_check(&self, check: &FailedCheck) -> bool {
        !self.sent_checks.contains(&check.key())
    }

    fn is_new_comment(&self, comment: &PrComment) -> bool {
        !self.sent_comments.contains(&comment.id)
    }

    /// What the checks and reviewers said that wasn't sent to the agent yet.
    pub(crate) fn unsent(&self, feedback: PrFeedback) -> PrFeedback {
        PrFeedback {
            failed_checks: feedback
                .failed_checks
                .into_iter()
                .filter(|check| self.is_new_check(check))
                .collect(),
            comments: feedback
                .comments
                .into_iter()
                .filter(|comment| self.is_new_comment(comment))
                .collect(),
        }
    }

    fn mark_notified(&mut self, alerts: &Alerts, ready: bool) {
        for check in &alerts.failed_checks {
            remember(&mut self.notified_checks, check.key());
        }
        for comment in &alerts.comments {
            remember(&mut self.notified_comments, comment.id.clone());
        }
        self.ready_notified = ready;
    }
}

/// Whether GitHub and the reviewers have nothing against merging: the checks
/// passed (or there are none), no changes are requested, and it merges
/// cleanly.
pub(crate) fn is_ready(snapshot: &PrSnapshot) -> bool {
    snapshot.pull_request.is_open()
        && !snapshot.has_pending_checks()
        && !snapshot
            .checks
            .iter()
            .any(pull_request::CheckRun::is_failed)
        && snapshot.review_decision.as_deref() != Some("CHANGES_REQUESTED")
        && snapshot.mergeable.as_deref() == Some("MERGEABLE")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PrKind {
    Merged,
    Closed,
    /// Failed checks or new comments.
    Problems,
    Checking,
    Ready,
    /// Open, and none of the above: a review is awaited, or it conflicts.
    Waiting,
}

/// What the board and the task list say of a task's pull request.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrSummary {
    pub number: u64,
    pub url: String,
    kind: PrKind,
    checks_done: usize,
    checks_total: usize,
    /// Failed checks not sent to the agent yet.
    failed_new: usize,
    /// Failed checks in all.
    failed_total: usize,
    new_comments: usize,
}

pub(crate) fn summarize(snapshot: &PrSnapshot, record: &TaskRecord) -> PrSummary {
    let failed = snapshot.failed_checks();
    let failed_new = failed
        .iter()
        .filter(|check| record.is_new_check(check))
        .count();
    let new_comments = snapshot
        .comments
        .iter()
        .filter(|comment| record.is_new_comment(comment))
        .count();
    let pending = snapshot.has_pending_checks();
    let mut summary = PrSummary {
        number: snapshot.pull_request.number,
        url: snapshot.pull_request.url.clone(),
        kind: PrKind::Waiting,
        checks_done: snapshot
            .checks
            .iter()
            .filter(|check| !check.is_pending())
            .count(),
        checks_total: snapshot.checks.len(),
        failed_new,
        failed_total: failed.len(),
        new_comments,
    };
    summary.kind = match snapshot.pull_request.state.as_str() {
        "MERGED" => PrKind::Merged,
        "CLOSED" => PrKind::Closed,
        _ if summary.failed_shown() > 0 || new_comments > 0 => PrKind::Problems,
        _ if pending => PrKind::Checking,
        _ if is_ready(snapshot) => PrKind::Ready,
        _ => PrKind::Waiting,
    };
    summary
}

fn count_label(
    count: usize,
    one: &'static str,
    many: &'static str,
    chinese: &'static str,
    is_chinese: bool,
) -> String {
    if is_chinese {
        chinese.replace("{}", &count.to_string())
    } else if count == 1 {
        one.to_string()
    } else {
        many.replace("{}", &count.to_string())
    }
}

impl PrSummary {
    pub(crate) fn kind(&self) -> PrKind {
        self.kind
    }

    /// The failed checks the row tells of: those not sent yet, or, once all
    /// were sent and nothing else is new, those that still fail.
    fn failed_shown(&self) -> usize {
        if self.failed_new > 0 {
            self.failed_new
        } else if self.new_comments == 0 {
            self.failed_total
        } else {
            0
        }
    }

    /// Whether there's something to send to the agent.
    pub(crate) fn has_unsent(&self) -> bool {
        self.failed_new > 0 || self.new_comments > 0
    }

    /// The state in words, for after the pull request's number. `None` when
    /// there's nothing to say.
    pub(crate) fn text(&self, chinese: bool) -> Option<String> {
        match self.kind {
            PrKind::Merged => Some(if chinese { "已合并" } else { "Merged" }.to_string()),
            PrKind::Closed => Some(if chinese { "已关闭" } else { "Closed" }.to_string()),
            PrKind::Checking => Some(if chinese {
                format!("检查中 {}/{}", self.checks_done, self.checks_total)
            } else {
                format!("Checking {}/{}", self.checks_done, self.checks_total)
            }),
            PrKind::Ready => Some(
                if chinese {
                    "可以合并"
                } else {
                    "Ready to merge"
                }
                .to_string(),
            ),
            PrKind::Waiting => None,
            PrKind::Problems => {
                let mut parts = Vec::new();
                let failed = self.failed_shown();
                if failed > 0 {
                    parts.push(count_label(
                        failed,
                        "1 check failed",
                        "{} checks failed",
                        "检查失败 {} 项",
                        chinese,
                    ));
                }
                if self.new_comments > 0 {
                    parts.push(count_label(
                        self.new_comments,
                        "1 new comment",
                        "{} new comments",
                        "{} 条新评论",
                        chinese,
                    ));
                }
                Some(parts.join(" · "))
            }
        }
    }

    /// The row's text: the number, then the state.
    pub(crate) fn line(&self, chinese: bool) -> String {
        match self.text(chinese) {
            Some(text) => format!("#{} · {}", self.number, text),
            None => format!("#{}", self.number),
        }
    }

    pub(crate) fn color(&self) -> Color {
        match self.kind {
            PrKind::Problems if self.failed_shown() > 0 => Color::Error,
            PrKind::Problems => Color::Warning,
            PrKind::Checking => Color::Info,
            PrKind::Ready => Color::Success,
            PrKind::Merged | PrKind::Closed | PrKind::Waiting => Color::Muted,
        }
    }

    pub(crate) fn icon(&self) -> IconName {
        match self.kind {
            PrKind::Problems if self.failed_shown() > 0 => IconName::XCircle,
            PrKind::Checking => IconName::LoadCircle,
            PrKind::Ready => IconName::Check,
            _ => IconName::PullRequest,
        }
    }
}

/// What a read of a pull request found that the user wasn't told of.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Alerts {
    pub failed_checks: Vec<FailedCheck>,
    pub comments: Vec<PrComment>,
    pub ready: bool,
}

impl Alerts {
    pub(crate) fn is_empty(&self) -> bool {
        self.failed_checks.is_empty() && self.comments.is_empty() && !self.ready
    }
}

/// Whether the row says the pull request can be merged: it can, and nothing
/// else on it is new.
pub(crate) fn shows_ready(snapshot: &PrSnapshot, record: &TaskRecord) -> bool {
    summarize(snapshot, record).kind() == PrKind::Ready
}

/// The failed checks and comments that are new (not sent to the agent) and
/// weren't told of yet, and whether being ready to merge is news.
pub(crate) fn new_alerts(snapshot: &PrSnapshot, record: &TaskRecord) -> Alerts {
    if !snapshot.pull_request.is_open() {
        return Alerts::default();
    }
    Alerts {
        failed_checks: snapshot
            .failed_checks()
            .into_iter()
            .filter(|check| {
                record.is_new_check(check) && !record.notified_checks.contains(&check.key())
            })
            .collect(),
        comments: snapshot
            .comments
            .iter()
            .filter(|comment| {
                record.is_new_comment(comment) && !record.notified_comments.contains(&comment.id)
            })
            .cloned()
            .collect(),
        ready: shows_ready(snapshot, record) && !record.ready_notified,
    }
}

fn first_line(text: &str, limit: usize) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

/// The title and body of the notification for `alerts`.
pub(crate) fn notification_text(
    task_name: &str,
    number: u64,
    alerts: &Alerts,
    chinese: bool,
) -> (String, String) {
    let mut titles = Vec::new();
    let mut details = Vec::new();
    if !alerts.failed_checks.is_empty() {
        titles.push(
            if chinese {
                "检查失败"
            } else {
                "checks failed"
            }
            .to_string(),
        );
        let names: Vec<&str> = alerts
            .failed_checks
            .iter()
            .take(3)
            .map(|check| check.name.as_str())
            .collect();
        let more = alerts.failed_checks.len() - names.len();
        let mut list = names.join(", ");
        if more > 0 {
            list.push_str(&format!(" +{more}"));
        }
        details.push(if chinese {
            format!("{list} 没通过")
        } else {
            format!("{list} failed")
        });
    }
    if let Some(comment) = alerts.comments.last() {
        titles.push(count_label(
            alerts.comments.len(),
            "1 new comment",
            "{} new comments",
            "{} 条新评论",
            chinese,
        ));
        details.push(format!(
            "{}: {}",
            comment.author,
            first_line(&comment.body, 80)
        ));
    }
    if alerts.ready {
        titles.push(
            if chinese {
                "可以合并"
            } else {
                "ready to merge"
            }
            .to_string(),
        );
        details.push(
            if chinese {
                "检查都通过了"
            } else {
                "all checks passed"
            }
            .to_string(),
        );
    }
    (
        format!("PR #{number} {}", titles.join(" · ")),
        format!("{task_name}: {}", details.join("; ")),
    )
}

/// Where the tracker gets pull requests from.
pub(crate) trait PrSource: Send + Sync + 'static {
    fn fetch(&self, worktree: &Path, branch: &str) -> Result<Option<PrSnapshot>>;
}

struct GhSource;

impl PrSource for GhSource {
    fn fetch(&self, worktree: &Path, branch: &str) -> Result<Option<PrSnapshot>> {
        pull_request::fetch_snapshot(worktree, branch)
    }
}

/// A notification the user should get.
#[derive(Clone)]
pub(crate) struct PrNotification {
    pub key: String,
    pub title: String,
    pub body: String,
    /// The terminals of the task; clicking shows one.
    pub terminals: Vec<WeakEntity<TerminalView>>,
}

pub(crate) enum PrTrackerEvent {
    Notify(PrNotification),
}

/// What sending the new feedback to the agent came to.
pub(crate) enum SendOutcome {
    /// Pasted into this terminal, whose agent takes it.
    Sent(Entity<TerminalView>),
    /// No terminal of the task runs an agent that can take it.
    NoAgent(String),
    NothingNew,
}

struct Tracked {
    task: AgentTask,
    /// The workspace that listed the task.
    owner: EntityId,
    terminals: Vec<WeakEntity<TerminalView>>,
    snapshot: Option<PrSnapshot>,
    checked_at: Option<Instant>,
    fetching: bool,
}

impl Tracked {
    fn is_due(&self, now: Instant) -> bool {
        if self.fetching || self.task.worktree_path.is_none() {
            return false;
        }
        let Some(interval) = poll_interval(self.snapshot.as_ref()) else {
            return false;
        };
        self.checked_at
            .is_none_or(|checked_at| now.saturating_duration_since(checked_at) >= interval)
    }
}

pub(crate) struct PrTracker {
    tasks: HashMap<String, Tracked>,
    records: HashMap<String, TaskRecord>,
    source: Arc<dyn PrSource>,
    /// Where the records are kept; none in tests.
    store: Option<PathBuf>,
    fetches: usize,
    last_error: Option<String>,
    saving: Task<()>,
    _ticks: Task<()>,
}

impl EventEmitter<PrTrackerEvent> for PrTracker {}

struct GlobalPrTracker(Entity<PrTracker>);

impl Global for GlobalPrTracker {}

fn store_path() -> PathBuf {
    paths::data_dir().join("pull_request_follow_up.json")
}

fn load_records(path: &Path) -> HashMap<String, TaskRecord> {
    let json = match std::fs::read(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(error) => {
            log::warn!("failed to read {}: {error}", path.display());
            return HashMap::new();
        }
    };
    serde_json::from_slice(&json)
        .map_err(|error| log::warn!("failed to read {}: {error}", path.display()))
        .unwrap_or_default()
}

fn save_records(path: &Path, records: &HashMap<String, TaskRecord>) -> Result<()> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let json = serde_json::to_vec(records)?;
    // Write a temporary file first, so quitting mid-write can't leave a
    // truncated one.
    let temp_path = path.with_extension("json.tmp");
    std::fs::write(&temp_path, json)?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

fn checking_enabled(cx: &App) -> bool {
    TerminalSettings::try_get(cx).is_some_and(|settings| settings.check_pull_requests)
}

impl PrTracker {
    /// The one tracker of the app, started by the first that asks.
    pub(crate) fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<GlobalPrTracker>() {
            return global.0.clone();
        }
        let tracker = cx.new(|cx| Self::new(Arc::new(GhSource), Some(store_path()), cx));
        cx.subscribe(&tracker, |_, event, cx| match event {
            PrTrackerEvent::Notify(notification) => {
                crate::command_notifications::show_pull_request_notification(notification, cx)
            }
        })
        .detach();
        cx.set_global(GlobalPrTracker(tracker.clone()));
        tracker
    }

    pub(crate) fn try_global(cx: &App) -> Option<&Entity<Self>> {
        cx.try_global::<GlobalPrTracker>().map(|global| &global.0)
    }

    pub(crate) fn new(
        source: Arc<dyn PrSource>,
        store: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let records = store.as_deref().map(load_records).unwrap_or_default();
        let ticks = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let now = cx.background_executor().now();
                if this.update(cx, |this, cx| this.tick(now, cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            tasks: HashMap::new(),
            records,
            source,
            store,
            fetches: 0,
            last_error: None,
            saving: Task::ready(()),
            _ticks: ticks,
        }
    }

    /// Replaces the tasks `owner` (a workspace) lists. What's known of a task
    /// that stays is kept.
    pub(crate) fn set_tasks(
        &mut self,
        owner: EntityId,
        entries: &[AgentTaskEntry],
        cx: &mut Context<Self>,
    ) {
        let keys: HashSet<String> = entries.iter().map(|entry| task_key(&entry.task)).collect();
        self.tasks
            .retain(|key, tracked| tracked.owner != owner || keys.contains(key));
        for entry in entries {
            let terminals = entry.terminals.iter().map(Entity::downgrade).collect();
            match self.tasks.get_mut(&task_key(&entry.task)) {
                Some(tracked) => {
                    tracked.task = entry.task.clone();
                    tracked.owner = owner;
                    tracked.terminals = terminals;
                }
                None => {
                    self.tasks.insert(
                        task_key(&entry.task),
                        Tracked {
                            task: entry.task.clone(),
                            owner,
                            terminals,
                            snapshot: None,
                            checked_at: None,
                            fetching: false,
                        },
                    );
                }
            }
        }
        cx.notify();
    }

    /// Reads the task's pull request at the next tick, e.g. after opening it.
    pub(crate) fn check_soon(&mut self, key: &str) {
        if let Some(tracked) = self.tasks.get_mut(key) {
            tracked.checked_at = None;
        }
    }

    pub(crate) fn summary(&self, key: &str) -> Option<PrSummary> {
        let snapshot = self.tasks.get(key)?.snapshot.as_ref()?;
        Some(summarize(
            snapshot,
            self.records.get(key).unwrap_or(&TaskRecord::default()),
        ))
    }

    /// The pull request's checks and comments that weren't sent to the
    /// agent, if there are any.
    pub(crate) fn unsent_feedback(&self, key: &str) -> Option<(PullRequest, PrFeedback)> {
        let snapshot = self.tasks.get(key)?.snapshot.as_ref()?;
        if !snapshot.pull_request.is_open() {
            return None;
        }
        let feedback = self
            .records
            .get(key)
            .unwrap_or(&TaskRecord::default())
            .unsent(snapshot.feedback());
        (!feedback.is_empty()).then(|| (snapshot.pull_request.clone(), feedback))
    }

    /// Remembers that the agent was sent `feedback`, by hand or not.
    pub(crate) fn mark_sent(&mut self, key: &str, feedback: &PrFeedback, cx: &mut Context<Self>) {
        self.records
            .entry(key.to_string())
            .or_default()
            .mark_sent(feedback);
        self.save(cx);
        cx.notify();
    }

    /// Pastes the new feedback into the task's agent (without pressing
    /// Enter). With `only_idle`, only into an agent that waits for its next
    /// prompt.
    pub(crate) fn send_unsent(
        &mut self,
        key: &str,
        only_idle: bool,
        cx: &mut Context<Self>,
    ) -> SendOutcome {
        let Some((pull_request, feedback)) = self.unsent_feedback(key) else {
            return SendOutcome::NothingNew;
        };
        let message = pull_request::feedback_message(&pull_request, &feedback);
        let agent = self
            .tasks
            .get(key)
            .into_iter()
            .flat_map(|tracked| tracked.terminals.iter())
            .filter_map(WeakEntity::upgrade)
            .find(|view| {
                let view = view.read(cx);
                crate::agent_context::can_paste_to_agent(view, &message, cx)
                    && (!only_idle
                        || view.agent_status().is_some_and(|(_, status)| {
                            matches!(status, AgentState::Idle | AgentState::Finished)
                        }))
            });
        match agent {
            Some(agent) => {
                agent.update(cx, |agent, cx| {
                    agent
                        .terminal
                        .update(cx, |terminal, _| terminal.paste(&message))
                });
                self.mark_sent(key, &feedback, cx);
                SendOutcome::Sent(agent)
            }
            None => SendOutcome::NoAgent(message),
        }
    }

    fn tick(&mut self, now: Instant, cx: &mut Context<Self>) {
        if !checking_enabled(cx) {
            return;
        }
        let mut due: Vec<(Option<Instant>, String)> = self
            .tasks
            .iter()
            .filter(|(_, tracked)| tracked.is_due(now))
            .map(|(key, tracked)| (tracked.checked_at, key.clone()))
            .collect();
        due.sort();
        for (_, key) in due {
            if self.fetches >= MAX_FETCHES {
                break;
            }
            self.start_fetch(key, cx);
        }
    }

    fn start_fetch(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(tracked) = self.tasks.get_mut(&key) else {
            return;
        };
        let Some(worktree) = tracked.task.worktree_path.clone() else {
            return;
        };
        let branch = tracked.task.branch.clone();
        tracked.fetching = true;
        self.fetches += 1;
        let source = self.source.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { source.fetch(&worktree, &branch) })
                .await;
            this.update(cx, |this, cx| this.finish_fetch(&key, result, cx))
                .log_err();
        })
        .detach();
    }

    fn finish_fetch(
        &mut self,
        key: &str,
        result: Result<Option<PrSnapshot>>,
        cx: &mut Context<Self>,
    ) {
        self.fetches = self.fetches.saturating_sub(1);
        let now = cx.background_executor().now();
        let Some(tracked) = self.tasks.get_mut(key) else {
            return;
        };
        tracked.fetching = false;
        tracked.checked_at = Some(now);
        let snapshot = match result {
            Ok(snapshot) => {
                self.last_error = None;
                snapshot
            }
            Err(error) => {
                // Once, not at every read.
                let message = format!("{error:#}");
                if self.last_error.as_deref() != Some(message.as_str()) {
                    if error.downcast_ref::<GhUnavailable>().is_some() {
                        log::warn!("not checking pull requests: {message}");
                    } else {
                        log::warn!("failed to check the pull request of {key:?}: {message}");
                    }
                    self.last_error = Some(message);
                }
                return;
            }
        };
        tracked.snapshot = snapshot.clone();
        if let Some(snapshot) = snapshot {
            self.report(key, &snapshot, cx);
        }
        cx.notify();
    }

    /// Tells the user what's new about the pull request, and sends it to the
    /// agent if that's turned on.
    fn report(&mut self, key: &str, snapshot: &PrSnapshot, cx: &mut Context<Self>) {
        let record = self.records.entry(key.to_string()).or_default();
        let alerts = new_alerts(snapshot, record);
        let before = record.clone();
        let ready = shows_ready(snapshot, record);
        record.mark_notified(&alerts, ready);
        let changed = *record != before;
        if !alerts.is_empty()
            && let Some(tracked) = self.tasks.get(key)
        {
            let (title, body) = notification_text(
                &super::display_name(&tracked.task),
                snapshot.pull_request.number,
                &alerts,
                util::i18n::is_chinese(),
            );
            cx.emit(PrTrackerEvent::Notify(PrNotification {
                key: key.to_string(),
                title,
                body,
                terminals: tracked.terminals.clone(),
            }));
        }
        if changed {
            self.save(cx);
        }
        if TerminalSettings::get_global(cx).auto_send_pr_feedback {
            self.send_unsent(key, true, cx);
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.store.clone() else {
            return;
        };
        let records = self.records.clone();
        // One write after the other.
        let previous = std::mem::replace(&mut self.saving, Task::ready(()));
        self.saving = cx.background_spawn(async move {
            previous.await;
            save_records(&path, &records).log_err();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tasks::pull_request::{CheckRun, PrComment};

    fn snapshot(state: &str, checks: &[(&str, &str)], comments: &[&str]) -> PrSnapshot {
        PrSnapshot {
            pull_request: PullRequest {
                number: 42,
                url: "https://github.com/o/r/pull/42".into(),
                state: state.into(),
            },
            head_commit: "abc".into(),
            checks: checks
                .iter()
                .map(|(name, bucket)| CheckRun {
                    name: name.to_string(),
                    bucket: bucket.to_string(),
                    link: None,
                })
                .collect(),
            review_decision: None,
            mergeable: Some("MERGEABLE".into()),
            comments: comments
                .iter()
                .map(|id| PrComment {
                    id: id.to_string(),
                    author: "cy".into(),
                    body: format!("comment {id}"),
                    path: None,
                    line: None,
                })
                .collect(),
        }
    }

    fn line(snapshot: &PrSnapshot, record: &TaskRecord, chinese: bool) -> String {
        summarize(snapshot, record).line(chinese)
    }

    #[test]
    fn test_six_states_in_both_languages() {
        let none = TaskRecord::default();
        let checking = snapshot(
            "OPEN",
            &[("a", "pass"), ("b", "pending"), ("c", "pending")],
            &[],
        );
        assert_eq!(line(&checking, &none, true), "#42 · 检查中 1/3");
        assert_eq!(line(&checking, &none, false), "#42 · Checking 1/3");
        let failed = snapshot("OPEN", &[("a", "pass"), ("b", "fail")], &[]);
        assert_eq!(line(&failed, &none, true), "#42 · 检查失败 1 项");
        assert_eq!(line(&failed, &none, false), "#42 · 1 check failed");
        let commented = snapshot("OPEN", &[("a", "pass")], &["1", "2"]);
        assert_eq!(line(&commented, &none, true), "#42 · 2 条新评论");
        assert_eq!(line(&commented, &none, false), "#42 · 2 new comments");
        let ready = snapshot("OPEN", &[("a", "pass"), ("b", "skipping")], &[]);
        assert_eq!(line(&ready, &none, true), "#42 · 可以合并");
        assert_eq!(line(&ready, &none, false), "#42 · Ready to merge");
        assert_eq!(
            line(&snapshot("MERGED", &[], &[]), &none, true),
            "#42 · 已合并"
        );
        assert_eq!(
            line(&snapshot("CLOSED", &[], &[]), &none, false),
            "#42 · Closed"
        );
    }

    #[test]
    fn test_failures_and_comments_are_written_together() {
        let both = snapshot("OPEN", &[("b", "fail"), ("c", "fail")], &["1"]);
        let none = TaskRecord::default();
        assert_eq!(line(&both, &none, true), "#42 · 检查失败 2 项 · 1 条新评论");
        assert_eq!(
            line(&both, &none, false),
            "#42 · 2 checks failed · 1 new comment"
        );
        assert!(summarize(&both, &none).has_unsent());
    }

    #[test]
    fn test_ready_needs_everything_to_agree() {
        let mut open = snapshot("OPEN", &[], &[]);
        assert!(is_ready(&open), "no checks at all is fine");
        open.review_decision = Some("CHANGES_REQUESTED".into());
        assert!(!is_ready(&open));
        open.review_decision = Some("APPROVED".into());
        assert!(is_ready(&open));
        open.mergeable = Some("CONFLICTING".into());
        assert!(!is_ready(&open));
        open.mergeable = Some("UNKNOWN".into());
        assert!(!is_ready(&open));
        let mut waiting = snapshot("OPEN", &[("a", "pass")], &[]);
        waiting.mergeable = Some("CONFLICTING".into());
        assert_eq!(
            summarize(&waiting, &TaskRecord::default()).line(true),
            "#42"
        );
        assert!(!is_ready(&snapshot("MERGED", &[], &[])));
    }

    #[test]
    fn test_what_was_sent_is_not_new() {
        let first = snapshot("OPEN", &[("build", "fail")], &["1"]);
        let mut record = TaskRecord::default();
        let feedback = record.unsent(first.feedback());
        assert_eq!(feedback.failed_checks.len(), 1);
        assert_eq!(feedback.comments.len(), 1);
        record.mark_sent(&feedback);

        // The failure stays on the row, but isn't new; nor is the comment.
        let summary = summarize(&first, &record);
        assert!(!summary.has_unsent());
        assert_eq!(summary.line(true), "#42 · 检查失败 1 项");
        assert!(record.unsent(first.feedback()).is_empty());

        // A new comment is the only thing the row says (the failure was sent).
        let second = snapshot("OPEN", &[("build", "fail")], &["1", "2"]);
        let summary = summarize(&second, &record);
        assert_eq!(summary.line(true), "#42 · 1 条新评论");
        assert_eq!(record.unsent(second.feedback()).comments[0].id, "2");

        // The same check failing on a new commit is new.
        let mut third = second;
        third.head_commit = "def".into();
        assert_eq!(record.unsent(third.feedback()).failed_checks.len(), 1);
    }

    #[test]
    fn test_record_survives_a_restart() {
        let mut record = TaskRecord::default();
        record.mark_sent(&snapshot("OPEN", &[("build", "fail")], &["7"]).feedback());
        let json =
            serde_json::to_string(&HashMap::from([("task".to_string(), record.clone())])).unwrap();
        let loaded: HashMap<String, TaskRecord> = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded["task"], record);
        assert!(
            loaded["task"]
                .unsent(snapshot("OPEN", &[("build", "fail")], &["7"]).feedback())
                .is_empty()
        );
        let older: HashMap<String, TaskRecord> = serde_json::from_str(r#"{"task": {}}"#).unwrap();
        assert_eq!(older["task"], TaskRecord::default());
    }

    #[test]
    fn test_record_files_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("records.json");
        assert!(load_records(&path).is_empty());
        let mut record = TaskRecord::default();
        record.mark_sent(&snapshot("OPEN", &[("build", "fail")], &["7"]).feedback());
        let records = HashMap::from([("task".to_string(), record)]);
        save_records(&path, &records).unwrap();
        assert_eq!(load_records(&path), records);
        std::fs::write(&path, "not json").unwrap();
        assert!(load_records(&path).is_empty());
    }

    #[test]
    fn test_remembered_ids_are_bounded() {
        let mut list = Vec::new();
        for id in 0..MAX_REMEMBERED + 10 {
            remember(&mut list, id.to_string());
        }
        remember(&mut list, "20".to_string());
        assert_eq!(list.len(), MAX_REMEMBERED);
        assert_eq!(list[0], "10");
    }

    #[test]
    fn test_poll_interval() {
        let running = snapshot("OPEN", &[("a", "pending")], &[]);
        let quiet = snapshot("OPEN", &[("a", "pass")], &[]);
        assert_eq!(poll_interval(Some(&running)), Some(CHECKING_INTERVAL));
        assert_eq!(poll_interval(Some(&quiet)), Some(QUIET_INTERVAL));
        assert_eq!(poll_interval(None), Some(QUIET_INTERVAL));
        assert_eq!(poll_interval(Some(&snapshot("MERGED", &[], &[]))), None);
        assert_eq!(poll_interval(Some(&snapshot("CLOSED", &[], &[]))), None);
    }

    #[test]
    fn test_alerts_come_once() {
        let mut record = TaskRecord::default();
        let failing = snapshot("OPEN", &[("build", "fail")], &["1"]);
        let alerts = new_alerts(&failing, &record);
        assert_eq!(alerts.failed_checks.len(), 1);
        assert_eq!(alerts.comments.len(), 1);
        assert!(!alerts.ready);
        record.mark_notified(&alerts, false);
        assert!(new_alerts(&failing, &record).is_empty(), "told already");

        // A new comment is told, the old failure isn't told again.
        let more = snapshot("OPEN", &[("build", "fail")], &["1", "2"]);
        let alerts = new_alerts(&more, &record);
        assert!(alerts.failed_checks.is_empty());
        assert_eq!(alerts.comments.len(), 1);
        record.mark_notified(&alerts, false);

        // Ready to merge is told once, and again after it wasn't ready.
        let ready = snapshot("OPEN", &[("build", "pass")], &[]);
        let alerts = new_alerts(&ready, &record);
        assert!(alerts.ready);
        record.mark_notified(&alerts, true);
        assert!(new_alerts(&ready, &record).is_empty());
        let running = snapshot("OPEN", &[("build", "pending")], &[]);
        record.mark_notified(&new_alerts(&running, &record), false);
        assert!(new_alerts(&ready, &record).ready);
    }

    #[test]
    fn test_sent_feedback_is_not_told_of() {
        let mut record = TaskRecord::default();
        let failing = snapshot("OPEN", &[("build", "fail")], &["1"]);
        record.mark_sent(&failing.feedback());
        assert!(new_alerts(&failing, &record).is_empty());
    }

    #[test]
    fn test_nothing_is_told_of_a_closed_pull_request() {
        let merged = snapshot("MERGED", &[("build", "fail")], &["1"]);
        assert!(new_alerts(&merged, &TaskRecord::default()).is_empty());
    }

    #[test]
    fn test_notification_text() {
        let alerts = Alerts {
            failed_checks: vec![
                FailedCheck {
                    name: "build-windows".into(),
                    link: None,
                    commit: "abc".into(),
                },
                FailedCheck {
                    name: "test".into(),
                    link: None,
                    commit: "abc".into(),
                },
            ],
            comments: Vec::new(),
            ready: false,
        };
        assert_eq!(
            notification_text("reparent", 42, &alerts, true),
            (
                "PR #42 检查失败".to_string(),
                "reparent: build-windows, test 没通过".to_string()
            )
        );
        assert_eq!(
            notification_text("reparent", 42, &alerts, false),
            (
                "PR #42 checks failed".to_string(),
                "reparent: build-windows, test failed".to_string()
            )
        );
        let alerts = Alerts {
            failed_checks: Vec::new(),
            comments: vec![PrComment {
                id: "1".into(),
                author: "cy".into(),
                body: "Please rename it.\nAnd more.".into(),
                path: None,
                line: None,
            }],
            ready: true,
        };
        assert_eq!(
            notification_text("reparent", 42, &alerts, false),
            (
                "PR #42 1 new comment · ready to merge".to_string(),
                "reparent: cy: Please rename it.; all checks passed".to_string()
            )
        );
    }

    use std::{
        cell::RefCell,
        rc::Rc,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use gpui::TestAppContext;

    #[derive(Default)]
    struct FakeSource {
        response: Mutex<Option<PrSnapshot>>,
        error: Mutex<Option<String>>,
        calls: AtomicUsize,
    }

    impl FakeSource {
        fn respond(&self, snapshot: PrSnapshot) {
            *self.response.lock().unwrap() = Some(snapshot);
            *self.error.lock().unwrap() = None;
        }

        fn fail(&self, message: &str) {
            *self.error.lock().unwrap() = Some(message.to_string());
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl PrSource for FakeSource {
        fn fetch(&self, _: &Path, _: &str) -> Result<Option<PrSnapshot>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(message) = self.error.lock().unwrap().clone() {
                anyhow::bail!(message);
            }
            Ok(self.response.lock().unwrap().clone())
        }
    }

    fn entry(name: &str, terminals: Vec<Entity<TerminalView>>) -> AgentTaskEntry {
        AgentTaskEntry {
            task: AgentTask {
                name: name.to_string(),
                branch: format!("agent/{name}"),
                base_branch: "main".to_string(),
                worktree_path: Some(PathBuf::from(format!("/work/{name}"))),
                repository_root: PathBuf::from("/repo"),
                port: None,
                group: None,
            },
            terminals,
        }
    }

    fn key(name: &str) -> String {
        task_key(&entry(name, Vec::new()).task)
    }

    /// Moves the clock forward as the tracker's own timer sees it.
    fn advance(cx: &mut TestAppContext, duration: Duration) {
        let mut elapsed = Duration::ZERO;
        while elapsed < duration {
            cx.executor().advance_clock(TICK);
            cx.run_until_parked();
            elapsed += TICK;
        }
    }

    fn update_terminal_settings(
        cx: &mut TestAppContext,
        update: impl FnOnce(&mut settings::TerminalSettingsContent),
    ) {
        use gpui::UpdateGlobal as _;
        cx.update(|cx| {
            settings::SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    update(settings.terminal.get_or_insert_default())
                });
            })
        });
    }

    type Notifications = Rc<RefCell<Vec<(String, String)>>>;

    fn collect_notifications(
        tracker: &Entity<PrTracker>,
        cx: &mut TestAppContext,
    ) -> Notifications {
        let notifications = Notifications::default();
        cx.update(|cx| {
            let notifications = notifications.clone();
            cx.subscribe(tracker, move |_, event, _| match event {
                PrTrackerEvent::Notify(notification) => notifications
                    .borrow_mut()
                    .push((notification.title.clone(), notification.body.clone())),
            })
            .detach();
        });
        notifications
    }

    fn titles(notifications: &Notifications) -> Vec<String> {
        notifications
            .borrow()
            .iter()
            .map(|(title, _)| title.clone())
            .collect()
    }

    fn set_tasks(tracker: &Entity<PrTracker>, entries: &[AgentTaskEntry], cx: &mut TestAppContext) {
        tracker.update(cx, |tracker, cx| {
            tracker.set_tasks(EntityId::from(1), entries, cx)
        });
    }

    fn summary_line(
        tracker: &Entity<PrTracker>,
        name: &str,
        chinese: bool,
        cx: &TestAppContext,
    ) -> Option<String> {
        tracker
            .read_with(cx, |tracker, _| tracker.summary(&key(name)))
            .map(|summary| summary.line(chinese))
    }

    #[gpui::test]
    async fn test_default_settings_check_at_the_intervals(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        let source = Arc::new(FakeSource::default());
        source.respond(snapshot("OPEN", &[("build", "pending")], &[]));
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        set_tasks(&tracker, &[entry("a", Vec::new())], cx);
        assert_eq!(source.calls(), 0);

        // No setting touched: checking is on by default, and the first read
        // comes at the first tick.
        advance(cx, TICK);
        assert_eq!(source.calls(), 1);
        assert_eq!(
            summary_line(&tracker, "a", true, cx).as_deref(),
            Some("#42 · 检查中 0/1")
        );

        // Checks are running: once a minute.
        advance(cx, Duration::from_secs(45));
        assert_eq!(source.calls(), 1);
        advance(cx, TICK);
        assert_eq!(source.calls(), 2);

        // Once they're done: once in five minutes.
        source.respond(snapshot("OPEN", &[("build", "pass")], &[]));
        advance(cx, CHECKING_INTERVAL);
        assert_eq!(source.calls(), 3);
        advance(cx, QUIET_INTERVAL - TICK);
        assert_eq!(source.calls(), 3);
        advance(cx, TICK);
        assert_eq!(source.calls(), 4);

        // Merged: never again.
        source.respond(snapshot("MERGED", &[], &[]));
        advance(cx, QUIET_INTERVAL);
        assert_eq!(source.calls(), 5);
        advance(cx, QUIET_INTERVAL * 4);
        assert_eq!(source.calls(), 5);
        assert_eq!(
            summary_line(&tracker, "a", false, cx).as_deref(),
            Some("#42 · Merged")
        );
    }

    #[gpui::test]
    async fn test_a_task_without_a_pull_request_is_looked_up_every_five_minutes(
        cx: &mut TestAppContext,
    ) {
        crate::tests::init_test(cx).await;
        let source = Arc::new(FakeSource::default());
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        set_tasks(&tracker, &[entry("a", Vec::new())], cx);
        advance(cx, TICK);
        assert_eq!(source.calls(), 1);
        advance(cx, QUIET_INTERVAL - TICK * 2);
        assert_eq!(source.calls(), 1);
        advance(cx, TICK * 2);
        assert_eq!(source.calls(), 2);
        assert_eq!(summary_line(&tracker, "a", false, cx), None);

        // Opening the pull request brings the next look forward.
        source.respond(snapshot("OPEN", &[], &[]));
        tracker.update(cx, |tracker, _| tracker.check_soon(&key("a")));
        advance(cx, TICK);
        assert_eq!(source.calls(), 3);
        assert!(summary_line(&tracker, "a", false, cx).is_some());
    }

    #[gpui::test]
    async fn test_turning_the_setting_off_runs_no_gh(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        update_terminal_settings(cx, |terminal| terminal.check_pull_requests = Some(false));
        let source = Arc::new(FakeSource::default());
        source.respond(snapshot("OPEN", &[("build", "pending")], &[]));
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        set_tasks(&tracker, &[entry("a", Vec::new())], cx);
        advance(cx, QUIET_INTERVAL * 3);
        assert_eq!(source.calls(), 0);

        update_terminal_settings(cx, |terminal| terminal.check_pull_requests = Some(true));
        advance(cx, TICK);
        assert_eq!(source.calls(), 1);

        update_terminal_settings(cx, |terminal| terminal.check_pull_requests = Some(false));
        advance(cx, QUIET_INTERVAL * 3);
        assert_eq!(source.calls(), 1);
    }

    #[gpui::test]
    async fn test_at_most_two_pull_requests_are_read_at_once(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        let source = Arc::new(FakeSource::default());
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        let entries: Vec<_> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| entry(name, Vec::new()))
            .collect();
        set_tasks(&tracker, &entries, cx);
        let now = cx.executor().now();
        tracker.update(cx, |tracker, cx| tracker.tick(now, cx));
        assert_eq!(tracker.read_with(cx, |tracker, _| tracker.fetches), 2);
        cx.run_until_parked();
        assert_eq!(source.calls(), 2);
        assert_eq!(tracker.read_with(cx, |tracker, _| tracker.fetches), 0);
        // The rest follow, two at a time.
        tracker.update(cx, |tracker, cx| tracker.tick(now, cx));
        assert_eq!(tracker.read_with(cx, |tracker, _| tracker.fetches), 2);
        cx.run_until_parked();
        tracker.update(cx, |tracker, cx| tracker.tick(now, cx));
        assert_eq!(tracker.read_with(cx, |tracker, _| tracker.fetches), 1);
        cx.run_until_parked();
        assert_eq!(source.calls(), 5);
    }

    #[gpui::test]
    async fn test_failures_are_not_fatal_and_recover(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        let source = Arc::new(FakeSource::default());
        source.fail("gh isn't installed");
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        set_tasks(&tracker, &[entry("a", Vec::new())], cx);
        advance(cx, QUIET_INTERVAL * 3);
        assert_eq!(source.calls(), 3, "tried every five minutes, not more");
        assert_eq!(
            tracker.read_with(cx, |tracker, _| tracker.last_error.clone()),
            Some("gh isn't installed".to_string())
        );
        assert_eq!(summary_line(&tracker, "a", false, cx), None);

        source.respond(snapshot("OPEN", &[("build", "pass")], &[]));
        advance(cx, QUIET_INTERVAL);
        assert_eq!(
            tracker.read_with(cx, |tracker, _| tracker.last_error.clone()),
            None
        );
        assert!(summary_line(&tracker, "a", false, cx).is_some());
    }

    #[gpui::test]
    async fn test_each_change_is_told_once(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        let source = Arc::new(FakeSource::default());
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        let notifications = collect_notifications(&tracker, cx);
        set_tasks(&tracker, &[entry("fix", Vec::new())], cx);

        source.respond(snapshot("OPEN", &[("build", "pending")], &[]));
        advance(cx, TICK);
        assert!(titles(&notifications).is_empty(), "checks are running");

        // A check fails: told once, however often it's read.
        source.respond(snapshot(
            "OPEN",
            &[("build", "fail"), ("lint", "pending")],
            &[],
        ));
        advance(cx, CHECKING_INTERVAL * 3);
        assert_eq!(titles(&notifications), ["PR #42 checks failed"]);
        assert_eq!(notifications.borrow()[0].1, "fix: build failed");

        // A comment: told once more.
        source.respond(snapshot(
            "OPEN",
            &[("build", "fail"), ("lint", "pending")],
            &["1"],
        ));
        advance(cx, CHECKING_INTERVAL * 3);
        assert_eq!(
            titles(&notifications),
            ["PR #42 checks failed", "PR #42 1 new comment"]
        );

        // Fixed, and the same comment is still there: nothing new, and
        // being ready to merge is told once it is (with the comment sent).
        let both_pass = snapshot("OPEN", &[("build", "pass"), ("lint", "pass")], &["1"]);
        source.respond(both_pass.clone());
        advance(cx, QUIET_INTERVAL * 2);
        assert_eq!(titles(&notifications).len(), 2);
        let (_, feedback) = tracker
            .read_with(cx, |tracker, _| tracker.unsent_feedback(&key("fix")))
            .unwrap();
        tracker.update(cx, |tracker, cx| {
            tracker.mark_sent(&key("fix"), &feedback, cx)
        });
        let mut ready = both_pass;
        ready.comments.clear();
        source.respond(ready);
        advance(cx, QUIET_INTERVAL * 3);
        assert_eq!(
            titles(&notifications).last().map(String::as_str),
            Some("PR #42 ready to merge")
        );
        assert_eq!(titles(&notifications).len(), 3);

        // Not ready for a moment (a new commit's checks run), then ready
        // again: told again.
        source.respond(snapshot("OPEN", &[("build", "pending")], &[]));
        advance(cx, CHECKING_INTERVAL);
        source.respond(snapshot("OPEN", &[("build", "pass")], &[]));
        advance(cx, CHECKING_INTERVAL);
        assert_eq!(titles(&notifications).len(), 4);
    }

    #[gpui::test]
    async fn test_what_was_sent_stays_sent_after_a_restart(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("pull_request_follow_up.json");
        let source = Arc::new(FakeSource::default());
        source.respond(snapshot("OPEN", &[("build", "fail")], &["1"]));

        let tracker = cx.new(|cx| PrTracker::new(source.clone(), Some(store.clone()), cx));
        let notifications = collect_notifications(&tracker, cx);
        set_tasks(&tracker, &[entry("a", Vec::new())], cx);
        advance(cx, TICK);
        assert_eq!(notifications.borrow().len(), 1);
        assert!(
            tracker
                .read_with(cx, |tracker, _| tracker.summary(&key("a")))
                .is_some_and(|summary| summary.has_unsent())
        );
        let (pull_request, feedback) = tracker
            .read_with(cx, |tracker, _| tracker.unsent_feedback(&key("a")))
            .unwrap();
        assert_eq!(pull_request.number, 42);
        assert_eq!(feedback.failed_checks.len(), 1);
        assert_eq!(feedback.comments.len(), 1);
        tracker.update(cx, |tracker, cx| {
            tracker.mark_sent(&key("a"), &feedback, cx)
        });
        cx.run_until_parked();
        drop(tracker);

        // A new Solo: the same pull request has nothing new, and no
        // notification.
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), Some(store.clone()), cx));
        let notifications = collect_notifications(&tracker, cx);
        set_tasks(&tracker, &[entry("a", Vec::new())], cx);
        advance(cx, TICK);
        assert_eq!(source.calls(), 2);
        assert!(notifications.borrow().is_empty());
        assert!(
            tracker
                .read_with(cx, |tracker, _| tracker.unsent_feedback(&key("a")))
                .is_none()
        );
        assert_eq!(
            summary_line(&tracker, "a", true, cx).as_deref(),
            Some("#42 · 检查失败 1 项")
        );

        // A new comment is new, the sent failure is not.
        source.respond(snapshot("OPEN", &[("build", "fail")], &["1", "2"]));
        advance(cx, QUIET_INTERVAL);
        assert_eq!(notifications.borrow().len(), 1);
        let (_, feedback) = tracker
            .read_with(cx, |tracker, _| tracker.unsent_feedback(&key("a")))
            .unwrap();
        assert!(feedback.failed_checks.is_empty());
        assert_eq!(feedback.comments[0].id, "2");
    }

    async fn agent_terminal(
        cx: &mut TestAppContext,
        status: crate::agent_status::AgentStatus,
    ) -> (Entity<terminal::Terminal>, Entity<TerminalView>) {
        let (project, _workspace, window) = crate::tests::init_test_with_window(cx).await;
        let (_pane, terminal, view) =
            crate::tests::add_display_only_terminal(&project, window, false, false, cx);
        view.update(cx, |view, _| {
            view.set_agent_status_for_test("claude", status)
        });
        // Multi-line pastes need bracketed paste, as in a real agent.
        terminal.update(cx, |terminal, cx| {
            terminal.write_raw_output(b"\x1b[?2004h", cx)
        });
        (terminal, view)
    }

    fn pasted(terminal: &Entity<terminal::Terminal>, cx: &mut TestAppContext) -> String {
        terminal
            .update(cx, |terminal, _| terminal.take_input_log())
            .into_iter()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .collect()
    }

    #[gpui::test]
    async fn test_auto_send_is_off_by_default_and_waits_for_an_idle_agent(cx: &mut TestAppContext) {
        use crate::agent_status::AgentStatus;
        let (terminal, view) = agent_terminal(cx, AgentStatus::Idle).await;
        pasted(&terminal, cx);
        let source = Arc::new(FakeSource::default());
        source.respond(snapshot("OPEN", &[("build", "fail")], &[]));
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        set_tasks(&tracker, &[entry("a", vec![view.clone()])], cx);

        // Off by default: nothing is pasted.
        advance(cx, TICK);
        assert_eq!(pasted(&terminal, cx), "");

        update_terminal_settings(cx, |terminal| terminal.auto_send_pr_feedback = Some(true));
        // A working agent isn't interrupted.
        view.update(cx, |view, _| {
            view.set_agent_status_for_test("claude", AgentStatus::Working)
        });
        source.respond(snapshot("OPEN", &[("build", "fail")], &["1"]));
        advance(cx, QUIET_INTERVAL);
        assert_eq!(pasted(&terminal, cx), "");

        // An idle one gets what's new, without Enter, and only once.
        view.update(cx, |view, _| {
            view.set_agent_status_for_test("claude", AgentStatus::Idle)
        });
        source.respond(snapshot("OPEN", &[("build", "fail")], &["1", "2"]));
        advance(cx, QUIET_INTERVAL);
        let input = pasted(&terminal, cx);
        assert!(
            input.starts_with("\x1b[200~Feedback on pull request #42"),
            "{input:?}"
        );
        assert!(
            input.contains("comment 1") && input.contains("comment 2"),
            "{input:?}"
        );
        assert!(input.contains("- build"), "{input:?}");
        assert!(input.ends_with("\x1b[201~"), "no Enter: {input:?}");
        advance(cx, QUIET_INTERVAL * 2);
        assert_eq!(pasted(&terminal, cx), "");
        assert!(
            tracker
                .read_with(cx, |tracker, _| tracker.unsent_feedback(&key("a")))
                .is_none()
        );
    }

    #[gpui::test]
    async fn test_sending_by_hand_pastes_only_what_is_new(cx: &mut TestAppContext) {
        use crate::agent_status::AgentStatus;
        let (terminal, view) = agent_terminal(cx, AgentStatus::Working).await;
        pasted(&terminal, cx);
        let source = Arc::new(FakeSource::default());
        source.respond(snapshot("OPEN", &[("build", "fail")], &["1"]));
        let tracker = cx.new(|cx| PrTracker::new(source.clone(), None, cx));
        set_tasks(&tracker, &[entry("a", vec![view.clone()])], cx);
        advance(cx, TICK);

        // By hand, a busy agent gets it too.
        view.update(cx, |view, _| {
            view.set_agent_status_for_test("claude", AgentStatus::Working)
        });
        let outcome = tracker.update(cx, |tracker, cx| tracker.send_unsent(&key("a"), false, cx));
        assert!(matches!(outcome, SendOutcome::Sent(_)));
        assert_ne!(pasted(&terminal, cx), "");
        let outcome = tracker.update(cx, |tracker, cx| tracker.send_unsent(&key("a"), false, cx));
        assert!(matches!(outcome, SendOutcome::NothingNew));

        // Without an agent running, the message is handed back.
        source.respond(snapshot("OPEN", &[("build", "fail")], &["1", "2"]));
        advance(cx, QUIET_INTERVAL);
        view.update(cx, |view, _| {
            view.set_agent_status_for_test("claude", AgentStatus::Done)
        });
        let outcome = tracker.update(cx, |tracker, cx| tracker.send_unsent(&key("a"), false, cx));
        match outcome {
            SendOutcome::NoAgent(message) => {
                assert!(message.contains("comment 2") && !message.contains("comment 1"))
            }
            _ => panic!("expected the message back"),
        }
    }
}
