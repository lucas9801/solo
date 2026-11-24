//! A tab of Solo's browser in the editor area.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use editor::{Editor, EditorEvent};
use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    Action as _, AnyElement, App, Bounds, ClipboardItem, Context, Element, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, GlobalElementId, Hitbox, HitboxBehavior, HitboxId,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Render, SharedString, Styled,
    Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px,
};
use project::Project;
use ui::{
    ButtonStyle, ContextMenu, Icon, IconButton, IconName, IconSize, Label, LabelSize, PopoverMenu,
    Tooltip, prelude::*,
};
use util::i18n::tr;
use workspace::{
    ItemId, Workspace, WorkspaceId,
    item::{Item, ItemEvent, SerializableItem, TabContentParams},
};

use solo_actions::agent_terminal::{PageComment, SendPageCommentsToAgent};

use crate::{
    address::{display_address, normalize_address},
    host::{EnvironmentConfig, ForwardedKeys, Host, HostEvent, NewWindowRequest},
    persistence::BrowserDb,
    pick::{PickedElement, short_text, truncate_html, unique_selector},
};

actions!(
    browser,
    [
        /// Goes back to the previous page.
        GoBack,
        /// Goes forward to the next page.
        GoForward,
        /// Reloads the page.
        Reload,
        /// Reloads the page without using the cache.
        HardReload,
        /// Moves focus to the address bar.
        FocusAddressBar,
        /// Opens the developer tools for the page.
        OpenDevTools,
        /// Finds text in the page.
        FindInPage,
        /// Opens the page in the system browser.
        OpenInSystemBrowser,
        /// Copies the page's address.
        CopyUrl,
        /// Starts or stops picking elements on the page to comment on.
        TogglePick,
        /// Sends the comments on the page's elements to the agent used last.
        SendCommentsToAgent,
    ]
);

/// A comment being written on a picked element, shown over the page.
struct CommentPopup {
    /// The comment being changed, or `None` for a new one.
    index: Option<usize>,
    draft: PageComment,
    editor: Entity<Editor>,
    /// Where the element is, relative to the page area.
    anchor: gpui::Point<Pixels>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Viewport {
    #[default]
    Desktop,
    Tablet,
    Phone,
}

impl Viewport {
    fn size(self) -> Option<(f32, f32)> {
        match self {
            Viewport::Desktop => None,
            Viewport::Tablet => Some((768., 1024.)),
            Viewport::Phone => Some((390., 844.)),
        }
    }
}

enum HostState {
    Starting,
    Ready,
    /// No WebView2 runtime, or the page could not be created.
    Unavailable {
        runtime_missing: bool,
        error: String,
    },
}

pub enum BrowserEvent {
    TitleChanged,
}

pub struct BrowserView {
    focus_handle: FocusHandle,
    address_editor: Entity<Editor>,
    url: String,
    title: Option<String>,
    can_go_back: bool,
    can_go_forward: bool,
    loading: bool,
    viewport: Viewport,
    applied_viewport: Option<(i32, i32)>,
    state: HostState,
    host: Option<Rc<Host>>,
    forwarded: Rc<RefCell<ForwardedKeys>>,
    surface_hitbox: Rc<Cell<Option<HitboxId>>>,
    surface_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    workspace: WeakEntity<Workspace>,
    picking: bool,
    comments: Vec<PageComment>,
    popup: Option<CommentPopup>,
    _subscriptions: Vec<Subscription>,
    _events_task: Option<Task<()>>,
}

impl BrowserView {
    pub fn new(
        url: Option<String>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with(url, None, workspace, window, cx)
    }

    /// A tab for a window another page opened.
    pub(crate) fn for_new_window(
        request: NewWindowRequest,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = request.url.clone();
        Self::new_with(Some(url), Some(request), workspace, window, cx)
    }

    fn new_with(
        url: Option<String>,
        adopt: Option<NewWindowRequest>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let address_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(tr("Enter an address", "输入网址"), window, cx);
            if let Some(url) = &url {
                editor.set_text(display_address(url), window, cx);
            }
            editor
        });

        let mut subscriptions = vec![
            cx.subscribe_in(&address_editor, window, Self::on_address_event),
            cx.on_focus(&focus_handle, window, |this, _, cx| {
                this.refresh_forwarded_keys(cx);
                if let Some(host) = &this.host {
                    host.focus();
                }
            }),
            cx.on_blur(&focus_handle, window, |this, window, _| {
                if let Some(host) = &this.host
                    && window.is_window_active()
                {
                    host.release_focus();
                }
            }),
        ];

        let surface_hitbox = Rc::new(Cell::new(None));
        let surface_bounds = Rc::new(Cell::new(None));
        let forwarded = Rc::new(RefCell::new(ForwardedKeys::default()));
        let (tx, mut rx) = mpsc::unbounded();

        let (state, host) = match crate::host::runtime_available().and_then(|()| {
            Host::new(
                window,
                &environment_config(cx),
                url.clone(),
                adopt,
                tx,
                forwarded.clone(),
            )
        }) {
            Ok(host) => (HostState::Starting, Some(Rc::new(host))),
            Err(error) => (
                HostState::Unavailable {
                    runtime_missing: crate::host::runtime_available().is_err(),
                    error: format!("{error:#}"),
                },
                None,
            ),
        };

        let events_task = host.as_ref().map(|_| {
            cx.spawn_in(window, async move |this, cx| {
                while let Some(event) = rx.next().await {
                    if this
                        .update_in(cx, |this, window, cx| {
                            this.handle_host_event(event, window, cx)
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });

        if let Some(host) = &host {
            let host = Rc::downgrade(host);
            let surface_hitbox = surface_hitbox.clone();
            subscriptions.push(window.on_frame_drawn(cx, move |window, _| {
                let Some(host) = host.upgrade() else {
                    return;
                };
                let viewport = window.viewport_size();
                let placement = surface_hitbox
                    .get()
                    .and_then(|id| window.hitbox_occlusion(id))
                    .map(|(bounds, mut holes)| {
                        // A modal's backdrop spans the whole window: keep the
                        // page visible around the modal. Clicking the page then
                        // moves focus, which closes the modal like a click on
                        // the backdrop would.
                        holes.retain(|hole| {
                            hole.size.width < viewport.width || hole.size.height < viewport.height
                        });
                        (bounds, holes)
                    });
                host.place(placement, window.scale_factor());
            }));
            subscriptions.push(cx.observe_window_bounds(window, |this, _, _| {
                if let Some(host) = &this.host {
                    host.parent_moved();
                }
            }));
        }

        Self {
            focus_handle,
            address_editor,
            url: url.unwrap_or_default(),
            title: None,
            can_go_back: false,
            can_go_forward: false,
            loading: false,
            viewport: Viewport::Desktop,
            applied_viewport: None,
            state,
            host,
            forwarded,
            surface_hitbox,
            surface_bounds,
            workspace,
            picking: false,
            comments: Vec::new(),
            popup: None,
            _subscriptions: subscriptions,
            _events_task: events_task,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn navigate(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.url = url.clone();
        self.address_editor.update(cx, |editor, cx| {
            editor.set_text(display_address(&url), window, cx)
        });
        if let Some(host) = &self.host {
            host.navigate(&url);
        }
        cx.notify();
    }

    fn on_address_event(
        &mut self,
        _: &Entity<Editor>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let EditorEvent::Blurred = event {
            // Show where the page is, not the half-typed text.
            let shown = display_address(&self.url);
            self.address_editor
                .update(cx, |editor, cx| editor.set_text(shown, window, cx));
        }
    }

    fn confirm_address(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.address_editor.read(cx).text(cx);
        match normalize_address(&typed) {
            Some(url) => {
                self.navigate(url, window, cx);
                window.focus(&self.focus_handle, cx);
            }
            None => {
                // Not an address Solo opens: leave the text for the person to fix.
                log::info!("not an address: {typed}");
            }
        }
    }

    fn handle_host_event(&mut self, event: HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::Ready => {
                self.state = HostState::Ready;
                self.applied_viewport = None;
                if self.focus_handle.is_focused(window)
                    && let Some(host) = &self.host
                {
                    host.focus();
                }
            }
            HostEvent::Failed(error) => {
                log::error!("browser tab: {error}");
                self.state = HostState::Unavailable {
                    runtime_missing: false,
                    error,
                };
                self.host = None;
            }
            HostEvent::SourceChanged(url) => {
                self.url = url;
                if !self.address_editor.focus_handle(cx).is_focused(window) {
                    let shown = display_address(&self.url);
                    self.address_editor
                        .update(cx, |editor, cx| editor.set_text(shown, window, cx));
                }
                cx.emit(BrowserEvent::TitleChanged);
            }
            HostEvent::TitleChanged(title) => {
                self.title = (!title.is_empty()).then_some(title);
                cx.emit(BrowserEvent::TitleChanged);
            }
            HostEvent::HistoryChanged {
                can_go_back,
                can_go_forward,
            } => {
                self.can_go_back = can_go_back;
                self.can_go_forward = can_go_forward;
            }
            HostEvent::Loading(loading) => {
                self.loading = loading;
                if !loading {
                    // A new document starts without the emulated size, or
                    // the pick script.
                    self.applied_viewport = None;
                    if self.picking {
                        self.inject_pick_script();
                    }
                }
            }
            HostEvent::NewWindow(request) => {
                // Follow a window the person opened; one an agent opened shows
                // up without taking the focus.
                let focus = self.focus_handle.contains_focused(window, cx);
                let workspace = self.workspace.clone();
                // Not while this view is being updated: adding the tab reads it.
                window.defer(cx, move |window, cx| {
                    crate::open_new_window(&workspace, request, focus, window, cx)
                });
            }
            HostEvent::GotFocus => {
                if !self.focus_handle.is_focused(window) {
                    window.focus(&self.focus_handle, cx);
                }
            }
            HostEvent::WebMessage(message) => self.handle_pick_message(&message, window, cx),
        }
        cx.notify();
    }

    /// The keystrokes that go to Solo while the page has focus: anything with
    /// Ctrl or Alt, or a function key, that Solo binds, except the ones pages
    /// rely on for editing text.
    fn refresh_forwarded_keys(&self, cx: &App) {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut keys = std::collections::HashSet::default();
        for binding in keymap.bindings() {
            let Some(first) = binding.keystrokes().first() else {
                continue;
            };
            let modifiers = *first.modifiers();
            let key = first.key().to_string();
            if crate::keys::forward_to_solo(&modifiers, &key) {
                keys.insert((modifiers, key));
            }
        }
        self.forwarded.borrow_mut().keys = keys;
    }

    fn go_back(&mut self, _: &GoBack, _: &mut Window, _: &mut Context<Self>) {
        if let Some(host) = &self.host {
            host.go_back();
        }
    }

    fn go_forward(&mut self, _: &GoForward, _: &mut Window, _: &mut Context<Self>) {
        if let Some(host) = &self.host {
            host.go_forward();
        }
    }

    fn reload(&mut self, _: &Reload, _: &mut Window, _: &mut Context<Self>) {
        if let Some(host) = &self.host {
            host.reload(false);
        }
    }

    fn hard_reload(&mut self, _: &HardReload, _: &mut Window, _: &mut Context<Self>) {
        if let Some(host) = &self.host {
            host.reload(true);
        }
    }

    #[cfg(test)]
    pub(crate) fn address_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.address_editor.focus_handle(cx).is_focused(window)
    }

    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_address_bar(&FocusAddressBar, window, cx);
    }

    fn focus_address_bar(
        &mut self,
        _: &FocusAddressBar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.address_editor.update(cx, |editor, cx| {
            editor.select_all(&editor::actions::SelectAll, window, cx);
        });
        window.focus(&self.address_editor.focus_handle(cx), cx);
    }

    fn open_devtools(&mut self, _: &OpenDevTools, _: &mut Window, _: &mut Context<Self>) {
        if let Some(host) = &self.host {
            host.open_devtools();
        }
    }

    fn find_in_page(&mut self, _: &FindInPage, _: &mut Window, _: &mut Context<Self>) {
        if let Some(host) = &self.host
            && !host.find()
        {
            log::warn!("this WebView2 runtime has no find API");
        }
    }

    fn open_in_system_browser(
        &mut self,
        _: &OpenInSystemBrowser,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.url.is_empty() {
            cx.open_url(&self.url);
        }
    }

    fn copy_url(&mut self, _: &CopyUrl, _: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.url.clone()));
    }

    fn set_viewport(&mut self, viewport: Viewport, cx: &mut Context<Self>) {
        self.viewport = viewport;
        self.applied_viewport = None;
        cx.notify();
    }

    /// Emulates the chosen device size in the page: called while painting,
    /// with the size the page area got.
    fn apply_viewport(&mut self, size: (i32, i32)) {
        let wanted = self.viewport.size().map(|_| size);
        let key = wanted.unwrap_or((0, 0));
        if self.applied_viewport == Some(key) || !matches!(self.state, HostState::Ready) {
            return;
        }
        self.applied_viewport = Some(key);
        let Some(host) = &self.host else {
            return;
        };
        match wanted {
            Some((width, height)) => host.call_devtools(
                "Emulation.setDeviceMetricsOverride",
                &serde_json::json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": 0,
                    "mobile": true,
                })
                .to_string(),
                Box::new(|result| {
                    if let Err(error) = result {
                        log::warn!("viewport: {error:#}");
                    }
                }),
            ),
            None => host.call_devtools(
                "Emulation.clearDeviceMetricsOverride",
                "{}",
                Box::new(|_| {}),
            ),
        }
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.focus_handle.clone();
        let viewport_button = |id: &'static str,
                               label: &'static str,
                               viewport: Viewport,
                               this: &Self,
                               cx: &mut Context<Self>| {
            Button::new(id, label)
                .label_size(LabelSize::Small)
                .style(ButtonStyle::Subtle)
                .toggle_state(this.viewport == viewport)
                .on_click(cx.listener(move |this, _, _, cx| this.set_viewport(viewport, cx)))
        };
        h_flex()
            .h(px(34.))
            .px_1p5()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().toolbar_background)
            .child(
                IconButton::new("browser-back", IconName::ArrowLeft)
                    .icon_size(IconSize::Small)
                    .disabled(!self.can_go_back)
                    .tooltip(Tooltip::for_action_title_in(
                        tr("Back", "后退"),
                        &GoBack,
                        &focus_handle,
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(&GoBack, window, cx))),
            )
            .child(
                IconButton::new("browser-forward", IconName::ArrowRight)
                    .icon_size(IconSize::Small)
                    .disabled(!self.can_go_forward)
                    .tooltip(Tooltip::for_action_title_in(
                        tr("Forward", "前进"),
                        &GoForward,
                        &focus_handle,
                    ))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.go_forward(&GoForward, window, cx)),
                    ),
            )
            .child(
                div()
                    .id("browser-reload-container")
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(|this, _, window, cx| {
                            this.hard_reload(&HardReload, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        IconButton::new("browser-reload", IconName::RotateCw)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text(tr(
                                "Reload (right-click: without cache)",
                                "刷新（右键：不用缓存）",
                            )))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reload(&Reload, window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(24.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().editor_background)
                    .key_context("BrowserAddressBar")
                    .on_action(cx.listener(Self::confirm_address))
                    .child(self.address_editor.clone()),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .child(viewport_button(
                        "viewport-desktop",
                        tr("Desktop", "桌面"),
                        Viewport::Desktop,
                        self,
                        cx,
                    ))
                    .child(viewport_button(
                        "viewport-tablet",
                        tr("Tablet", "平板"),
                        Viewport::Tablet,
                        self,
                        cx,
                    ))
                    .child(viewport_button(
                        "viewport-phone",
                        tr("Phone", "手机"),
                        Viewport::Phone,
                        self,
                        cx,
                    )),
            )
            .child(
                Button::new("browser-pick", tr("Pick", "选取"))
                    .start_icon(Icon::new(IconName::Crosshair).size(IconSize::Small))
                    .label_size(LabelSize::Small)
                    .style(if self.picking {
                        ButtonStyle::Tinted(ui::TintColor::Accent)
                    } else {
                        ButtonStyle::Outlined
                    })
                    .toggle_state(self.picking)
                    .tooltip(Tooltip::text(tr(
                        "Pick elements to comment on (Esc to stop)",
                        "选取元素写批注（Esc 退出）",
                    )))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.toggle_pick(&TogglePick, window, cx)
                        }),
                    ),
            )
            .when(!self.comments.is_empty(), |this| {
                this.child(
                    h_flex()
                        .child(
                            Button::new(
                                "browser-send-comments",
                                format!(
                                    "{} · {}",
                                    tr("Send to Agent", "发送给 agent"),
                                    self.comments.len()
                                ),
                            )
                            .label_size(LabelSize::Small)
                            .style(ButtonStyle::Filled)
                            .tooltip(Tooltip::text(tr(
                                "Paste the comments into the agent used last",
                                "粘贴到最近用过的 agent 的输入里",
                            )))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send(false, window, cx)),
                            ),
                        )
                        .child(
                            IconButton::new("browser-send-choose", IconName::ChevronDown)
                                .icon_size(IconSize::Small)
                                .style(ButtonStyle::Filled)
                                .tooltip(Tooltip::text(tr("Choose an agent…", "选择 agent…")))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send(true, window, cx)),
                                ),
                        ),
                )
            })
            .child({
                let focus_handle = focus_handle.clone();
                PopoverMenu::new("browser-more")
                    .trigger_with_tooltip(
                        IconButton::new("browser-more-button", IconName::Ellipsis)
                            .icon_size(IconSize::Small),
                        Tooltip::text(tr("More", "更多")),
                    )
                    .anchor(gpui::Anchor::TopRight)
                    .menu(move |window, cx| {
                        let focus_handle = focus_handle.clone();
                        Some(ContextMenu::build(window, cx, |menu, _, _| {
                            menu.context(focus_handle)
                                .action(
                                    tr("Developer Tools", "开发者工具"),
                                    OpenDevTools.boxed_clone(),
                                )
                                .action(
                                    tr("Find in Page", "在页面中查找"),
                                    FindInPage.boxed_clone(),
                                )
                                .separator()
                                .action(
                                    tr("Open in System Browser", "在系统浏览器中打开"),
                                    OpenInSystemBrowser.boxed_clone(),
                                )
                                .action(tr("Copy Address", "复制网址"), CopyUrl.boxed_clone())
                        }))
                    })
            })
    }

    fn toggle_pick(&mut self, _: &TogglePick, _: &mut Window, cx: &mut Context<Self>) {
        self.set_picking(!self.picking, cx);
    }

    fn set_picking(&mut self, picking: bool, cx: &mut Context<Self>) {
        if self.picking == picking {
            return;
        }
        self.picking = picking;
        if picking {
            self.inject_pick_script();
        } else {
            self.popup = None;
            if let Some(host) = &self.host {
                host.execute_script(
                    "window.__soloPick && window.__soloPick.stop()",
                    Box::new(|_| {}),
                );
            }
        }
        cx.notify();
    }

    fn inject_pick_script(&self) {
        let Some(host) = &self.host else {
            return;
        };
        host.execute_script(
            PICK_SCRIPT,
            Box::new(|result| {
                if let Err(error) = result {
                    log::warn!("pick script: {error:#}");
                }
            }),
        );
        self.update_marks();
    }

    /// Numbers the elements that have comments, found again by selector (so
    /// after a reload or a hot update too).
    fn update_marks(&self) {
        let Some(host) = &self.host else {
            return;
        };
        let marks = self
            .comments
            .iter()
            .enumerate()
            .map(|(index, comment)| {
                serde_json::json!({ "n": index + 1, "selector": comment.selector })
            })
            .collect::<Vec<_>>();
        host.execute_script(
            &format!(
                "window.__soloPick && window.__soloPick.setMarks({})",
                serde_json::Value::Array(marks)
            ),
            Box::new(|_| {}),
        );
    }

    fn handle_pick_message(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(message) else {
            return;
        };
        match value["type"].as_str() {
            Some("cancel") => self.set_picking(false, cx),
            Some("mark") => {
                let anchor = rect_anchor(&value["rect"]);
                if let Some(index) = value["n"]
                    .as_u64()
                    .and_then(|n| (n as usize).checked_sub(1))
                    && let Some(comment) = self.comments.get(index).cloned()
                {
                    self.open_popup(Some(index), comment, anchor, window, cx);
                }
            }
            Some("pick") => match serde_json::from_value::<PickedElement>(value) {
                Ok(picked) => self.picked(picked, window, cx),
                Err(error) => log::warn!("pick message: {error}"),
            },
            _ => {}
        }
    }

    fn picked(&mut self, picked: PickedElement, window: &mut Window, cx: &mut Context<Self>) {
        let selector =
            unique_selector(&picked.document, &picked.path).unwrap_or_else(|| "body".into());
        let screenshot = pick_screenshot_path();
        let draft = PageComment {
            selector,
            text: short_text(&picked.text, 20),
            html: truncate_html(&picked.html),
            styles: picked.styles.clone(),
            screenshot: Some(screenshot.clone()),
            comment: String::new(),
        };
        if let Some(host) = &self.host {
            capture_element(host, &picked, screenshot);
        }
        let anchor = gpui::point(
            px(picked.rect.x as f32),
            px((picked.rect.y + picked.rect.height) as f32),
        );
        self.open_popup(None, draft, anchor, window, cx);
    }

    fn open_popup(
        &mut self,
        index: Option<usize>,
        draft: PageComment,
        anchor: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(2, 8, window, cx);
            editor.set_placeholder_text(tr("What should change?", "要改什么？"), window, cx);
            editor.set_text(draft.comment.clone(), window, cx);
            editor
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.popup = Some(CommentPopup {
            index,
            draft,
            editor,
            anchor,
        });
        cx.notify();
    }

    fn close_popup(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let text = popup.editor.read(cx).text(cx).trim().to_string();
        let mut draft = popup.draft;
        match (save, popup.index) {
            (true, _) if text.is_empty() => {}
            (true, Some(index)) => {
                draft.comment = text;
                if let Some(comment) = self.comments.get_mut(index) {
                    *comment = draft;
                }
            }
            (true, None) => {
                draft.comment = text;
                self.comments.push(draft);
            }
            (false, None) => {
                if let Some(path) = &draft.screenshot {
                    std::fs::remove_file(path).ok();
                }
            }
            (false, Some(_)) => {}
        }
        self.update_marks();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn delete_popup_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        if let Some(index) = popup.index
            && index < self.comments.len()
        {
            let removed = self.comments.remove(index);
            if let Some(path) = &removed.screenshot {
                std::fs::remove_file(path).ok();
            }
        }
        self.update_marks();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn send_comments(
        &mut self,
        _: &SendCommentsToAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send(false, window, cx);
    }

    /// Pastes the comments into an agent's prompt (without sending it) and
    /// clears them.
    fn send(&mut self, choose_terminal: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.comments.is_empty() {
            return;
        }
        let mut comments = std::mem::take(&mut self.comments);
        for comment in &mut comments {
            if comment
                .screenshot
                .as_ref()
                .is_some_and(|path| !path.is_file())
            {
                comment.screenshot = None;
            }
        }
        window.dispatch_action(
            Box::new(SendPageCommentsToAgent {
                url: self.url.clone(),
                comments,
                choose_terminal,
            }),
            cx,
        );
        self.set_picking(false, cx);
        self.update_marks();
        cx.notify();
    }

    fn render_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popup = self.popup.as_ref()?;
        let origin = self.surface_bounds.get()?.origin;
        let position = origin + popup.anchor + gpui::point(px(0.), px(6.));
        let title = if popup.draft.text.is_empty() {
            popup.draft.selector.clone()
        } else {
            format!("{} “{}”", popup.draft.selector, popup.draft.text)
        };
        let editing = popup.index.is_some();
        let colors = cx.theme().colors();
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        v_flex()
                            .id("browser-comment-popup")
                            .occlude()
                            .w(px(300.))
                            .p_3()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.elevated_surface_background)
                            .shadow_lg()
                            .key_context("BrowserCommentPopup")
                            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                                this.close_popup(false, window, cx)
                            }))
                            .on_action(cx.listener(
                                |this, _: &editor::actions::Cancel, window, cx| {
                                    this.close_popup(false, window, cx)
                                },
                            ))
                            .child(
                                Label::new(title)
                                    .size(LabelSize::Small)
                                    .color(Color::Accent)
                                    .buffer_font(cx)
                                    .truncate(),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(colors.border_focused)
                                    .bg(colors.editor_background)
                                    .child(popup.editor.clone()),
                            )
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .when(editing, |this| {
                                        this.child(
                                            Button::new("comment-delete", tr("Delete", "删除"))
                                                .style(ButtonStyle::Subtle)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.delete_popup_comment(window, cx)
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::new("comment-cancel", tr("Cancel", "取消"))
                                            .style(ButtonStyle::Outlined)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.close_popup(false, window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(
                                            "comment-add",
                                            if editing {
                                                tr("Save", "保存")
                                            } else {
                                                tr("Add", "添加")
                                            },
                                        )
                                        .style(ButtonStyle::Filled)
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.close_popup(true, window, cx)
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn render_unavailable(
        &self,
        runtime_missing: bool,
        error: &str,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let url = self.url.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(Label::new(if runtime_missing {
                SharedString::from(tr(
                    "Microsoft Edge WebView2 Runtime is required",
                    "需要安装 Microsoft Edge WebView2 运行时",
                ))
            } else {
                SharedString::from(format!(
                    "{} {error}",
                    tr("The page could not be shown:", "无法显示网页：")
                ))
            }))
            .child(
                h_flex()
                    .gap_2()
                    .when(runtime_missing, |this| {
                        this.child(
                            Button::new("download-webview2", tr("Download", "下载"))
                                .style(ButtonStyle::Filled)
                                .on_click(|_, _, cx| {
                                    cx.open_url(
                                        "https://developer.microsoft.com/microsoft-edge/webview2/",
                                    )
                                }),
                        )
                    })
                    .when(!url.is_empty(), |this| {
                        this.child(
                            Button::new(
                                "open-in-system-browser",
                                tr("Open in System Browser", "在系统浏览器中打开"),
                            )
                            .style(ButtonStyle::Outlined)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .into_any_element()
    }
}

const PICK_SCRIPT: &str = include_str!("pick.js");

/// Element screenshots go to `<data dir>/browser/picks`; ones older than a
/// week are deleted when Solo starts (see [`crate::clean_up_picks`]).
pub(crate) fn picks_dir() -> std::path::PathBuf {
    paths::data_dir().join("browser").join("picks")
}

fn pick_screenshot_path() -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    picks_dir().join(format!("{stamp}.png"))
}

/// The bottom-left corner of a `{x, y, width, height}` box from the page.
fn rect_anchor(rect: &serde_json::Value) -> gpui::Point<Pixels> {
    let number = |key: &str| rect[key].as_f64().unwrap_or(0.) as f32;
    gpui::point(px(number("x")), px(number("y") + number("height")))
}

/// Saves a screenshot of the picked element to `path`, in the background of
/// the page's own process.
fn capture_element(host: &Host, picked: &PickedElement, path: std::path::PathBuf) {
    let rect = picked.rect;
    let params = serde_json::json!({
        "format": "png",
        "clip": {
            // Document coordinates.
            "x": rect.x + picked.scroll_x,
            "y": rect.y + picked.scroll_y,
            "width": rect.width.max(1.),
            "height": rect.height.max(1.),
            "scale": 1,
        },
    });
    host.call_devtools(
        "Page.captureScreenshot",
        &params.to_string(),
        Box::new(move |result| {
            use base64::Engine as _;
            let written = result.and_then(|json| {
                let value: serde_json::Value = serde_json::from_str(&json)?;
                let data = value["data"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("no image data"))?;
                let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, bytes)?;
                Ok(())
            });
            if let Err(error) = written {
                log::warn!("element screenshot: {error:#}");
            }
        }),
    );
}

/// The browser process's settings: where it keeps its data, and the debugging
/// port when agents may control it.
pub(crate) fn environment_config(cx: &App) -> EnvironmentConfig {
    EnvironmentConfig {
        user_data_dir: paths::data_dir().join("browser"),
        remote_debugging_port: crate::agent_control::debugging_port(cx),
    }
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match &self.state {
            HostState::Unavailable {
                runtime_missing,
                error,
            } => {
                let (runtime_missing, error) = (*runtime_missing, error.clone());
                self.render_unavailable(runtime_missing, &error, cx)
            }
            HostState::Starting | HostState::Ready => {
                let surface = WebSurface {
                    view: cx.entity().downgrade(),
                    hitbox: self.surface_hitbox.clone(),
                    bounds: self.surface_bounds.clone(),
                };
                match self.viewport.size() {
                    None => div().size_full().child(surface).into_any_element(),
                    Some((width, height)) => v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .bg(cx.theme().colors().panel_background)
                        .child(
                            div()
                                .w(px(width))
                                .h(px(height))
                                .max_w_full()
                                .max_h(gpui::relative(0.92))
                                .border_1()
                                .border_color(cx.theme().colors().border)
                                .child(surface),
                        )
                        .child(
                            Label::new(format!("{} × {}", width as u32, height as u32))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .into_any_element(),
                }
            }
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("BrowserView")
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::go_forward))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::hard_reload))
            .on_action(cx.listener(Self::focus_address_bar))
            .on_action(cx.listener(Self::open_devtools))
            .on_action(cx.listener(Self::find_in_page))
            .on_action(cx.listener(Self::open_in_system_browser))
            .on_action(cx.listener(Self::copy_url))
            .on_action(cx.listener(Self::toggle_pick))
            .on_action(cx.listener(Self::send_comments))
            .bg(cx.theme().colors().editor_background)
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(content))
            .when_some(self.render_popup(cx), |this, popup| this.child(popup))
    }
}

/// The area the page covers. Paints nothing: the page's window sits on top.
struct WebSurface {
    view: WeakEntity<BrowserView>,
    hitbox: Rc<Cell<Option<HitboxId>>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl IntoElement for WebSurface {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebSurface {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = gpui::relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.hitbox.set(Some(hitbox.id));
        self.bounds.set(Some(bounds));
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let size = (
            f32::from(bounds.size.width).round() as i32,
            f32::from(bounds.size.height).round() as i32,
        );
        if let Some(view) = self.view.upgrade() {
            // Changing the emulated size does not change Solo's layout, so it
            // can happen while painting.
            let view = view.downgrade();
            window.defer(cx, move |_, cx| {
                view.update(cx, |view, _| view.apply_viewport(size)).ok();
            });
        }
    }
}

impl Item for BrowserView {
    type Event = BrowserEvent;

    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        match &self.title {
            Some(title) => title.clone().into(),
            None if !self.url.is_empty() => display_address(&self.url).into(),
            None => tr("New Tab", "新标签页").into(),
        }
    }

    fn tab_icon(&self, _: &Window, _: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Public))
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        (!self.url.is_empty()).then(|| self.url.clone().into())
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            BrowserEvent::TitleChanged => f(ItemEvent::UpdateTab),
        }
    }

    fn tab_content(&self, params: TabContentParams, _: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn include_in_nav_history() -> bool {
        false
    }
}

impl SerializableItem for BrowserView {
    fn serialized_item_kind() -> &'static str {
        "BrowserView"
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let db = BrowserDb::global(cx);
        workspace::delete_unloaded_items(alive_items, workspace_id, "browser_tabs", &db, cx)
    }

    fn deserialize(
        _project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Entity<Self>>> {
        let db = BrowserDb::global(cx);
        window.spawn(cx, async move |cx| {
            let url = restored_address(db.get_url(item_id, workspace_id)?);
            cx.update(|window, cx| cx.new(|cx| BrowserView::new(url, workspace, window, cx)))
        })
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        _closing: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<anyhow::Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let url = self.url.clone();
        let db = BrowserDb::global(cx);
        Some(cx.background_spawn(async move { db.save_url(item_id, workspace_id, url).await }))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        matches!(event, BrowserEvent::TitleChanged)
    }
}

/// An empty tab is saved with an empty address, and navigating to that fails:
/// it comes back as an empty tab again.
fn restored_address(saved: Option<String>) -> Option<String> {
    saved.filter(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use super::restored_address;

    #[test]
    fn empty_tabs_are_restored_without_an_address() {
        assert_eq!(restored_address(Some(String::new())), None);
        assert_eq!(restored_address(None), None);
        assert_eq!(
            restored_address(Some("http://localhost:5173/".into())),
            Some("http://localhost:5173/".into())
        );
    }
}
