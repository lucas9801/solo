//! SOLO: the UI of Solo's network rules (see `net_guard`): the `network` and
//! `downloads` settings, the questions before downloads, and the offline
//! button in the status bar.
//!
//! Allowed components are kept in `downloads.allowed` of the user settings
//! file, where they can be read and edited like any other setting (and are
//! synced with it), rather than in Solo's database.

use std::{sync::Arc, time::Duration};

use collections::HashSet;
use fs::Fs;
use futures::{FutureExt as _, StreamExt as _};
use gpui::{
    Action as _, App, AppContext as _, Context, DismissEvent, Global, IntoElement, Render, Window,
};
use net_guard::{Answer, Component, DownloadMode, Policy};
use settings::{RegisterSetting, Settings, SettingsStore, update_settings_file};
use ui::{Tooltip, prelude::*};
use util::i18n::tr;
use workspace::{
    StatusItemView, Workspace,
    item::ItemHandle,
    notifications::{
        NotificationId, dismiss_app_notification, show_app_notification,
        simple_message_notification::MessageNotification,
    },
};

gpui::actions!(
    network,
    [
        /// Turns offline mode on or off.
        ToggleOfflineMode
    ]
);

/// The `network` and `downloads` settings.
#[derive(Clone, Debug, RegisterSetting)]
pub struct NetworkSettings {
    pub policy: Policy,
}

impl Settings for NetworkSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let network = content.network.clone().unwrap_or_default();
        let downloads = content.downloads.clone().unwrap_or_default();
        Self {
            policy: Policy {
                offline: network.offline.unwrap_or(false),
                mode: match downloads.mode.unwrap_or_default() {
                    settings::DownloadMode::Ask => DownloadMode::Ask,
                    settings::DownloadMode::Auto => DownloadMode::Auto,
                    settings::DownloadMode::Never => DownloadMode::Never,
                },
                allowed: downloads.allowed.unwrap_or_default().into_iter().collect(),
                check_updates: downloads.check_updates.unwrap_or(true),
            },
        }
    }
}

/// The questions on screen: their notification and the components they ask
/// about.
#[derive(Default)]
struct Questions {
    shown: Vec<(NotificationId, Vec<String>)>,
}

impl Global for Questions {}

pub fn init(cx: &mut App) {
    apply_settings(cx);
    cx.observe_global::<SettingsStore>(apply_settings).detach();

    cx.set_global(Questions::default());
    let guard = net_guard::global();
    guard.set_asks_user(true);
    let mut changes = guard.subscribe();
    cx.spawn(async move |cx| {
        while changes.next().await.is_some() {
            // Requests that come together are asked together.
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            while let Some(Some(())) = changes.next().now_or_never() {}
            cx.update(update_questions);
        }
    })
    .detach();

    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|_, _: &ToggleOfflineMode, _, cx| toggle_offline(cx));
    })
    .detach();
}

fn apply_settings(cx: &mut App) {
    net_guard::set_policy(NetworkSettings::get_global(cx).policy.clone());
}

/// Turns offline mode on or off, in the user settings.
pub fn toggle_offline(cx: &mut App) {
    let offline = !NetworkSettings::get_global(cx).policy.offline;
    update_settings_file(<dyn Fs>::global(cx), cx, move |content, _| {
        content.network.get_or_insert_default().offline = Some(offline);
    });
}

/// Keeps the allowed components in the settings and shows the questions
/// asked since the last call.
fn update_questions(cx: &mut App) {
    let remember = net_guard::global().take_to_remember();
    if !remember.is_empty() {
        update_settings_file(<dyn Fs>::global(cx), cx, move |content, _| {
            let allowed = content
                .downloads
                .get_or_insert_default()
                .allowed
                .get_or_insert_default();
            for id in remember {
                allowed.insert(id, true);
            }
        });
    }

    let pending = net_guard::pending();
    let pending_ids = pending
        .iter()
        .map(|component| component.id.clone())
        .collect::<HashSet<_>>();
    let mut answered = Vec::new();
    cx.update_global::<Questions, _>(|questions, _| {
        questions.shown.retain(|(id, ids)| {
            let open = ids.iter().any(|id| pending_ids.contains(id));
            if !open {
                answered.push(id.clone());
            }
            open
        });
    });
    for id in answered {
        dismiss_app_notification(&id, cx);
    }

    let shown = cx
        .global::<Questions>()
        .shown
        .iter()
        .flat_map(|(_, ids)| ids.iter().cloned())
        .collect::<HashSet<_>>();
    let new = pending
        .into_iter()
        .filter(|component| !shown.contains(&component.id))
        .collect::<Vec<_>>();
    match new.len() {
        0 => {}
        1 => ask_one(new.into_iter().next().unwrap_or_else(Component::node), cx),
        _ => ask_many(new, cx),
    }
}

fn ids_of(components: &[Component]) -> Vec<String> {
    components
        .iter()
        .map(|component| component.id.clone())
        .collect()
}

/// Closing a question without answering is "Not now".
fn not_now_on_dismiss(
    notification: &gpui::Entity<MessageNotification>,
    ids: Vec<String>,
    cx: &mut Context<Workspace>,
) {
    cx.subscribe(notification, move |_, _, _: &DismissEvent, _| {
        net_guard::answer(&ids, Answer::NotNow);
    })
    .detach();
}

fn ask_one(component: Component, cx: &mut App) {
    let id = NotificationId::named(format!("solo-download-{}", component.id).into());
    let ids = vec![component.id.clone()];
    cx.update_global::<Questions, _>(|questions, _| {
        questions.shown.push((id.clone(), ids.clone()))
    });
    let message = match (&component.source, util::i18n::is_chinese()) {
        (Some(source), false) => {
            format!("Solo needs to download {} from {source}.", component.name)
        }
        (Some(source), true) => format!("Solo 需要从 {source} 下载 {}。", component.name),
        (None, false) => format!("Solo needs to download {}.", component.name),
        (None, true) => format!("Solo 需要下载 {}。", component.name),
    };
    show_app_notification(id, cx, move |cx| {
        let allow_ids = ids.clone();
        let not_now_ids = ids.clone();
        let local_ids = ids.clone();
        let notification = cx.new(|cx| {
            MessageNotification::new(message.clone(), cx)
                .with_title(tr("Download?", "要下载吗？"))
                .content_icon(IconName::Download, Color::Accent)
                .show_suppress_button(false)
                .primary_message(tr("Allow (remember)", "允许（记住）"))
                .primary_on_click(move |_, _| net_guard::answer(&allow_ids, Answer::Allow))
                .secondary_message(tr("Not now", "暂不"))
                .secondary_on_click(move |_, _| net_guard::answer(&not_now_ids, Answer::NotNow))
                .tertiary_message(tr("Use a local path…", "使用本地路径…"))
                .tertiary_on_click(move |window, cx| {
                    net_guard::answer(&local_ids, Answer::NotNow);
                    window.dispatch_action(zed_actions::OpenSettingsFile.boxed_clone(), cx);
                })
        });
        not_now_on_dismiss(&notification, ids.clone(), cx);
        notification
    });
}

fn ask_many(components: Vec<Component>, cx: &mut App) {
    let ids = ids_of(&components);
    let id = NotificationId::named(format!("solo-download-{}", ids.join(",")).into());
    cx.update_global::<Questions, _>(|questions, _| {
        questions.shown.push((id.clone(), ids.clone()))
    });
    let mut message = tr("Solo needs to download:", "Solo 需要下载：").to_string();
    for component in &components {
        message.push_str("\n• ");
        message.push_str(&component.label());
    }
    let components = Arc::new(components);
    show_app_notification(id.clone(), cx, move |cx| {
        let allow_ids = ids.clone();
        let components = components.clone();
        let question = id.clone();
        let id = id.clone();
        let notification = cx.new(|cx| {
            MessageNotification::new(message.clone(), cx)
                .with_title(tr("Download?", "要下载吗？"))
                .content_icon(IconName::Download, Color::Accent)
                .show_suppress_button(false)
                .primary_message(tr("Allow all and remember", "全部允许并记住"))
                .primary_on_click(move |_, _| net_guard::answer(&allow_ids, Answer::Allow))
                .secondary_message(tr("Choose…", "逐个选择…"))
                .secondary_on_click(move |_, cx| {
                    // One question per component instead.
                    let id = id.clone();
                    let components = components.clone();
                    cx.defer(move |cx| {
                        cx.update_global::<Questions, _>(|questions, _| {
                            questions.shown.retain(|(shown, _)| *shown != id)
                        });
                        dismiss_app_notification(&id, cx);
                        for component in components.iter() {
                            ask_one(component.clone(), cx);
                        }
                    });
                })
        });
        // Closing it (not "Choose…") is "Not now" for the components not
        // answered one by one.
        let ids = ids.clone();
        cx.subscribe(&notification, move |_, _, _: &DismissEvent, cx| {
            let choosing = !cx
                .global::<Questions>()
                .shown
                .iter()
                .any(|(shown, _)| *shown == question);
            if !choosing {
                net_guard::answer(&ids, Answer::NotNow);
            }
        })
        .detach();
        notification
    });
}

/// The offline button in the status bar: a globe when online, "Offline" when
/// offline. Clicking it switches.
pub struct OfflineButton {
    _settings: gpui::Subscription,
}

impl OfflineButton {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            _settings: cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
        }
    }
}

impl Render for OfflineButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if NetworkSettings::get_global(cx).policy.offline {
            Button::new("solo-offline", tr("Offline", "离线"))
                .label_size(LabelSize::Small)
                .color(Color::Warning)
                .tooltip(Tooltip::text(tr(
                    "Offline: Solo only connects to this computer and the local network. Click to go online.",
                    "离线：Solo 只连接本机和局域网。点击恢复联网。",
                )))
                .on_click(|_, _, cx| toggle_offline(cx))
                .into_any_element()
        } else {
            IconButton::new("solo-online", IconName::Public)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text(tr(
                    "Online. Click to go offline.",
                    "在线。点击进入离线模式。",
                )))
                .on_click(|_, _, cx| toggle_offline(cx))
                .into_any_element()
        }
    }
}

impl StatusItemView for OfflineButton {
    fn set_active_pane_item(
        &mut self,
        _: Option<&dyn ItemHandle>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<workspace::HideStatusItem> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn settings_become_the_policy(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            let policy = NetworkSettings::get_global(cx).policy.clone();
            assert_eq!(policy, Policy::default());

            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |content| {
                    content.network.get_or_insert_default().offline = Some(true);
                    let downloads = content.downloads.get_or_insert_default();
                    downloads.mode = Some(settings::DownloadMode::Never);
                    downloads.check_updates = Some(false);
                    downloads
                        .allowed
                        .get_or_insert_default()
                        .insert("vtsls".into(), true);
                });
            });
            let policy = NetworkSettings::get_global(cx).policy.clone();
            assert!(policy.offline);
            assert_eq!(policy.mode, DownloadMode::Never);
            assert!(!policy.check_updates);
            assert_eq!(policy.allowed.get("vtsls"), Some(&true));
        });
    }
}
