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
    fn show_new_menu(pane: &Entity<Pane>, cx: &mut VisualTestContext) -> Entity<ui::ContextMenu> {
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
        let view = right.read_with(cx, |pane, cx| {
            pane.active_item()
                .and_then(|item| item.act_as::<BrowserView>(cx))
                .expect("the new page is the active tab")
        });
        assert!(
            cx.update(|window, cx| view.read(cx).address_has_focus(window, cx)),
            "the address bar has the focus"
        );

        // The menu draws each entry through a label that translates its text;
        // run with SOLO_LANGUAGE=zh to check what Chinese users see.
        if util::i18n::is_chinese() {
            let menu = show_new_menu(&right, cx);
            let shown: Vec<_> = menu.read_with(cx, |menu, _| {
                menu.entry_labels()
                    .into_iter()
                    .map(|label| {
                        util::i18n::translate(&label)
                            .map(str::to_string)
                            .unwrap_or_else(|| label.to_string())
                    })
                    .collect()
            });
            assert_eq!(shown, ["新建文件", "新建网页", "打开文件", "搜索项目", "搜索符号"]);
        }
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

    struct Surface {
        id: std::rc::Rc<std::cell::Cell<Option<gpui::HitboxId>>>,
    }

    impl gpui::Render for Surface {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            let id = self.id.clone();
            gpui::div().size_full().child(
                gpui::canvas(
                    move |bounds, window, _| {
                        id.set(Some(
                            window
                                .insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
                                .id,
                        ));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
        }
    }

    /// The close prompt's backdrop spans the window, which the page's
    /// cut-outs leave out, so the dialog itself has to be cut out of the page.
    #[gpui::test]
    async fn test_prompt_dialog_is_cut_out_of_the_page(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| ui_prompt::init(cx));
        let id = std::rc::Rc::new(std::cell::Cell::new(None));
        let (_, cx) = cx.add_window_view({
            let id = id.clone();
            move |_, _| Surface { id }
        });
        let _answer = cx.update(|window, cx| {
            window.prompt(
                gpui::PromptLevel::Warning,
                "Closing will end 4 programs.",
                Some("claude (idle), in alpha"),
                &[
                    gpui::PromptButton::ok("Close"),
                    gpui::PromptButton::cancel("Cancel"),
                ],
                cx,
            )
        });
        cx.run_until_parked();
        let (viewport, holes) = cx.update(|window, _| {
            let (_, holes) = window
                .hitbox_occlusion(id.get().expect("the surface was drawn"))
                .expect("the surface is in the last frame");
            (window.viewport_size(), holes)
        });
        assert!(
            holes
                .iter()
                .any(|hole| hole.size.width < viewport.width && hole.size.height < viewport.height),
            "the dialog is a hole smaller than the window: {holes:?}"
        );
    }

    struct TooltipSurface {
        id: std::rc::Rc<std::cell::Cell<Option<gpui::HitboxId>>>,
    }

    impl gpui::Render for TooltipSurface {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{
                InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
                Styled as _,
            };
            let id = self.id.clone();
            gpui::div()
                .size_full()
                .child(
                    gpui::canvas(
                        move |bounds, window, _| {
                            id.set(Some(
                                window
                                    .insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
                                    .id,
                            ));
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    gpui::div()
                        .id("target")
                        .w(gpui::px(50.))
                        .h(gpui::px(50.))
                        .tooltip(ui::Tooltip::text("Online. Click to go offline.")),
                )
        }
    }

    /// The interface's tooltips have a see-through margin; the page may only
    /// be cut open where the card is drawn.
    #[gpui::test]
    async fn test_tooltip_hole_is_the_card_without_its_margin(cx: &mut TestAppContext) {
        init_test(cx);
        let id = std::rc::Rc::new(std::cell::Cell::new(None));
        let (_, cx) = cx.add_window_view({
            let id = id.clone();
            move |_, _| TooltipSurface { id }
        });
        cx.run_until_parked();
        cx.simulate_mouse_move(
            gpui::point(gpui::px(10.), gpui::px(10.)),
            None,
            gpui::Modifiers::default(),
        );
        cx.executor().advance_clock(std::time::Duration::from_millis(600));
        cx.run_until_parked();
        let holes = cx.update(|window, _| {
            window
                .hitbox_occlusion(id.get().expect("the surface was drawn"))
                .expect("the surface is in the last frame")
                .1
        });
        assert_eq!(holes.len(), 1, "the tooltip is shown: {holes:?}");
        // The tooltip starts 1px from the cursor, with 8px and 10px of margin.
        assert!(
            holes[0].origin.x >= gpui::px(19.) && holes[0].origin.y >= gpui::px(21.),
            "the hole starts at the card: {holes:?}"
        );
    }
}
