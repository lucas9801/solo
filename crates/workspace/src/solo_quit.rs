//! SOLO: how Solo goes when it quits. Agents and commands running in
//! terminals outlive Solo restarting by itself (to install an update, say)
//! and crashing; quitting by the user (Quit, closing the last window) asks
//! first, as closing a window does, and ends them, unless
//! `terminal.keep_running_on_quit` says otherwise (the terminal views decide,
//! see `Item::keeps_running_after_quit`).

use gpui::{App, Global, SharedString, Task};

use crate::item::RunningProgram;

#[derive(Default, Clone, Copy)]
struct SoloQuit {
    /// Solo is quitting to start again at once (an update, a restart).
    restarting: bool,
    /// The user quit Solo, and confirmed ending what runs.
    user_quitting: bool,
}

impl Global for SoloQuit {}

/// Whether Solo is quitting to start again at once.
pub fn is_restarting(cx: &App) -> bool {
    cx.try_global::<SoloQuit>()
        .is_some_and(|quit| quit.restarting)
}

/// Whether the user quit Solo (all windows prepared to close): what keeps
/// running only for an update or a crash ends now.
pub fn is_user_quitting(cx: &App) -> bool {
    cx.try_global::<SoloQuit>()
        .is_some_and(|quit| quit.user_quitting)
}

/// Calls `f` whenever how Solo quits changes.
pub fn observe_quitting<T: 'static>(
    cx: &mut gpui::Context<T>,
    mut f: impl FnMut(&mut T, &mut gpui::Context<T>) + 'static,
) -> gpui::Subscription {
    cx.observe_global::<SoloQuit>(move |this, cx| f(this, cx))
}

pub(crate) fn set_restarting(restarting: bool, cx: &mut App) {
    cx.default_global::<SoloQuit>().restarting = restarting;
}

/// Solo was prepared to restart (see `prepare_windows_to_restart`), but
/// doesn't (the installer couldn't start, the Store's dialog was
/// cancelled): quitting later is the user's again, which asks and ends.
pub fn restart_cancelled(cx: &mut App) {
    if is_restarting(cx) {
        log::info!("Solo isn't restarting after all");
        set_restarting(false, cx);
    }
}

/// The user quits Solo, all windows prepared to close: unless it's
/// restarting, ends what was to keep running only for an update or a crash.
pub(crate) fn quitting_by_user(cx: &mut App) {
    if is_restarting(cx) {
        return;
    }
    log::info!("Solo quits: ending what runs in its terminals");
    cx.default_global::<SoloQuit>().user_quitting = true;
    if let Some(programs) = cx.try_global::<BackgroundPrograms>() {
        (programs.end)(cx);
    }
}

/// A program running for Solo outside its windows' items: a terminal an
/// earlier Solo left running, whose tab isn't open yet.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundProgram {
    /// The tab it's in.
    pub tab: SharedString,
    pub program: RunningProgram,
}

/// What lists (as the quit prompt shows them, in the background: it reads
/// files and the processes), and ends, the [`BackgroundProgram`]s quitting
/// ends: none when quitting leaves them running.
pub struct BackgroundPrograms {
    pub list: fn(&App) -> Task<Vec<BackgroundProgram>>,
    pub end: fn(&mut App),
}

impl Global for BackgroundPrograms {}

pub(crate) fn background_programs(cx: &App) -> Task<Vec<BackgroundProgram>> {
    match cx.try_global::<BackgroundPrograms>() {
        Some(programs) => (programs.list)(cx),
        None => Task::ready(Vec::new()),
    }
}
