//! SOLO: desktop notifications from the terminal while Solo is in the
//! background: when a long command finishes (a build, a test run), when a
//! program asks for one with OSC 9 / OSC 777, when an agent needs you, and
//! (if turned on) when an agent stops working. Each kind has its own setting.
//! Clicking a notification brings back the terminal it came from.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, Context, Global, SharedString, SystemNotification, WeakEntity, Window,
};
use settings::Settings;
use terminal::terminal_settings::TerminalSettings;

use crate::TerminalView;

/// Which terminal each shown notification came from, by tag.
#[derive(Default)]
struct NotificationTargets(HashMap<SharedString, (AnyWindowHandle, WeakEntity<TerminalView>)>);

impl Global for NotificationTargets {}

pub(crate) fn init(cx: &mut App) {
    cx.on_system_notification_response(|response, cx| {
        let Some((window, view)) = cx
            .default_global::<NotificationTargets>()
            .0
            .remove(&response.tag)
        else {
            return;
        };
        window
            .update(cx, |_, window, cx| {
                window.activate_window();
                if let Some(view) = view.upgrade() {
                    crate::workspace_terminals::show_terminal(&view, window, cx);
                }
            })
            .ok();
    });
}

impl TerminalView {
    /// Notifies about the command that just finished, if it ran long enough
    /// and Solo is in the background.
    pub(crate) fn notify_command_finished(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let threshold = TerminalSettings::get_global(cx).notify_long_commands_after_seconds;
        if threshold == 0 || window.is_window_active() {
            return;
        }
        let terminal = self.terminal.read(cx);
        let Some(block) = terminal.shell_integration().blocks().last() else {
            return;
        };
        let (Some(started_at), Some(finished_at)) = (block.started_at, block.finished_at) else {
            return;
        };
        let duration = finished_at - started_at;
        if duration < Duration::from_secs(threshold) {
            return;
        }
        log::debug!("notifying about a command that ran {duration:?}");
        let command = block
            .command
            .clone()
            .unwrap_or_else(|| util::i18n::tr("Command", "命令").to_string());
        let (title, body) =
            finished_notification_text(&command, block.exit_code, duration, &terminal.title(true));
        self.show_notification(title, body, window, cx);
    }

    /// Shows a notification a program asked for, while Solo is in the background.
    pub(crate) fn notify_from_program(
        &mut self,
        title: Option<String>,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !TerminalSettings::get_global(cx).program_notifications || window.is_window_active() {
            return;
        }
        let title = title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| self.terminal.read(cx).title(true));
        self.show_notification(title, body, window, cx);
    }

    /// AI agents ring the bell when they need the user (Claude Code's default
    /// notification channel outside iTerm2); turn that into a notification.
    /// Other programs' bells stay bells.
    pub(crate) fn notify_agent_bell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !TerminalSettings::get_global(cx).agent_input_notifications || window.is_window_active()
        {
            return;
        }
        let extra = &TerminalSettings::get_global(cx).agent_commands;
        let terminal = self.terminal.read(cx);
        let Some(command) = terminal
            .shell_integration()
            .blocks()
            .last()
            .filter(|block| block.is_running())
            .and_then(|block| block.command.clone())
            .filter(|command| crate::command_blocks::is_agent_command(command, extra))
        else {
            return;
        };
        let title = command.lines().next().unwrap_or_default().to_string();
        let body = format!(
            "{} · {}",
            util::i18n::tr("Waiting for you", "等待你的操作"),
            terminal.title(true)
        );
        self.show_notification(title, body, window, cx);
    }

    /// An agent reported that it needs the user (a permission or a question),
    /// while Solo's window is in the background.
    pub(crate) fn notify_agent_needs_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !TerminalSettings::get_global(cx).agent_input_notifications || window.is_window_active()
        {
            return;
        }
        let Some((name, _)) = self.agent_status() else {
            return;
        };
        let title = if util::i18n::is_chinese() {
            format!("{name} 需要你的输入")
        } else {
            format!("{name} needs your input")
        };
        let body = self.terminal.read(cx).title(true);
        self.show_notification(title, body, window, cx);
    }

    /// An agent went quiet after working a while in the background: it's
    /// probably done with its turn and waiting for the user.
    pub(crate) fn notify_agent_stopped(
        &mut self,
        worked_for: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !TerminalSettings::get_global(cx).agent_stopped_notifications
            || window.is_window_active()
            || worked_for < crate::agent_status::NOTIFY_AFTER_WORKING
        {
            return;
        }
        let Some((name, _)) = self.agent_status() else {
            return;
        };
        let title = if util::i18n::is_chinese() {
            format!("{name} 已停止工作")
        } else {
            format!("{name} stopped working")
        };
        let body = format!(
            "{} · {}",
            util::i18n::tr("Waiting for you", "等待你的操作"),
            self.terminal.read(cx).title(true)
        );
        self.show_notification(title, body, window, cx);
    }

    fn show_notification(
        &mut self,
        title: String,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One notification per terminal; a newer one replaces the last.
        let tag: SharedString = format!("solo-terminal-{}", cx.entity_id()).into();
        let target = (window.window_handle(), cx.entity().downgrade());
        cx.default_global::<NotificationTargets>()
            .0
            .insert(tag.clone(), target);
        cx.show_system_notification(SystemNotification {
            tag,
            title: title.into(),
            body: body.into(),
            actions: Vec::new(),
        });
    }
}

fn finished_notification_text(
    command: &str,
    exit_code: Option<i32>,
    duration: Duration,
    terminal_title: &str,
) -> (String, String) {
    let command = command.lines().next().unwrap_or_default();
    let title = match exit_code {
        Some(code) if code != 0 && util::i18n::is_chinese() => {
            format!("✗ {command}（退出码 {code}）")
        }
        Some(code) if code != 0 => format!("✗ {command} (exit {code})"),
        _ => format!("✓ {command}"),
    };
    let secs = duration.as_secs();
    let duration = if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    };
    let body = if util::i18n::is_chinese() {
        format!("用时 {duration} · {terminal_title}")
    } else {
        format!("Finished in {duration} · {terminal_title}")
    };
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finished_notification_text() {
        assert_eq!(
            finished_notification_text("cargo build", Some(0), Duration::from_secs(72), "solo"),
            (
                "✓ cargo build".to_string(),
                "Finished in 1m 12s · solo".to_string()
            )
        );
        assert_eq!(
            finished_notification_text("npm test\nmore", Some(1), Duration::from_secs(12), "x"),
            (
                "✗ npm test (exit 1)".to_string(),
                "Finished in 12s · x".to_string()
            )
        );
    }

    async fn background_terminal(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::Entity<terminal::Terminal>,
        gpui::Entity<TerminalView>,
        &mut gpui::VisualTestContext,
    ) {
        use gpui::AppContext as _;
        let (project, workspace) = crate::tests::init_test(cx).await;
        let terminal = cx.new(|cx| {
            terminal::TerminalBuilder::new_display_only(
                Default::default(),
                settings::AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                util::paths::PathStyle::local(),
            )
            .subscribe(cx)
        });
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
        cx.update(|_, cx| cx.set_app_identity("Solo-Terminal-Test", "Solo"));
        cx.deactivate_window();
        cx.run_until_parked();
        (terminal, view, cx)
    }

    fn update_terminal_settings(
        cx: &mut gpui::VisualTestContext,
        update: impl FnOnce(&mut settings::TerminalSettingsContent),
    ) {
        use gpui::UpdateGlobal as _;
        cx.update(|_, cx| {
            settings::SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    update(settings.terminal.get_or_insert_default())
                });
            })
        });
    }

    fn agent_stopped(view: &gpui::Entity<TerminalView>, cx: &mut gpui::VisualTestContext) {
        view.update_in(cx, |view, window, cx| {
            view.set_agent_status_for_test("claude", crate::agent_status::AgentStatus::Finished);
            view.notify_agent_stopped(Duration::from_secs(60), window, cx);
        });
    }

    fn agent_needs_input(view: &gpui::Entity<TerminalView>, cx: &mut gpui::VisualTestContext) {
        view.update_in(cx, |view, window, cx| {
            view.set_agent_status_for_test("claude", crate::agent_status::AgentStatus::NeedsInput);
            view.notify_agent_needs_input(window, cx);
        });
    }

    fn shown_titles(cx: &mut gpui::VisualTestContext) -> Vec<String> {
        cx.shown_system_notifications()
            .into_iter()
            .map(|notification| notification.title.to_string())
            .collect()
    }

    #[gpui::test]
    async fn test_agent_stopped_notification_is_off_by_default(cx: &mut gpui::TestAppContext) {
        let (_terminal, view, cx) = background_terminal(cx).await;

        agent_stopped(&view, cx);
        assert!(shown_titles(cx).is_empty());

        update_terminal_settings(cx, |terminal| {
            terminal.agent_stopped_notifications = Some(true)
        });
        agent_stopped(&view, cx);
        assert_eq!(shown_titles(cx), ["claude stopped working"]);
    }

    #[gpui::test]
    async fn test_each_notification_kind_has_its_own_setting(cx: &mut gpui::TestAppContext) {
        let (terminal, view, cx) = background_terminal(cx).await;
        let program_notification = |cx: &mut gpui::VisualTestContext| {
            terminal.update(cx, |terminal, cx| {
                terminal.write_raw_output(b"\x1b]777;notify;Build;Done\x07", cx)
            });
            cx.run_until_parked();
        };

        // On by default, with "stopped working" off.
        agent_needs_input(&view, cx);
        program_notification(cx);
        assert_eq!(shown_titles(cx), ["claude needs your input", "Build"]);

        // Turning off programs' notifications leaves agents' alone.
        update_terminal_settings(cx, |terminal| terminal.program_notifications = Some(false));
        agent_needs_input(&view, cx);
        program_notification(cx);
        assert_eq!(
            shown_titles(cx),
            [
                "claude needs your input",
                "Build",
                "claude needs your input"
            ]
        );

        // And the other way around; turning on "stopped working" doesn't
        // bring either back.
        update_terminal_settings(cx, |terminal| {
            terminal.program_notifications = Some(true);
            terminal.agent_input_notifications = Some(false);
            terminal.agent_stopped_notifications = Some(true);
        });
        agent_needs_input(&view, cx);
        program_notification(cx);
        assert_eq!(
            shown_titles(cx),
            [
                "claude needs your input",
                "Build",
                "claude needs your input",
                "Build"
            ]
        );
    }
}
