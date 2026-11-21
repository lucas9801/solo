//! SOLO: the small menu a click on a terminal link shows when Solo shouldn't
//! just open it: an external web address, a file Solo can't open, a file
//! name several files have and which one is a guess, a file only another
//! project tab has (`terminal.link_menu`, "auto"). Ctrl+click does the
//! menu's first item instead.

use crate::terminal_path_like_target::PathLinkResolution;
use settings::TerminalLinkMenu;
use std::path::{Path, PathBuf};
use workspace::path_link::{OpenTarget, OtherTabTarget, ResolvedLink};

/// How many files of a name the menu lists before "show all".
pub(crate) const MAX_LISTED_CANDIDATES: usize = 8;
/// How many characters of a path fit a menu row (in the monospace font, at
/// the menu's widest).
const MAX_PATH_CHARS: usize = 52;

/// What was clicked, as far as deciding about a menu goes.
#[derive(Clone, Debug)]
pub(crate) enum ClickedLink {
    Url {
        url: String,
        /// A web address on this computer (`localhost`, `127.0.0.1`).
        local: bool,
    },
    /// What a path link resolved to, and for a single file, whether Solo
    /// opens it (text or an image), checked the way opening it does.
    Path {
        resolution: PathLinkResolution,
        solo_can_open: bool,
    },
}

/// One row of the link menu.
#[derive(Clone, Debug)]
pub(crate) enum LinkMenuItem {
    OpenInBrowser(String),
    OpenInSolo(String),
    CopyLink(String),
    /// Opens the file (or folder) in Solo.
    Open(OpenTarget),
    OpenWithDefaultProgram(PathBuf),
    /// Opening a program or a file that starts one (`.exe`, `.bat`, `.lnk`,
    /// ...) runs it: never the first row, so Ctrl+click never runs it.
    RunWithDefaultProgram(PathBuf),
    RevealInFileManager(PathBuf),
    CopyPath(PathBuf),
    /// One of several files of the link's name, with its folder relative to
    /// the worktree, for the row.
    Candidate {
        target: OpenTarget,
        folder: String,
    },
    /// The file in another project tab: switches to it and opens the file.
    InOtherTab(OtherTabTarget),
    /// Opens the file finder searching for the name, for all of them.
    ShowAll { file_name: String, count: usize },
    Separator,
}

impl LinkMenuItem {
    /// The row's text, in the interface's language.
    pub(crate) fn label(&self) -> String {
        let tr = util::i18n::tr;
        match self {
            Self::OpenInBrowser(_) => tr("Open in Browser", "在浏览器中打开").into(),
            Self::OpenInSolo(_) => tr("Open in Solo", "在 Solo 中打开").into(),
            Self::CopyLink(_) => tr("Copy Link", "复制链接").into(),
            Self::Open(_) => tr("Open", "打开").into(),
            Self::OpenWithDefaultProgram(_) => {
                tr("Open with Default Program", "用默认程序打开").into()
            }
            Self::RunWithDefaultProgram(_) => {
                tr("Run It (Open with Default Program)", "运行（用默认程序打开）").into()
            }
            Self::RevealInFileManager(_) => tr(
                "Reveal in File Explorer",
                "在文件资源管理器中显示",
            )
            .into(),
            Self::CopyPath(_) => tr("Copy Path", "复制路径").into(),
            Self::Candidate { folder, .. } => middle_ellipsis(folder, MAX_PATH_CHARS),
            Self::InOtherTab(target) => middle_ellipsis(
                &if target.relative_folder.is_empty() {
                    target.tab_name.clone()
                } else {
                    format!("{}/{}", target.tab_name, target.relative_folder)
                },
                MAX_PATH_CHARS,
            ),
            Self::ShowAll { count, .. } => {
                if util::i18n::is_chinese() {
                    format!("在文件查找里看全部 {count} 个")
                } else {
                    format!("Show All {count} in File Finder")
                }
            }
            Self::Separator => String::new(),
        }
    }

    /// Whether the row shows a path (in the monospace font).
    pub(crate) fn is_path(&self) -> bool {
        matches!(self, Self::Candidate { .. } | Self::InOtherTab(_))
    }
}

/// `text` shortened to `max` characters by leaving out its middle.
pub(crate) fn middle_ellipsis(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep / 2;
    let tail = keep - head;
    let start = text.chars().take(head).collect::<String>();
    let end = text.chars().skip(count - tail).collect::<String>();
    format!("{start}…{end}")
}

/// What a click on `link` does: the menu's rows, or none to open it as
/// before. Ctrl+click does the first row.
pub(crate) fn menu_items(link: &ClickedLink, setting: TerminalLinkMenu) -> Vec<LinkMenuItem> {
    use LinkMenuItem::*;
    if setting == TerminalLinkMenu::Never {
        return Vec::new();
    }
    let always = setting == TerminalLinkMenu::Always;
    match link {
        ClickedLink::Url { url, local } => {
            if *local && !always {
                return Vec::new();
            }
            let (first, second) = if *local {
                (OpenInSolo(url.clone()), OpenInBrowser(url.clone()))
            } else {
                (OpenInBrowser(url.clone()), OpenInSolo(url.clone()))
            };
            vec![first, second, Separator, CopyLink(url.clone())]
        }
        ClickedLink::Path {
            resolution,
            solo_can_open,
        } => match resolution {
            PathLinkResolution::Here(link) if link.ambiguous && link.candidates.len() > 1 => {
                candidate_items(link)
            }
            PathLinkResolution::Here(link) => {
                let target = &link.target;
                let path = target.path().path.clone();
                if target.is_file() && runs_when_opened(&path) {
                    vec![
                        RevealInFileManager(path.clone()),
                        CopyPath(path.clone()),
                        Separator,
                        RunWithDefaultProgram(path),
                    ]
                } else if target.is_file() && !solo_can_open {
                    vec![
                        OpenWithDefaultProgram(path.clone()),
                        RevealInFileManager(path.clone()),
                        Separator,
                        CopyPath(path),
                    ]
                } else if always {
                    vec![
                        Open(target.clone()),
                        RevealInFileManager(path.clone()),
                        Separator,
                        CopyPath(path),
                    ]
                } else {
                    Vec::new()
                }
            }
            PathLinkResolution::InOtherTabs(found) => found
                .iter()
                .take(MAX_LISTED_CANDIDATES)
                .cloned()
                .map(InOtherTab)
                .collect(),
        },
    }
}

/// The rows for several files of a link's name, best first.
fn candidate_items(link: &ResolvedLink) -> Vec<LinkMenuItem> {
    let mut items = link
        .candidates
        .iter()
        .take(MAX_LISTED_CANDIDATES)
        .map(|candidate| LinkMenuItem::Candidate {
            target: candidate.target.clone(),
            folder: candidate_folder(&candidate.target),
        })
        .collect::<Vec<_>>();
    if link.candidates.len() > MAX_LISTED_CANDIDATES {
        let file_name = link
            .target
            .path()
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        items.push(LinkMenuItem::Separator);
        items.push(LinkMenuItem::ShowAll {
            file_name,
            count: link.candidates.len(),
        });
    }
    items
}

/// The folder of a candidate, relative to its worktree when known.
fn candidate_folder(target: &OpenTarget) -> String {
    match target {
        OpenTarget::Worktree(_, entry, ..) => entry
            .path
            .parent()
            .map(|folder| folder.as_unix_str().to_string())
            .unwrap_or_default(),
        OpenTarget::Path(path, ..) => path
            .path
            .parent()
            .map(|folder| folder.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// Extensions Windows runs rather than opens, on top of `PATHEXT`.
const RUNNABLE_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi",
    "msp", "mst", "scr", "cpl", "lnk", "url", "hta", "pif", "reg", "jar", "appref-ms", "application",
    "gadget", "msc", "scf", "settingcontent-ms", "library-ms", "diagcab", "appx",
    "appxbundle", "msix", "msixbundle",
];

/// Whether opening `path` with its default program would run something:
/// a program, a script, a shortcut. Those are never opened by Ctrl+click.
pub(crate) fn runs_when_opened(path: &Path) -> bool {
    let Some(extension) = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) else {
        return false;
    };
    RUNNABLE_EXTENSIONS.contains(&extension.as_str())
        || std::env::var("PATHEXT").is_ok_and(|pathext| {
            pathext
                .split(';')
                .filter_map(|entry| entry.trim().strip_prefix('.'))
                .any(|entry| entry.eq_ignore_ascii_case(&extension))
        })
}

/// Whether a web address is on this computer.
pub(crate) fn is_local_url(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if url.scheme() == "file" {
        return true;
    }
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// Whether Solo opens the file at `path` itself, by the rule opening it
/// uses: images by their extension (the image viewer), everything else
/// unless its first bytes are binary (the editor refuses those, "Binary files
/// are not supported"). A file that can't be read is left to opening,
/// which says why.
pub(crate) fn solo_can_open(path: &Path, file: Option<&mut dyn std::io::Read>) -> bool {
    let is_image = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .is_some_and(|extension| {
            gpui::Img::extensions().contains(&extension.as_str()) && !extension.contains("svg")
        });
    if is_image {
        return true;
    }
    let Some(file) = file else {
        return true;
    };
    match file_content::read_byte_header(file) {
        Ok((header, _)) => {
            file_content::decode_byte_header(&header).1 != file_content::ByteContent::Binary
        }
        Err(_) => true,
    }
}

/// The menu's icon for `item`.
fn icon_for(item: &LinkMenuItem) -> ui::IconName {
    use ui::IconName;
    match item {
        LinkMenuItem::OpenInBrowser(_) | LinkMenuItem::OpenWithDefaultProgram(_) => {
            IconName::ArrowUpRight
        }
        LinkMenuItem::RunWithDefaultProgram(_) => IconName::Warning,
        LinkMenuItem::OpenInSolo(_) => IconName::Public,
        LinkMenuItem::CopyLink(_) | LinkMenuItem::CopyPath(_) => IconName::Copy,
        LinkMenuItem::RevealInFileManager(_) => IconName::Folder,
        LinkMenuItem::Open(_) | LinkMenuItem::Candidate { .. } | LinkMenuItem::InOtherTab(_) => {
            IconName::FileCode
        }
        LinkMenuItem::ShowAll { .. } => IconName::MagnifyingGlass,
        LinkMenuItem::Separator => IconName::Dash,
    }
}

mod view {
    use super::*;
    use crate::TerminalView;
    use crate::terminal_path_like_target::{
        LinkPurpose, open_path_like_target, resolve_path_like_target,
    };
    use editor::items::open_resolved_target;
    use gpui::{
        App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, Focusable as _,
        Pixels, Point, Task, TaskExt as _, WeakEntity, Window, px,
    };
    use settings::Settings as _;
    use terminal::{PathLikeTarget, terminal_settings::TerminalSettings};
    use ui::{ContextMenu, prelude::*};
    use workspace::Workspace;

    /// The menu's narrowest and widest rows (its padding comes on top).
    const MIN_ROW_WIDTH: f32 = 296.;
    const MAX_ROW_WIDTH: f32 = 396.;

    impl TerminalView {
        /// SOLO: a click on a web address in the terminal.
        pub(crate) fn open_url_link(
            &mut self,
            url: String,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let setting = TerminalSettings::get_global(cx).link_menu;
            // A web address clicked meanwhile wins over a path still being
            // resolved.
            self.link_menu_task = Task::ready(());
            let modifiers = window.modifiers();
            // Ctrl+Shift+click opens it the other way, as before.
            if modifiers.control && modifiers.shift {
                Workspace::open_web_link(&url, window, cx);
                return;
            }
            let link = ClickedLink::Url {
                local: is_local_url(&url),
                url: url.clone(),
            };
            let items = menu_items(&link, setting);
            if items.is_empty() {
                Workspace::open_web_link(&url, window, cx);
            } else if modifiers.secondary(cx) {
                self.perform_first_link_item(items, window, cx);
            } else {
                self.deploy_link_menu(items, window, cx);
            }
        }

        /// SOLO: a click on a path in the terminal.
        pub(crate) fn open_path_link(
            &mut self,
            target: PathLikeTarget,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let setting = TerminalSettings::get_global(cx).link_menu;
            let workspace = self.workspace.clone();
            if setting == TerminalLinkMenu::Never {
                open_path_like_target(&workspace, self, &target, window, cx);
                return;
            }
            let secondary = window.modifiers().secondary(cx);
            let resolution = resolve_path_like_target(
                &workspace,
                &target,
                LinkPurpose::Click,
                cx,
                #[cfg(test)]
                workspace::path_link::BackgroundPathChecks::LocalFileSystem,
            );
            // A key pressed (or a scroll, or another click) while the link is
            // resolved cancels it: what the user does next isn't interrupted
            // by a menu or a file opening.
            self.link_menu_keystrokes = Some(cx.observe_keystrokes(|this, _, _, _| {
                this.link_menu_task = Task::ready(());
                this.link_menu_keystrokes = None;
            }));
            self.link_menu_task = cx.spawn_in(window, async move |this, cx| {
                let Some(resolution) = resolution.await else {
                    this.update(cx, |this, _| this.link_menu_keystrokes = None).ok();
                    return;
                };
                let single_file = match &resolution {
                    PathLinkResolution::Here(link) if link.target.is_file() => {
                        Some(link.target.path().path.clone())
                    }
                    _ => None,
                };
                let fs = this
                    .update(cx, |this, cx| {
                        this.project
                            .upgrade()
                            .filter(|project| project.read(cx).is_local())
                            .map(|project| project.read(cx).fs().clone())
                    })
                    .ok()
                    .flatten();
                let solo_can_open = match (single_file, fs) {
                    (Some(path), Some(fs)) => {
                        cx.background_spawn(async move {
                            let mut file = fs.open_sync(&path).await.ok();
                            solo_can_open(
                                &path,
                                file.as_mut().map(|file| file as &mut dyn std::io::Read),
                            )
                        })
                        .await
                    }
                    _ => true,
                };
                let link = ClickedLink::Path {
                    resolution,
                    solo_can_open,
                };
                let items = menu_items(&link, setting);
                this.update_in(cx, |this, window, cx| {
                    this.link_menu_keystrokes = None;
                    if items.is_empty() {
                        if let ClickedLink::Path {
                            resolution: PathLinkResolution::Here(link),
                            ..
                        } = link
                        {
                            this.perform_link_item(LinkMenuItem::Open(link.target), window, cx);
                        }
                    } else if secondary {
                        this.perform_first_link_item(items, window, cx);
                    } else {
                        this.deploy_link_menu(items, window, cx);
                    }
                })
                .ok();
            });
        }

        fn perform_first_link_item(
            &mut self,
            items: Vec<LinkMenuItem>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            if let Some(first) = items
                .into_iter()
                .find(|item| !matches!(item, LinkMenuItem::Separator))
            {
                self.perform_link_item(first, window, cx);
            }
        }

        /// Does what a row of the link menu says.
        pub(crate) fn perform_link_item(
            &mut self,
            item: LinkMenuItem,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let workspace = self.workspace.clone();
            #[cfg(test)]
            self.performed_link_items.push(item.clone());
            match item {
                LinkMenuItem::OpenInBrowser(url) => cx.open_url(&url),
                LinkMenuItem::OpenInSolo(url) => {
                    window.dispatch_action(
                        Box::new(solo_actions::browser::OpenUrl {
                            url,
                            in_solo: Some(true),
                            ..Default::default()
                        }),
                        cx,
                    );
                }
                LinkMenuItem::CopyLink(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                LinkMenuItem::CopyPath(path) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ));
                }
                LinkMenuItem::OpenWithDefaultProgram(path)
                | LinkMenuItem::RunWithDefaultProgram(path) => cx.open_with_system(&path),
                LinkMenuItem::RevealInFileManager(path) => cx.reveal_path(&path),
                LinkMenuItem::Open(target) | LinkMenuItem::Candidate { target, .. } => {
                    open_target_in(workspace, target, window, cx);
                }
                LinkMenuItem::InOtherTab(found) => {
                    let Some(other) = found.workspace.upgrade() else {
                        return;
                    };
                    let multi_workspace = workspace.upgrade().and_then(|workspace| {
                        workspace
                            .read(cx)
                            .multi_workspace()
                            .and_then(|multi_workspace| multi_workspace.upgrade())
                    });
                    if let Some(multi_workspace) = multi_workspace {
                        window.defer(cx, move |window, cx| {
                            multi_workspace.update(cx, |multi_workspace, cx| {
                                multi_workspace.activate(other, None, window, cx)
                            });
                        });
                    }
                    open_target_in(found.workspace.clone(), found.target, window, cx);
                }
                LinkMenuItem::ShowAll { file_name, .. } => {
                    if let Some(workspace) = workspace.upgrade() {
                        let action = workspace::ToggleFileFinder {
                            query: Some(file_name),
                            ..Default::default()
                        };
                        let focus = workspace.read(cx).focus_handle(cx);
                        window.defer(cx, move |window, cx| {
                            focus.dispatch_action(&action, window, cx)
                        });
                    }
                }
                LinkMenuItem::Separator => {}
            }
        }

        /// Where the link menu goes: 4 pixels below the clicked link, at
        /// its start, where the link is now. `None` when it's no longer on
        /// the screen (the output scrolled).
        fn link_menu_position(&self, cx: &App) -> Option<Point<Pixels>> {
            let terminal = self.terminal.read(cx);
            let content = terminal.last_content();
            let bounds = content.terminal_bounds;
            let Some(word) = terminal.opened_word() else {
                return Some(bounds.bounds.origin);
            };
            let display_offset = content.display_offset as i32;
            let start = word.word_match.start();
            let end = word.word_match.end();
            let column = if start.line == end.line {
                start.column
            } else {
                0
            };
            let row = end.line + display_offset;
            let rows = (bounds.bounds.size.height / bounds.line_height).floor() as i32;
            if row < 0 || (rows > 0 && row >= rows) {
                return None;
            }
            Some(Point {
                x: bounds.bounds.origin.x + bounds.cell_width * column as f32,
                y: bounds.bounds.origin.y + bounds.line_height * (row + 1) as f32 + px(4.),
            })
        }

        /// Shows the link menu with `items` under the clicked link.
        pub(crate) fn deploy_link_menu(
            &mut self,
            items: Vec<LinkMenuItem>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let Some(position) = self.link_menu_position(cx) else {
                return;
            };
            #[cfg(test)]
            {
                self.shown_link_menu = Some(items.clone());
            }
            let this = cx.entity().downgrade();
            let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
                for item in items {
                    if matches!(item, LinkMenuItem::Separator) {
                        menu = menu.separator();
                        continue;
                    }
                    let label = item.label();
                    let icon = icon_for(&item);
                    let is_path = item.is_path();
                    let this = this.clone();
                    menu = menu.custom_entry(
                        move |_, cx| {
                            let label = Label::new(label.clone()).single_line();
                            h_flex()
                                .gap_2()
                                .min_w(px(MIN_ROW_WIDTH))
                                .max_w(px(MAX_ROW_WIDTH))
                                .child(
                                    Icon::new(icon)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(if is_path { label.buffer_font(cx) } else { label })
                                .into_any_element()
                        },
                        move |window, cx| {
                            let item = item.clone();
                            this.update(cx, |this, cx| this.perform_link_item(item, window, cx))
                                .ok();
                        },
                    );
                }
                menu
            });
            self.show_link_menu(menu, position, window, cx);
        }

        fn show_link_menu(
            &mut self,
            menu: Entity<ContextMenu>,
            position: Point<Pixels>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            window.focus(&menu.focus_handle(cx), cx);
            let subscription = cx.subscribe_in(
                &menu,
                window,
                |this, _, _: &DismissEvent, window, cx| {
                    if this.context_menu.as_ref().is_some_and(|context_menu| {
                        context_menu.0.focus_handle(cx).contains_focused(window, cx)
                    }) {
                        cx.focus_self(window);
                    }
                    this.context_menu.take();
                    this.link_menu_keystrokes = None;
                    cx.notify();
                },
            );
            // Typing anything but the menu's keys closes it.
            self.link_menu_keystrokes = Some(cx.observe_keystrokes(|this, event, window, cx| {
                let key = event.keystroke.key.as_str();
                if !matches!(
                    key,
                    "up" | "down" | "enter" | "escape" | "home" | "end" | "pageup" | "pagedown"
                ) {
                    this.dismiss_link_menu(window, cx);
                }
            }));
            self.context_menu = Some((menu, position, subscription));
            cx.notify();
        }

        /// Closes the link menu, or cancels resolving a clicked link
        /// (scrolling, typing).
        pub(crate) fn dismiss_link_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            self.link_menu_task = Task::ready(());
            if self.link_menu_keystrokes.take().is_some() {
                if let Some((menu, _, _)) = self.context_menu.take()
                    && menu.focus_handle(cx).contains_focused(window, cx)
                {
                    cx.focus_self(window);
                }
                cx.notify();
            }
        }
    }

    fn open_target_in(
        workspace: WeakEntity<Workspace>,
        target: OpenTarget,
        window: &mut Window,
        cx: &mut Context<TerminalView>,
    ) {
        let task: Task<anyhow::Result<bool>> = cx.spawn_in(window, async move |_, cx| {
            open_resolved_target(&workspace, &target, cx).await
        });
        task.detach_and_log_err(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use util::paths::PathWithPosition;
    use workspace::path_link::{CandidateRank, LinkCandidate};

    fn path_target(path: &str) -> OpenTarget {
        OpenTarget::Path(
            PathWithPosition::from_path(PathBuf::from(path)),
            false,
            workspace::path_link::OpenTargetFoundBy::BackgroundPathResolution,
        )
    }

    fn labels(items: &[LinkMenuItem]) -> Vec<String> {
        items.iter().map(LinkMenuItem::label).collect()
    }

    fn file(path: &str, solo_can_open: bool) -> ClickedLink {
        ClickedLink::Path {
            resolution: PathLinkResolution::Here(ResolvedLink {
                target: path_target(path),
                candidates: vec![LinkCandidate {
                    target: path_target(path),
                    rank: CandidateRank::default(),
                }],
                ambiguous: false,
            }),
            solo_can_open,
        }
    }

    #[test]
    fn menus_for_the_four_cases_and_none_for_the_rest() {
        use TerminalLinkMenu::*;
        let external = ClickedLink::Url {
            url: "https://docs.unity.cn/a.html".into(),
            local: false,
        };
        assert_eq!(
            labels(&menu_items(&external, Auto)),
            ["Open in Browser", "Open in Solo", "", "Copy Link"]
        );
        let local = ClickedLink::Url {
            url: "http://localhost:5173/".into(),
            local: true,
        };
        assert!(menu_items(&local, Auto).is_empty());
        assert_eq!(
            labels(&menu_items(&local, Always)),
            ["Open in Solo", "Open in Browser", "", "Copy Link"]
        );

        let apk = file("/work/PerfCmp.apk", false);
        assert_eq!(
            labels(&menu_items(&apk, Auto)),
            [
                "Open with Default Program",
                "Reveal in File Explorer",
                "",
                "Copy Path"
            ]
        );
        let source = file("/work/main.cpp", true);
        assert!(menu_items(&source, Auto).is_empty());
        assert_eq!(
            labels(&menu_items(&source, Always))[0],
            "Open"
        );

        // "Never": no menu for anything, as before.
        for link in [&external, &local, &apk, &source] {
            assert!(menu_items(link, Never).is_empty());
        }
    }

    #[test]
    fn same_name_files_list_their_folders_and_show_all_past_eight() {
        let candidates = (0..10)
            .map(|ix| LinkCandidate {
                target: path_target(&format!("/Engine/Module{ix}/Public/Array.h")),
                rank: CandidateRank::default(),
            })
            .collect::<Vec<_>>();
        let link = ClickedLink::Path {
            resolution: PathLinkResolution::Here(ResolvedLink {
                target: candidates[0].target.clone(),
                candidates,
                ambiguous: true,
            }),
            solo_can_open: true,
        };
        let items = menu_items(&link, TerminalLinkMenu::Auto);
        let labels = labels(&items);
        assert_eq!(labels.len(), 10);
        assert!(labels[0].ends_with("Module0/Public"), "{}", labels[0]);
        assert_eq!(labels[9], "Show All 10 in File Finder");
        assert!(matches!(
            &items[9],
            LinkMenuItem::ShowAll { file_name, count: 10 } if file_name == "Array.h"
        ));
        assert!(items[0].is_path());
    }

    #[test]
    fn long_paths_lose_their_middle() {
        assert_eq!(middle_ellipsis("short", 10), "short");
        let shortened = middle_ellipsis("Extras/ThirdPartyNotUE/libimobiledevice/src/plist/include", 20);
        assert_eq!(shortened.chars().count(), 20);
        assert!(shortened.starts_with("Extras/Th"));
        assert!(shortened.ends_with("t/include"));
        assert!(shortened.contains('…'));
    }

    #[test]
    fn local_web_addresses() {
        assert!(is_local_url("http://localhost:5173/"));
        assert!(is_local_url("http://127.0.0.1:8080/x"));
        assert!(is_local_url("http://app.localhost/"));
        assert!(!is_local_url("https://github.com/lucas9801/solo"));
        assert!(!is_local_url("not a url"));
    }

    #[test]
    fn what_solo_opens() {
        let folder = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = folder.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let solo_can_open = |path: PathBuf| {
            let mut file = std::fs::File::open(&path).unwrap();
            super::solo_can_open(&path, Some(&mut file))
        };
        assert!(solo_can_open(write("main.cpp", b"int main() {}\n")));
        assert!(solo_can_open(write("notes.txt", "中文".as_bytes())));
        assert!(!solo_can_open(write(
            "PerfCmp.apk",
            b"PK\x03\x04\x00\x00\x00\x00\x08\x00\x00\x00\x00\x00\x00\x00"
        )));
        assert!(!solo_can_open(write("KernelBase.dll", b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00")));
        // An image opens in the image viewer, whatever its bytes.
        assert!(solo_can_open(write("shot.png", b"\x89PNG\r\n\x1a\n\x00\x00\x00")));
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::TerminalView;
    use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, UpdateGlobal as _, VisualTestContext};
    use project::Project;
    use serde_json::json;
    use settings::SettingsStore;
    use std::path::Path;
    use terminal::{
        PathLikeTarget, TerminalBuilder,
        terminal_settings::{AlternateScroll, CursorShape},
    };
    use util::{path, paths::PathStyle};
    use workspace::{AppState, MultiWorkspace};

    async fn view_in(
        cx: &mut TestAppContext,
        tree: serde_json::Value,
    ) -> (Entity<TerminalView>, VisualTestContext) {
        let fs = cx.update(AppState::test).fs.as_fake().clone();
        cx.update(|cx| {
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        fs.insert_tree(path!("/Engine"), tree).await;
        let project = Project::test(fs, [Path::new(path!("/Engine"))], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        let terminal = cx.new(|cx| {
            TerminalBuilder::new_display_only(
                CursorShape::default(),
                AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                PathStyle::local(),
            )
            .subscribe(cx)
        });
        let view = cx.update(|window, cx| {
            cx.new(|cx| {
                TerminalView::new(
                    terminal,
                    workspace.downgrade(),
                    None,
                    project.downgrade(),
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        (view, cx.clone())
    }

    fn set_link_menu(cx: &mut VisualTestContext, value: TerminalLinkMenu) {
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.terminal.get_or_insert_default().link_menu = Some(value);
                });
            });
        });
    }

    fn engine() -> serde_json::Value {
        json!({
            ".git": {},
            "Source": {
                "Runtime": { "Core": { "Containers": { "Array.h": "x\n".repeat(900) } } },
                "ThirdParty": { "Eigen": { "Array.h": "x\n".repeat(900) } },
            },
            "Extras": { "plist": { "Array.h": "x\n".repeat(10) } },
            "Binaries": { "PerfCmp.apk": "PK\u{3}\u{4}\u{0}\u{0}\u{0}\u{0}\u{8}\u{0}\u{0}\u{0}" },
            "main.cpp": "int main() {}\n",
        })
    }

    fn target(maybe_path: &str) -> PathLikeTarget {
        PathLikeTarget {
            maybe_path: maybe_path.to_string(),
            working_directory: Some(path!("/Engine").into()),
            context_paths: Vec::new(),
            position_files: Vec::new(),
        }
    }

    fn click_path(view: &Entity<TerminalView>, cx: &mut VisualTestContext, maybe_path: &str) {
        view.update_in(cx, |view, window, cx| {
            view.shown_link_menu = None;
            view.performed_link_items.clear();
            view.open_path_link(target(maybe_path), window, cx)
        });
        cx.run_until_parked();
    }

    fn shown(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Option<Vec<String>> {
        view.read_with(cx, |view, _| {
            view.shown_link_menu
                .as_ref()
                .map(|items| items.iter().map(LinkMenuItem::label).collect())
        })
    }

    fn performed(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Vec<String> {
        view.read_with(cx, |view, _| {
            view.performed_link_items
                .iter()
                .map(|item| match item {
                    LinkMenuItem::Open(target) | LinkMenuItem::Candidate { target, .. } => {
                        target.path().path.to_string_lossy().replace('\\', "/")
                    }
                    other => other.label(),
                })
                .collect()
        })
    }

    #[gpui::test]
    async fn clicks_show_menus_by_the_setting(cx: &mut TestAppContext) {
        let (view, mut cx) = view_in(cx, engine()).await;
        let cx = &mut cx;

        // Default ("auto"): a guess between files of a name asks.
        click_path(&view, cx, "Array.h:883");
        let menu = shown(&view, cx).expect("a menu for Array.h");
        assert_eq!(menu.len(), 3, "{menu:?}");
        assert!(menu[0].ends_with("Source/Runtime/Core/Containers"), "{menu:?}");
        assert!(performed(&view, cx).is_empty());

        // A file Solo can't open asks too.
        click_path(&view, cx, "Binaries/PerfCmp.apk");
        assert_eq!(
            shown(&view, cx).unwrap(),
            [
                "Open with Default Program",
                "Reveal in File Explorer",
                "",
                "Copy Path"
            ]
        );

        // A single source file opens right away.
        click_path(&view, cx, "main.cpp");
        assert_eq!(shown(&view, cx), None);
        assert_eq!(performed(&view, cx).len(), 1);
        assert!(performed(&view, cx)[0].ends_with("/Engine/main.cpp"));

        // Ctrl+click: the menu's first item, without the menu.
        cx.simulate_modifiers_change(Modifiers::control());
        click_path(&view, cx, "Array.h:883");
        assert_eq!(shown(&view, cx), None);
        let opened = performed(&view, cx);
        assert_eq!(opened.len(), 1);
        assert!(
            opened[0].ends_with("Source/Runtime/Core/Containers/Array.h"),
            "{opened:?}"
        );
        click_path(&view, cx, "Binaries/PerfCmp.apk");
        assert_eq!(performed(&view, cx), ["Open with Default Program"]);
        cx.simulate_modifiers_change(Modifiers::none());

        // "always": a menu for every link.
        set_link_menu(cx, TerminalLinkMenu::Always);
        click_path(&view, cx, "main.cpp");
        assert_eq!(shown(&view, cx).unwrap()[0], "Open");

        // "never": no menu at all, as before.
        set_link_menu(cx, TerminalLinkMenu::Never);
        for link in ["Array.h:883", "Binaries/PerfCmp.apk", "main.cpp"] {
            click_path(&view, cx, link);
            assert_eq!(shown(&view, cx), None, "{link}");
        }
    }

    #[gpui::test]
    async fn programs_are_never_run_by_a_click(cx: &mut TestAppContext) {
        let mut tree = engine();
        tree["tools"] = json!({
            "setup.exe": "MZ\u{90}\u{0}\u{3}\u{0}\u{0}\u{0}\u{4}\u{0}",
            "build.bat": "@echo off\n",
            "Solo.lnk": "L\u{0}\u{0}\u{0}\u{1}\u{14}\u{2}\u{0}",
        });
        let (view, mut cx) = view_in(cx, tree).await;
        let cx = &mut cx;
        for program in ["tools/setup.exe", "tools/build.bat", "tools/Solo.lnk"] {
            // The menu: showing it first, running it last and said so.
            click_path(&view, cx, program);
            assert_eq!(
                shown(&view, cx).unwrap(),
                [
                    "Reveal in File Explorer",
                    "Copy Path",
                    "",
                    "Run It (Open with Default Program)"
                ],
                "{program}"
            );
            // Ctrl+click: shown in File Explorer, not run.
            cx.simulate_modifiers_change(Modifiers::control());
            click_path(&view, cx, program);
            assert_eq!(performed(&view, cx), ["Reveal in File Explorer"], "{program}");
            cx.simulate_modifiers_change(Modifiers::none());
        }
        assert!(runs_when_opened(Path::new("x.EXE")));
        assert!(runs_when_opened(Path::new("a.ps1")));
        assert!(!runs_when_opened(Path::new("PerfCmp.apk")));
        assert!(!runs_when_opened(Path::new("KernelBase.dll")));
    }

    #[gpui::test]
    async fn typing_or_scrolling_cancels_a_pending_click(cx: &mut TestAppContext) {
        let (view, mut cx) = view_in(cx, engine()).await;
        let cx = &mut cx;
        // A key right after the click: no menu, nothing opened.
        view.update_in(cx, |view, window, cx| {
            view.open_path_link(target("Array.h:883"), window, cx)
        });
        cx.simulate_keystrokes("a");
        cx.run_until_parked();
        assert_eq!(shown(&view, cx), None);
        assert!(performed(&view, cx).is_empty());
        // A scroll right after the click: the same.
        view.update_in(cx, |view, window, cx| {
            view.open_path_link(target("main.cpp"), window, cx);
            view.dismiss_link_menu(window, cx);
        });
        cx.run_until_parked();
        assert_eq!(shown(&view, cx), None);
        assert!(performed(&view, cx).is_empty());
    }

    #[gpui::test]
    async fn web_addresses(cx: &mut TestAppContext) {
        let (view, mut cx) = view_in(cx, engine()).await;
        let cx = &mut cx;
        let click = |view: &Entity<TerminalView>, cx: &mut VisualTestContext, url: &str| {
            view.update_in(cx, |view, window, cx| {
                view.shown_link_menu = None;
                view.performed_link_items.clear();
                view.open_url_link(url.to_string(), window, cx)
            });
            cx.run_until_parked();
        };
        click(&view, cx, "https://docs.unity.cn/manual.html");
        assert_eq!(
            shown(&view, cx).unwrap(),
            ["Open in Browser", "Open in Solo", "", "Copy Link"]
        );
        click(&view, cx, "http://localhost:5173/");
        assert_eq!(shown(&view, cx), None);
        cx.simulate_modifiers_change(Modifiers::control());
        click(&view, cx, "https://docs.unity.cn/manual.html");
        assert_eq!(shown(&view, cx), None);
        assert_eq!(performed(&view, cx), ["Open in Browser"]);
        cx.simulate_modifiers_change(Modifiers::none());
        set_link_menu(cx, TerminalLinkMenu::Never);
        click(&view, cx, "https://docs.unity.cn/manual.html");
        assert_eq!(shown(&view, cx), None);
        assert!(performed(&view, cx).is_empty());
    }

    #[gpui::test]
    async fn menu_keys_and_typing(cx: &mut TestAppContext) {
        let (view, mut cx) = view_in(cx, engine()).await;
        let cx = &mut cx;
        click_path(&view, cx, "Array.h:883");
        assert!(view.read_with(cx, |view, _| view.context_menu.is_some()));
        // Nothing is selected at first: Down selects the first file, Down
        // again the second; Enter opens it. (The menu's own key bindings
        // send these; the view isn't drawn in this test.)
        let menu = view.read_with(cx, |view, _| view.context_menu.as_ref().unwrap().0.clone());
        menu.update_in(cx, |menu, window, cx| {
            menu.select_next(&menu::SelectNext, window, cx);
            menu.select_next(&menu::SelectNext, window, cx);
            menu.confirm(&menu::Confirm, window, cx);
        });
        cx.run_until_parked();
        let opened = performed(&view, cx);
        assert_eq!(opened.len(), 1, "{opened:?}");
        assert!(opened[0].ends_with("ThirdParty/Eigen/Array.h"), "{opened:?}");
        assert!(view.read_with(cx, |view, _| view.context_menu.is_none()));

        // Escape closes it; typing closes it.
        click_path(&view, cx, "Array.h:883");
        let menu = view.read_with(cx, |view, _| view.context_menu.as_ref().unwrap().0.clone());
        menu.update_in(cx, |menu, window, cx| menu.cancel(&menu::Cancel, window, cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.context_menu.is_none()));
        click_path(&view, cx, "Array.h:883");
        cx.simulate_keystrokes("a");
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.context_menu.is_none()));
        assert!(performed(&view, cx).is_empty());
    }
}
