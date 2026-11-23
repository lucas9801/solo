//! SOLO: a web browser in the editor area, next to the terminal.
//!
//! Pages are shown by the system's WebView2 runtime in a native child window
//! that follows its tab (see `webview2.rs`). Links from the terminal, the
//! Markdown preview and the timeline go through [`solo_actions::browser::OpenUrl`],
//! which picks Solo's browser or the system browser per the "open links" setting.

pub mod address;
pub mod agent_control;
mod browser_view;
mod host;
mod keys;
mod persistence;
pub mod pick;
#[cfg(target_os = "windows")]
mod webview2;

use gpui::{App, AppContext as _, Context, WeakEntity, Window};
use settings::{RegisterSetting, Settings};
use solo_actions::browser::{OpenBrowser, OpenUrl};
use workspace::Workspace;

pub use address::{LinkTarget, OpenLinksIn, link_target};
pub use browser_view::{BrowserEvent, BrowserView, Viewport};

#[derive(Clone, Debug, RegisterSetting)]
pub struct BrowserSettings {
    pub open_links: OpenLinksIn,
    pub allow_agent_control: bool,
}

impl Settings for BrowserSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let browser = content.browser.clone().unwrap_or_default();
        Self {
            open_links: match browser.open_links.unwrap_or_default() {
                settings::BrowserOpenLinks::LocalInSolo => OpenLinksIn::LocalInSolo,
                settings::BrowserOpenLinks::AllInSolo => OpenLinksIn::AllInSolo,
                settings::BrowserOpenLinks::AllInSystem => OpenLinksIn::AllInSystem,
            },
            allow_agent_control: browser.allow_agent_control.unwrap_or(false),
        }
    }
}

pub fn init(cx: &mut App) {
    workspace::register_serializable_item::<BrowserView>(cx);
    agent_control::init(cx);
    clean_up_picks(cx);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace
            .register_action(|workspace, _: &OpenBrowser, window, cx| {
                let weak = cx.entity().downgrade();
                let view = cx.new(|cx| BrowserView::new(None, weak, window, cx));
                workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx);
                view.update(cx, |view, cx| view.focus_address(window, cx));
            })
            .register_action(|workspace, action: &OpenUrl, window, cx| {
                open_url(workspace, action, window, cx);
            });
    })
    .detach();
}

/// Deletes element screenshots (see "Pick") older than a week: they're only
/// needed until the agent has looked at them.
fn clean_up_picks(cx: &mut App) {
    cx.background_spawn(async {
        let Ok(entries) = std::fs::read_dir(browser_view::picks_dir()) else {
            return;
        };
        let week = std::time::Duration::from_secs(7 * 24 * 60 * 60);
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > week);
            if old {
                std::fs::remove_file(entry.path()).ok();
            }
        }
    })
    .detach();
}

/// Opens a link where the "open links" setting says.
pub fn open_url(
    workspace: &mut Workspace,
    action: &OpenUrl,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let setting = BrowserSettings::get_global(cx).open_links;
    let target = if cfg!(target_os = "windows") {
        match action.in_solo {
            // SOLO: picked in the terminal's link menu.
            Some(true)
                if url::Url::parse(&action.url)
                    .ok()
                    .as_ref()
                    .is_some_and(address::is_allowed_in_solo) =>
            {
                LinkTarget::Solo
            }
            Some(_) => LinkTarget::System,
            None => link_target(&action.url, setting, action.alternate),
        }
    } else {
        LinkTarget::System
    };
    match target {
        LinkTarget::System => cx.open_url(&action.url),
        LinkTarget::Solo => {
            let weak = cx.entity().downgrade();
            show_page(
                workspace,
                weak,
                action.url.clone(),
                !action.in_background,
                window,
                cx,
            );
        }
    }
}

/// Shows a window a page opened in a new tab of Solo's browser.
pub(crate) fn open_new_window(
    workspace: &WeakEntity<Workspace>,
    request: host::NewWindowRequest,
    focus: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let weak = workspace.clone();
    workspace
        .update(cx, |workspace, cx| {
            let view = cx.new(|cx| BrowserView::for_new_window(request, weak, window, cx));
            workspace.add_item_to_active_pane(Box::new(view), None, focus, window, cx);
        })
        .ok();
}

/// Switches to the tab already showing `url`, or opens a new one.
fn show_page(
    workspace: &mut Workspace,
    weak: WeakEntity<Workspace>,
    url: String,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing = workspace
        .items_of_type::<BrowserView>(cx)
        .find(|view| address::same_page(view.read(cx).url(), &url));
    if let Some(existing) = existing {
        workspace.activate_item(&existing, true, focus, window, cx);
        return;
    }
    let view = cx.new(|cx| BrowserView::new(Some(url), weak, window, cx));
    workspace.add_item_to_active_pane(Box::new(view), None, focus, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
    use project::Project;
    use settings::SettingsStore;
    use workspace::{Pane, SplitDirection};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            init(cx);
        });
    }

    /// Opens the "+" menu of `pane` the way a click does: the pane focused,
    /// the button's menu shown.
    fn show_new_menu(
        pane: &Entity<Pane>,
        cx: &mut VisualTestContext,
    ) -> Entity<ui::ContextMenu> {
        pane.update_in(cx, |pane, window, cx| {
            window.focus(&pane.focus_handle(cx), cx)
        });
        cx.run_until_parked();
        let handle = pane.read_with(cx, |pane, _| pane.new_item_context_menu_handle.clone());
        cx.update(|window, cx| handle.show(window, cx));
        cx.run_until_parked();
        handle.menu().expect("the + menu is shown")
    }

    #[gpui::test]
    async fn test_new_web_page_in_the_pane_of_the_plus(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let left = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let right = workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_pane(left.clone(), SplitDirection::Right, window, cx)
        });
        // Panes without tabs draw no tab bar, hence no "+".
        for pane in [&left, &right] {
            workspace.update_in(cx, |workspace, window, cx| {
                let item = cx.new(workspace::item::test::TestItem::new);
                workspace.add_item(pane.clone(), Box::new(item), None, false, false, window, cx);
            });
        }
        left.update_in(cx, |pane, window, cx| {
            window.focus(&pane.focus_handle(cx), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.active_pane().clone()),
            left
        );

        let menu = show_new_menu(&right, cx);
        let labels: Vec<_> = menu.read_with(cx, |menu, _| {
            menu.entry_labels()
                .into_iter()
                .map(|label| label.to_string())
                .collect()
        });
        assert_eq!(
            labels,
            [
                "New File",
                "New Web Page",
                "Open File",
                "Search Project",
                "Search Symbols"
            ]
        );

        menu.update_in(cx, |menu, window, cx| {
            menu.select_first(&menu::SelectFirst, window, cx);
            menu.select_next(&menu::SelectNext, window, cx);
            menu.confirm(&menu::Confirm, window, cx);
        });
        cx.run_until_parked();

        let browser_pages = |pane: &Entity<Pane>, cx: &mut VisualTestContext| {
            pane.read_with(cx, |pane, cx| {
                pane.items()
                    .filter(|item| item.act_as::<BrowserView>(cx).is_some())
                    .count()
            })
        };
        assert_eq!(browser_pages(&right, cx), 1, "the page opens on the right");
        assert_eq!(browser_pages(&left, cx), 0);
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.active_pane().clone()),
            right
        );
        right.read_with(cx, |pane, cx| {
            let view = pane
                .active_item()
                .and_then(|item| item.act_as::<BrowserView>(cx))
                .expect("the new page is the active tab");
            assert_eq!(view.read(cx).url(), "", "the address bar starts empty");
        });
    }

    /// The menu and the command palette name the action alike, in Chinese.
    #[test]
    fn test_new_web_page_names_in_chinese() {
        assert_eq!(util::i18n::chinese("New Web Page"), Some("新建网页"));
        assert_eq!(
            util::i18n::chinese("browser: open browser"),
            Some("浏览器: 新建网页")
        );
    }
}
