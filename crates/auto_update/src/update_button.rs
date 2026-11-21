//! SOLO: the update button at the right end of the title bar.
//!
//! Solo's own updater ([`crate::solo_update`]) and the Microsoft Store
//! edition's ([`crate::store_update`]) tell the user about updates only
//! through this: an [`UpdateEntry`] they keep up to date, which every
//! window's title bar shows as a button (hidden while there's nothing to
//! say) with a menu of what can be done. What the button and its menu say is
//! decided here, by plain functions, so it can be tested without a window.

use gpui::{App, AppContext as _, Context, Entity, Global, Task, Window};
use semver::Version;
use std::{rc::Rc, time::Duration};

/// How long "up to date" stays after a manual check found nothing newer.
pub const UP_TO_DATE_DURATION: Duration = Duration::from_secs(4);

/// What the user can do from the button's menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    /// Install now: Solo quits (or is closed by the Store) and starts again.
    Restart,
    /// Install when the user quits Solo.
    OnQuit,
    /// The release notes; for an update Solo can't install itself, the
    /// Microsoft Store page, where it's got instead.
    ViewNotes,
    Skip,
    /// Try again after a failure.
    Retry,
}

/// What the button shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ButtonState {
    /// Nothing to say: no button.
    Hidden,
    /// A manual check is running.
    Checking,
    /// A manual check found nothing newer; shown for [`UP_TO_DATE_DURATION`].
    UpToDate,
    /// A newer version ([`UpdateEntry::offer`]) can be installed.
    Available,
    Downloading {
        percent: Option<u8>,
    },
    /// Solo is closing (or being closed) to install the update.
    Installing,
    /// The update is installed when the user quits Solo. No button: the
    /// user chose already; a manual check shows it again as
    /// [`ButtonState::Available`].
    OnQuit,
    /// A check or an update failed, for this reason.
    Failed {
        reason: String,
    },
}

/// The update the button is about.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateOffer {
    /// `None` when the Store doesn't say which version its update is.
    pub version: Option<Version>,
    /// The release notes (markdown), for a one-sentence summary.
    pub notes: Option<String>,
    /// Where "view release notes" goes.
    pub notes_url: Option<String>,
    /// Whether Solo can install it itself (otherwise only the release page
    /// helps).
    pub can_install: bool,
    /// Whether "Install When Solo Quits" can work: not for a Store update the
    /// Store won't install without asking.
    pub can_install_on_quit: bool,
}

/// The icon the button draws before its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonIcon {
    Download,
    Warning,
    /// A spinning circle: a check runs.
    Spinner,
    Check,
}

/// What the button shows: its text, icon, and whether it's a filled pill
/// (tinted with the accent color, or the warning color for a failure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonLabel {
    pub text: String,
    pub warning: bool,
    pub icon: ButtonIcon,
    /// Drawn on a tinted pill; "checking" and "up to date" only say
    /// something, without a background.
    pub filled: bool,
    /// How far a download got, drawn as a thin bar along the pill's bottom.
    pub progress: Option<u8>,
}

/// One row of the button's menu after its two lines of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Action(UpdateAction, String),
    Separator,
}

/// The button's menu: a title ("Solo 2.5.0"), a line under it (the release
/// notes' summary, or a warning in yellow), then the rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMenu {
    pub title: String,
    pub detail: Option<String>,
    pub detail_is_warning: bool,
    pub items: Vec<MenuItem>,
}

fn text(chinese: bool, english: &str, zh: &str) -> String {
    if chinese { zh } else { english }.to_string()
}

fn version_suffix(offer: Option<&UpdateOffer>) -> String {
    offer
        .and_then(|offer| offer.version.as_ref())
        .map(|version| format!(" {version}"))
        .unwrap_or_default()
}

/// The release notes' one sentence for the menu, in the UI's language,
/// without a leading "Solo 2.5.3：" that the menu's title already says;
/// SOLO: in English, capitalized then.
pub fn offer_summary(offer: &UpdateOffer, chinese: bool) -> Option<String> {
    let summary = crate::solo_update::notes_summary(offer.notes.as_deref()?, chinese)?;
    let rest = without_version_prefix(&summary);
    if rest.is_empty() {
        return None;
    }
    // SOLO: the English rest of "Solo 2.5.4: what an agent changed" starts
    // a sentence of its own in the menu.
    if !chinese && rest.len() != summary.len() {
        let mut characters = rest.chars();
        if let Some(first) = characters.next() {
            return Some(first.to_uppercase().chain(characters).collect());
        }
    }
    Some(rest.to_string())
}

/// `summary` without a leading "Solo <version>：" or "Solo <version>: ".
fn without_version_prefix(summary: &str) -> &str {
    let Some(rest) = summary.strip_prefix("Solo ") else {
        return summary;
    };
    let rest = rest.strip_prefix(['v', 'V']).unwrap_or(rest);
    let version_end = rest
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(rest.len());
    let version = &rest[..version_end];
    if !version.starts_with(|character: char| character.is_ascii_digit()) {
        return summary;
    }
    let rest = &rest[version_end..];
    match rest.strip_prefix('：').or_else(|| rest.strip_prefix(':')) {
        Some(rest) => rest.trim_start(),
        None => summary,
    }
}

/// What the button shows in `state`, or `None` when it's hidden.
///
/// An update waiting for Solo to quit hides the button: the user already
/// chose what happens. A manual "check for updates" shows it again (as
/// [`ButtonState::Available`]).
pub fn button_label(
    state: &ButtonState,
    offer: Option<&UpdateOffer>,
    chinese: bool,
) -> Option<ButtonLabel> {
    let version = version_suffix(offer);
    let label = |text: String, icon: ButtonIcon, filled: bool| {
        Some(ButtonLabel {
            text,
            warning: false,
            icon,
            filled,
            progress: None,
        })
    };
    match state {
        ButtonState::Hidden | ButtonState::OnQuit => None,
        ButtonState::Checking => label(
            text(chinese, "Checking…", "正在检查…"),
            ButtonIcon::Spinner,
            false,
        ),
        ButtonState::UpToDate => label(
            text(chinese, "Up to date", "已是最新"),
            ButtonIcon::Check,
            false,
        ),
        ButtonState::Available => label(
            if chinese {
                format!("更新到{version}")
            } else {
                format!("Update to{version}")
            },
            ButtonIcon::Download,
            true,
        ),
        ButtonState::Downloading { percent } => {
            let suffix = percent.map(|p| format!(" {p}%")).unwrap_or_default();
            Some(ButtonLabel {
                text: if chinese {
                    format!("下载中{suffix}")
                } else {
                    format!("Downloading{suffix}")
                },
                warning: false,
                icon: ButtonIcon::Download,
                filled: true,
                progress: *percent,
            })
        }
        ButtonState::Installing => label(
            text(chinese, "Installing…", "正在安装…"),
            ButtonIcon::Download,
            true,
        ),
        ButtonState::Failed { .. } => Some(ButtonLabel {
            text: text(chinese, "Update failed", "更新失败"),
            warning: true,
            icon: ButtonIcon::Warning,
            filled: true,
            progress: None,
        }),
    }
}

/// The button's menu in `state`, or `None` when clicking it does nothing.
/// `running` names the programs (agents) that installing now would end;
/// `summary` is the release notes' one sentence.
pub fn update_menu(
    state: &ButtonState,
    offer: Option<&UpdateOffer>,
    summary: Option<String>,
    running: &[String],
    chinese: bool,
) -> Option<UpdateMenu> {
    let title = format!("Solo{}", version_suffix(offer));
    let restart = MenuItem::Action(
        UpdateAction::Restart,
        text(chinese, "Update and Restart", "更新并重启"),
    );
    let on_quit = MenuItem::Action(
        UpdateAction::OnQuit,
        text(chinese, "Install When Solo Quits", "退出时安装"),
    );
    let notes = offer
        .filter(|offer| offer.notes_url.is_some())
        .map(|offer| {
            let label = if offer.can_install {
                text(chinese, "View Release Notes", "查看更新内容")
            } else {
                text(
                    chinese,
                    "Get It from the Microsoft Store",
                    "从 Microsoft Store 获取",
                )
            };
            MenuItem::Action(UpdateAction::ViewNotes, label)
        });
    let skip = offer.filter(|offer| offer.version.is_some()).map(|_| {
        MenuItem::Action(
            UpdateAction::Skip,
            text(chinese, "Skip This Version", "跳过此版本"),
        )
    });
    let tail = |items: &mut Vec<MenuItem>| {
        let rest: Vec<MenuItem> = notes.clone().into_iter().chain(skip.clone()).collect();
        if !rest.is_empty() {
            if !items.is_empty() {
                items.push(MenuItem::Separator);
            }
            items.extend(rest);
        }
    };
    let running_warning = (!running.is_empty()).then(|| {
        if chinese {
            format!("现在更新会结束：{}", running.join("、"))
        } else {
            format!("Updating now ends: {}", running.join(", "))
        }
    });

    let mut items = Vec::new();
    let (detail, detail_is_warning) = match state {
        ButtonState::Hidden | ButtonState::Checking | ButtonState::UpToDate => return None,
        ButtonState::Available => {
            let offer = offer?;
            if offer.can_install {
                let on_quit = offer.can_install_on_quit.then_some(on_quit);
                if running.is_empty() {
                    items.push(restart);
                    items.extend(on_quit);
                } else {
                    // Updating now would end the agents: installing when the
                    // user quits comes first.
                    items.extend(on_quit);
                    items.push(restart);
                }
            }
            tail(&mut items);
            match running_warning.filter(|_| offer.can_install) {
                Some(warning) => (Some(warning), true),
                None => (summary, false),
            }
        }
        ButtonState::Downloading { .. } | ButtonState::Installing => {
            tail(&mut items);
            (summary, false)
        }
        ButtonState::OnQuit => {
            items.push(restart);
            tail(&mut items);
            match running_warning {
                Some(warning) => (Some(warning), true),
                None => (summary, false),
            }
        }
        ButtonState::Failed { reason } => {
            items.push(MenuItem::Action(
                UpdateAction::Retry,
                text(chinese, "Retry", "重试"),
            ));
            tail(&mut items);
            (Some(reason.clone()), true)
        }
    };
    Some(UpdateMenu {
        title,
        detail,
        detail_is_warning,
        items,
    })
}

type ActionHandler = Rc<dyn Fn(UpdateAction, &mut Window, &mut App)>;

/// The state the button shows, shared by every window.
pub struct UpdateEntry {
    state: ButtonState,
    offer: Option<UpdateOffer>,
    handler: Option<ActionHandler>,
    up_to_date_timer: Option<Task<()>>,
}

struct GlobalUpdateEntry(Entity<UpdateEntry>);

impl Global for GlobalUpdateEntry {}

/// The update button's state, if an updater runs (or it was simulated).
pub fn entry(cx: &App) -> Option<Entity<UpdateEntry>> {
    Some(cx.try_global::<GlobalUpdateEntry>()?.0.clone())
}

/// The update button's state, made if no updater made it yet.
pub fn entry_or_init(cx: &mut App) -> Entity<UpdateEntry> {
    if let Some(entry) = entry(cx) {
        return entry;
    }
    let entry = cx.new(|_| UpdateEntry {
        state: ButtonState::Hidden,
        offer: None,
        handler: None,
        up_to_date_timer: None,
    });
    cx.set_global(GlobalUpdateEntry(entry.clone()));
    entry
}

impl UpdateEntry {
    pub fn state(&self) -> &ButtonState {
        &self.state
    }

    pub fn offer(&self) -> Option<&UpdateOffer> {
        self.offer.as_ref()
    }

    /// The updater that carries out what's picked in the menu.
    pub fn set_handler(&mut self, handler: impl Fn(UpdateAction, &mut Window, &mut App) + 'static) {
        self.handler = Some(Rc::new(handler));
    }

    pub fn set_state(&mut self, state: ButtonState, cx: &mut Context<Self>) {
        self.up_to_date_timer = None;
        if self.state != state {
            self.state = state;
            cx.notify();
        }
    }

    pub fn set_offer(&mut self, offer: Option<UpdateOffer>, cx: &mut Context<Self>) {
        if self.offer != offer {
            self.offer = offer;
            cx.notify();
        }
    }

    /// Says "up to date" for a few seconds, then hides the button, unless
    /// something else is shown meanwhile.
    pub fn show_up_to_date(&mut self, cx: &mut Context<Self>) {
        self.set_state(ButtonState::UpToDate, cx);
        self.up_to_date_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UP_TO_DATE_DURATION).await;
            this.update(cx, |this, cx| {
                if this.state == ButtonState::UpToDate {
                    this.state = ButtonState::Hidden;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Steps through the button's states with a made-up update (the
    /// "simulate update available" debug action).
    pub fn simulate_next(&mut self, cx: &mut Context<Self>) {
        let next = match &self.state {
            ButtonState::Hidden | ButtonState::Checking | ButtonState::UpToDate => {
                ButtonState::Available
            }
            ButtonState::Available => ButtonState::Downloading { percent: Some(45) },
            ButtonState::Downloading { .. } | ButtonState::Installing => ButtonState::OnQuit,
            ButtonState::OnQuit => ButtonState::Failed {
                reason: "Simulated: couldn't reach GitHub".into(),
            },
            ButtonState::Failed { .. } => {
                self.show_up_to_date(cx);
                return;
            }
        };
        if self.offer.is_none() {
            self.set_offer(
                Some(UpdateOffer {
                    version: Some(Version::new(9, 9, 0)),
                    notes: Some("Solo 9.9：示例更新。\nSolo 9.9: a sample update.".into()),
                    notes_url: Some("https://github.com/lucas9801/solo/releases".into()),
                    can_install: true,
                    can_install_on_quit: true,
                }),
                cx,
            );
        }
        self.set_state(next, cx);
    }
}

/// Carries out what the user picked in the button's menu.
pub fn perform(
    entry: &Entity<UpdateEntry>,
    action: UpdateAction,
    window: &mut Window,
    cx: &mut App,
) {
    match entry.read(cx).handler.clone() {
        Some(handler) => handler(action, window, cx),
        None => log::info!("update button: {action:?} picked, but no updater runs"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn offer(version: &str) -> UpdateOffer {
        UpdateOffer {
            version: Some(Version::parse(version).unwrap()),
            notes: None,
            notes_url: Some("https://example.com/notes".into()),
            can_install: true,
            can_install_on_quit: true,
        }
    }

    fn label(state: ButtonState, chinese: bool) -> Option<String> {
        button_label(&state, Some(&offer("2.5.0")), chinese).map(|label| label.text)
    }

    #[test]
    fn button_text_in_each_state() {
        assert_eq!(label(ButtonState::Hidden, true), None);
        assert_eq!(
            label(ButtonState::Available, true).as_deref(),
            Some("更新到 2.5.0")
        );
        assert_eq!(
            label(ButtonState::Available, false).as_deref(),
            Some("Update to 2.5.0")
        );
        assert_eq!(
            label(ButtonState::Downloading { percent: Some(45) }, true).as_deref(),
            Some("下载中 45%")
        );
        assert_eq!(
            label(ButtonState::Downloading { percent: None }, false).as_deref(),
            Some("Downloading")
        );
        // Chosen to install when Solo quits: the button goes.
        assert_eq!(label(ButtonState::OnQuit, true), None);
        assert_eq!(label(ButtonState::OnQuit, false), None);
        assert_eq!(
            label(ButtonState::Installing, true).as_deref(),
            Some("正在安装…")
        );
        assert_eq!(
            label(ButtonState::Checking, false).as_deref(),
            Some("Checking…")
        );
        assert_eq!(
            label(ButtonState::UpToDate, true).as_deref(),
            Some("已是最新")
        );
        let failed = button_label(
            &ButtonState::Failed {
                reason: "offline".into(),
            },
            None,
            true,
        )
        .unwrap();
        assert_eq!(failed.text, "更新失败");
        assert!(failed.warning);
        assert!(
            !button_label(&ButtonState::Available, None, true)
                .unwrap()
                .warning
        );
        // The Store didn't say which version: no version in the text.
        let unknown = UpdateOffer {
            version: None,
            ..offer("2.5.0")
        };
        assert_eq!(
            button_label(&ButtonState::Available, Some(&unknown), true)
                .unwrap()
                .text,
            "更新到"
        );
        // No arrow characters left: the icon is drawn.
        for state in [
            ButtonState::Checking,
            ButtonState::UpToDate,
            ButtonState::Available,
            ButtonState::Downloading { percent: Some(1) },
            ButtonState::Installing,
            ButtonState::Failed {
                reason: "x".into(),
            },
        ] {
            for chinese in [true, false] {
                let text = label(state.clone(), chinese).unwrap();
                assert!(!text.contains(['↑', '↓']), "{text}");
            }
        }
    }

    #[test]
    fn button_icon_fill_and_progress_in_each_state() {
        let look = |state: ButtonState| {
            let label = button_label(&state, Some(&offer("2.5.0")), true).unwrap();
            (label.icon, label.filled, label.warning, label.progress)
        };
        assert_eq!(
            look(ButtonState::Available),
            (ButtonIcon::Download, true, false, None)
        );
        assert_eq!(
            look(ButtonState::Downloading { percent: Some(45) }),
            (ButtonIcon::Download, true, false, Some(45))
        );
        assert_eq!(
            look(ButtonState::Downloading { percent: None }),
            (ButtonIcon::Download, true, false, None)
        );
        assert_eq!(
            look(ButtonState::Failed {
                reason: "offline".into()
            }),
            (ButtonIcon::Warning, true, true, None)
        );
        assert_eq!(
            look(ButtonState::Checking),
            (ButtonIcon::Spinner, false, false, None)
        );
        assert_eq!(
            look(ButtonState::UpToDate),
            (ButtonIcon::Check, false, false, None)
        );
    }

    #[test]
    fn summary_drops_the_version_the_title_says() {
        let with_notes = |notes: &str| UpdateOffer {
            notes: Some(notes.into()),
            ..offer("2.5.3")
        };
        let notes = "Solo 2.5.3：终端大量输出时 CPU 大幅下降。
Solo 2.5.3: Much less CPU when terminals print a lot.";
        assert_eq!(
            offer_summary(&with_notes(notes), true).as_deref(),
            Some("终端大量输出时 CPU 大幅下降。")
        );
        assert_eq!(
            offer_summary(&with_notes(notes), false).as_deref(),
            Some("Much less CPU when terminals print a lot.")
        );
        // Summaries that don't start with the version stay as they are.
        let notes = "终端重连时恢复画面。
Terminals keep their screen when reconnecting.";
        assert_eq!(
            offer_summary(&with_notes(notes), true).as_deref(),
            Some("终端重连时恢复画面。")
        );
        // The English rest starts with a capital letter, as a sentence does;
        // a summary without the version stays as it was written.
        let notes = "Solo 2.5.4：agent 改了什么，在审阅页里逐处保留或拒绝。
Solo 2.5.4: what an agent changed, to keep or reject on the review page.";
        assert_eq!(
            offer_summary(&with_notes(notes), false).as_deref(),
            Some("What an agent changed, to keep or reject on the review page.")
        );
        assert_eq!(
            offer_summary(&with_notes(notes), true).as_deref(),
            Some("agent 改了什么，在审阅页里逐处保留或拒绝。")
        );
        let notes = "终端更快。
terminals are faster.";
        assert_eq!(
            offer_summary(&with_notes(notes), false).as_deref(),
            Some("terminals are faster.")
        );
        assert_eq!(without_version_prefix("Solo 很快：更新"), "Solo 很快：更新");
        assert_eq!(without_version_prefix("Solo v2.5.3: Faster"), "Faster");
        assert_eq!(without_version_prefix("Solo 2.5.3 is faster"), "Solo 2.5.3 is faster");
        assert_eq!(without_version_prefix("Solo: faster"), "Solo: faster");
    }

    fn actions(menu: &UpdateMenu) -> Vec<Option<UpdateAction>> {
        menu.items
            .iter()
            .map(|item| match item {
                MenuItem::Action(action, _) => Some(*action),
                MenuItem::Separator => None,
            })
            .collect()
    }

    #[test]
    fn menu_offers_restart_first_without_agents() {
        let menu = update_menu(
            &ButtonState::Available,
            Some(&offer("2.5.0")),
            Some("终端重连时恢复画面。".into()),
            &[],
            true,
        )
        .unwrap();
        assert_eq!(menu.title, "Solo 2.5.0");
        assert_eq!(menu.detail.as_deref(), Some("终端重连时恢复画面。"));
        assert!(!menu.detail_is_warning);
        assert_eq!(
            actions(&menu),
            [
                Some(UpdateAction::Restart),
                Some(UpdateAction::OnQuit),
                None,
                Some(UpdateAction::ViewNotes),
                Some(UpdateAction::Skip),
            ]
        );
        let labels: Vec<_> = menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action(_, label) => Some(label.as_str()),
                MenuItem::Separator => None,
            })
            .collect();
        assert_eq!(
            labels,
            ["更新并重启", "退出时安装", "查看更新内容", "跳过此版本"]
        );
    }

    #[test]
    fn menu_without_installing_at_quit() {
        let offer = UpdateOffer {
            can_install_on_quit: false,
            ..offer("2.5.0")
        };
        for running in [vec![], vec!["claude".to_string()]] {
            let menu =
                update_menu(&ButtonState::Available, Some(&offer), None, &running, true).unwrap();
            assert!(!actions(&menu).contains(&Some(UpdateAction::OnQuit)));
            assert_eq!(actions(&menu)[0], Some(UpdateAction::Restart));
        }
    }

    #[test]
    fn menu_with_agents_running_installs_on_quit_first() {
        let running = ["claude".to_string(), "codex".to_string()];
        let menu = update_menu(
            &ButtonState::Available,
            Some(&offer("2.5.0")),
            Some("summary".into()),
            &running,
            true,
        )
        .unwrap();
        assert_eq!(
            menu.detail.as_deref(),
            Some("现在更新会结束：claude、codex")
        );
        assert!(menu.detail_is_warning);
        assert_eq!(
            actions(&menu)[..2],
            [Some(UpdateAction::OnQuit), Some(UpdateAction::Restart)]
        );
        let menu = update_menu(
            &ButtonState::Available,
            Some(&offer("2.5.0")),
            None,
            &running,
            false,
        )
        .unwrap();
        assert_eq!(
            menu.detail.as_deref(),
            Some("Updating now ends: claude, codex")
        );
    }

    #[test]
    fn menu_after_a_failure_says_why_and_retries() {
        let menu = update_menu(
            &ButtonState::Failed {
                reason: "couldn't reach GitHub".into(),
            },
            Some(&offer("2.5.0")),
            Some("summary".into()),
            &[],
            false,
        )
        .unwrap();
        assert_eq!(menu.detail.as_deref(), Some("couldn't reach GitHub"));
        assert!(menu.detail_is_warning);
        assert_eq!(actions(&menu)[0], Some(UpdateAction::Retry));
        // A failed check: no update to skip or read about.
        let menu = update_menu(
            &ButtonState::Failed {
                reason: "offline".into(),
            },
            None,
            None,
            &[],
            true,
        )
        .unwrap();
        assert_eq!(menu.title, "Solo");
        assert_eq!(actions(&menu), [Some(UpdateAction::Retry)]);
        assert_eq!(
            menu.items[0],
            MenuItem::Action(UpdateAction::Retry, "重试".into())
        );
    }

    #[test]
    fn menu_in_other_states() {
        for state in [
            ButtonState::Hidden,
            ButtonState::Checking,
            ButtonState::UpToDate,
        ] {
            assert_eq!(
                update_menu(&state, Some(&offer("2.5.0")), None, &[], true),
                None
            );
        }
        let menu =
            update_menu(&ButtonState::OnQuit, Some(&offer("2.5.0")), None, &[], true).unwrap();
        assert_eq!(
            actions(&menu),
            [
                Some(UpdateAction::Restart),
                None,
                Some(UpdateAction::ViewNotes),
                Some(UpdateAction::Skip),
            ]
        );
        let menu = update_menu(
            &ButtonState::Downloading { percent: Some(3) },
            Some(&offer("2.5.0")),
            None,
            &[],
            true,
        )
        .unwrap();
        assert_eq!(
            actions(&menu),
            [Some(UpdateAction::ViewNotes), Some(UpdateAction::Skip)]
        );
        // Solo can't install it itself: only the Microsoft Store page.
        let page_only = UpdateOffer {
            can_install: false,
            can_install_on_quit: true,
            ..offer("2.5.0")
        };
        let menu = update_menu(
            &ButtonState::Available,
            Some(&page_only),
            None,
            &["claude".into()],
            true,
        )
        .unwrap();
        assert_eq!(menu.detail, None);
        assert_eq!(
            actions(&menu),
            [Some(UpdateAction::ViewNotes), Some(UpdateAction::Skip)]
        );
        assert_eq!(
            menu.items[0],
            MenuItem::Action(UpdateAction::ViewNotes, "从 Microsoft Store 获取".into())
        );
        let menu =
            update_menu(&ButtonState::Available, Some(&page_only), None, &[], false).unwrap();
        assert_eq!(
            menu.items[0],
            MenuItem::Action(
                UpdateAction::ViewNotes,
                "Get It from the Microsoft Store".into()
            )
        );
    }

    #[gpui::test]
    async fn up_to_date_goes_away_after_a_few_seconds(cx: &mut TestAppContext) {
        let entry = cx.update(entry_or_init);
        entry.update(cx, |entry, cx| entry.show_up_to_date(cx));
        assert_eq!(
            entry.read_with(cx, |e, _| e.state().clone()),
            ButtonState::UpToDate
        );
        cx.executor().advance_clock(UP_TO_DATE_DURATION / 2);
        assert_eq!(
            entry.read_with(cx, |e, _| e.state().clone()),
            ButtonState::UpToDate
        );
        cx.executor().advance_clock(UP_TO_DATE_DURATION);
        assert_eq!(
            entry.read_with(cx, |e, _| e.state().clone()),
            ButtonState::Hidden
        );

        // Something else shown meanwhile stays.
        entry.update(cx, |entry, cx| {
            entry.show_up_to_date(cx);
            entry.set_state(ButtonState::Available, cx);
        });
        cx.executor().advance_clock(UP_TO_DATE_DURATION * 2);
        assert_eq!(
            entry.read_with(cx, |e, _| e.state().clone()),
            ButtonState::Available
        );
    }
}
