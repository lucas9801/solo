//! SOLO: the update button at the right end of the title bar, for Solo's own
//! updater and the Microsoft Store edition's (`auto_update::update_button`
//! decides what it says). It replaces Zed's `UpdateVersion`, which followed
//! Zed's updater, which never runs in Solo.

use auto_update::update_button::{
    self as model, ButtonIcon, ButtonLabel, ButtonState, MenuItem, UpdateAction, UpdateEntry,
    offer_summary, perform,
};
use gpui::{ClickEvent, CursorStyle, Empty, Entity, FontWeight, Hsla, Render, relative};
use ui::{
    CommonAnimationExt as _, ContextMenu, ContextMenuEntry, IconPosition, PopoverMenu,
    prelude::*,
};

pub struct UpdateButton {
    entry: Entity<UpdateEntry>,
}

impl UpdateButton {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Made by the updater if one runs; otherwise it stays hidden.
        let entry = model::entry_or_init(cx);
        cx.observe(&entry, |_, _, cx| cx.notify()).detach();
        Self { entry }
    }

    /// Steps through the button's states with a made-up update (the
    /// "simulate update available" debug action).
    pub fn simulate(&mut self, cx: &mut Context<Self>) {
        self.entry.update(cx, |entry, cx| entry.simulate_next(cx));
    }
}

fn build_menu(
    entry: &Entity<UpdateEntry>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<ContextMenu>> {
    let chinese = util::i18n::is_chinese();
    let (state, offer) = {
        let entry = entry.read(cx);
        (entry.state().clone(), entry.offer().cloned())
    };
    let summary = offer
        .as_ref()
        .and_then(|offer| offer_summary(offer, chinese));
    let running = auto_update::solo_update::running_program_names(cx);
    let menu = model::update_menu(&state, offer.as_ref(), summary, &running, chinese)?;
    let entry = entry.clone();
    Some(ContextMenu::build(
        window,
        cx,
        move |mut context_menu, _, _| {
            let title = menu.title.clone();
            context_menu = context_menu.custom_row(move |_, _| {
                Label::new(title.clone())
                    .size(LabelSize::Default)
                    .into_any_element()
            });
            if let Some(detail) = menu.detail.clone() {
                let color = if menu.detail_is_warning {
                    Color::Warning
                } else {
                    Color::Muted
                };
                context_menu = context_menu.custom_row(move |_, _| {
                    div()
                        .max_w(rems(18.))
                        .child(
                            Label::new(detail.clone())
                                .size(LabelSize::Small)
                                .color(color),
                        )
                        .into_any_element()
                });
            }
            for item in &menu.items {
                context_menu = match item {
                    MenuItem::Separator => context_menu.separator(),
                    MenuItem::Action(action, label) => {
                        let entry = entry.clone();
                        let action = *action;
                        let mut item = ContextMenuEntry::new(label.clone())
                            .handler(move |window, cx| perform(&entry, action, window, cx));
                        // It opens a web page.
                        if action == UpdateAction::ViewNotes {
                            item = item
                                .icon(IconName::ArrowUpRight)
                                .icon_position(IconPosition::End)
                                .icon_size(IconSize::XSmall)
                                .icon_color(Color::Muted);
                        }
                        context_menu.item(item)
                    }
                };
            }
            context_menu
        },
    ))
}

/// The pill's background and border: `tint` laid over the title bar, a
/// little stronger while hovered or while its menu is open.
fn pill_colors(tint: Hsla, emphasized: bool) -> (Hsla, Hsla) {
    if emphasized {
        (tint.opacity(0.18), tint.opacity(0.40))
    } else {
        (tint.opacity(0.14), tint.opacity(0.28))
    }
}

/// The button: an icon and a label on a rounded pill, with a thin progress
/// bar along its bottom while downloading.
#[derive(IntoElement)]
struct UpdatePill {
    label: ButtonLabel,
    selected: bool,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    cursor_style: CursorStyle,
}

impl UpdatePill {
    fn new(label: ButtonLabel) -> Self {
        Self {
            label,
            selected: false,
            on_click: None,
            cursor_style: CursorStyle::Arrow,
        }
    }
}

impl Clickable for UpdatePill {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self.cursor_style = CursorStyle::PointingHand;
        self
    }

    fn cursor_style(mut self, cursor_style: CursorStyle) -> Self {
        self.cursor_style = cursor_style;
        self
    }
}

impl Toggleable for UpdatePill {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl RenderOnce for UpdatePill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let label = self.label;
        let tint = if label.warning {
            cx.theme().status().warning
        } else {
            cx.theme().colors().text_accent
        };
        let icon = match label.icon {
            ButtonIcon::Download => Icon::new(IconName::Download)
                .size(IconSize::Small)
                .color(Color::Accent)
                .into_any_element(),
            ButtonIcon::Warning => Icon::new(IconName::Warning)
                .size(IconSize::Small)
                .color(Color::Warning)
                .into_any_element(),
            ButtonIcon::Spinner => Icon::new(IconName::LoadCircle)
                .size(IconSize::Small)
                .color(Color::Muted)
                .with_rotate_animation(2)
                .into_any_element(),
            ButtonIcon::Check => Icon::new(IconName::Check)
                .size(IconSize::Small)
                .color(Color::Success)
                .into_any_element(),
        };
        let (background, border) = pill_colors(tint, self.selected);
        let (hover_background, hover_border) = pill_colors(tint, true);
        h_flex()
            .id("solo-update-button")
            .relative()
            .overflow_hidden()
            .h(rems_from_px(22_f32))
            .pl(rems_from_px(8_f32))
            .pr(rems_from_px(10_f32))
            .gap(rems_from_px(6_f32))
            .rounded_full()
            .border_1()
            .border_color(gpui::transparent_black())
            .when(label.filled, |pill| {
                pill.bg(background).border_color(border).when(
                    self.on_click.is_some(),
                    |pill| {
                        pill.hover(move |style| {
                            style.bg(hover_background).border_color(hover_border)
                        })
                    },
                )
            })
            .cursor(self.cursor_style)
            .child(icon)
            .child(
                Label::new(label.text)
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM),
            )
            .when_some(label.progress, |pill, percent| {
                pill.child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .h(px(2.))
                        .w(relative(f32::from(percent.min(100)) / 100.))
                        .bg(tint),
                )
            })
            .when_some(self.on_click, |pill, on_click| {
                pill.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}

impl Render for UpdateButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entry = self.entry.clone();
        let (state, label) = {
            let entry = entry.read(cx);
            let label = model::button_label(entry.state(), entry.offer(), util::i18n::is_chinese());
            (entry.state().clone(), label)
        };
        let Some(label) = label else {
            return Empty.into_any_element();
        };
        let button = UpdatePill::new(label);
        // "Checking" and "up to date" only say something.
        if matches!(state, ButtonState::Checking | ButtonState::UpToDate) {
            return div().mr_1().child(button).into_any_element();
        }
        div()
            .mr_1()
            .child(
                PopoverMenu::new("solo-update-menu")
                    .menu(move |window, cx| build_menu(&entry, window, cx))
                    .trigger(button)
                    .anchor(gpui::Anchor::TopRight),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Rgba;
    use ui::utils::calculate_contrast_ratio;

    /// `key` of the theme named `name` in a bundled theme file.
    fn theme_color(themes: &str, name: &str, key: &str) -> Hsla {
        let themes: serde_json::Value = serde_json::from_str(themes).unwrap();
        let theme = themes["themes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|theme| theme["name"] == name)
            .unwrap_or_else(|| panic!("no theme {name}"));
        let hex = theme["style"][key]
            .as_str()
            .unwrap_or_else(|| panic!("{name} has no {key}"));
        Rgba::try_from(hex).unwrap().into()
    }

    #[test]
    fn pill_text_is_readable_in_bundled_themes() {
        let ayu = include_str!("../../../assets/themes/ayu/ayu.json");
        let one = include_str!("../../../assets/themes/one/one.json");
        for (themes, name) in [
            (ayu, "Ayu Dark"),
            (ayu, "Ayu Light"),
            (one, "One Dark"),
            (one, "One Light"),
        ] {
            let title_bar = theme_color(themes, name, "title_bar.background");
            let text = theme_color(themes, name, "text");
            let accent = theme_color(themes, name, "text.accent");
            for emphasized in [false, true] {
                let (background, border) = pill_colors(accent, emphasized);
                // Laid over the title bar, not replacing it.
                assert!(background.a < 0.2 && border.a <= 0.4);
                let pill = title_bar.blend(background);
                let ratio = calculate_contrast_ratio(text, pill);
                // At least 4.5:1 in Ayu Dark (the theme the design is for).
                // Light themes whose title bar text is under that already
                // lose little on the pill.
                let on_title_bar = calculate_contrast_ratio(text, title_bar);
                let wanted = if name == "Ayu Dark" {
                    4.5
                } else {
                    4.5_f32.min(on_title_bar * 0.85)
                };
                assert!(
                    ratio >= wanted,
                    "{name}, emphasized {emphasized}: contrast {ratio:.2}                      (title bar {on_title_bar:.2})"
                );
                eprintln!(
                    "{name}, emphasized {emphasized}: contrast {ratio:.2} (title bar {on_title_bar:.2})"
                );
                // The pill stands out from the title bar instead of looking
                // like a hole in it (darker than the bar in a dark theme).
                assert_ne!(pill, title_bar);
            }
        }
        // The design's numbers for Ayu Dark: accent #5ac1fe at 14% over
        // #313337 gives about #374753.
        let pill = theme_color(ayu, "Ayu Dark", "title_bar.background")
            .blend(pill_colors(theme_color(ayu, "Ayu Dark", "text.accent"), false).0);
        let pill = Rgba::from(pill);
        for (channel, expected) in [(pill.r, 0x37), (pill.g, 0x47), (pill.b, 0x53)] {
            assert!(((channel * 255.).round() as i32 - expected).abs() <= 1);
        }
    }
}
