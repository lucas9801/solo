//! SOLO: the update button at the right end of the title bar, for Solo's own
//! updater and the Microsoft Store edition's (`auto_update::update_button`
//! decides what it says). It replaces Zed's `UpdateVersion`, which followed
//! Zed's updater, which never runs in Solo.

use auto_update::update_button::{
    self as model, ButtonState, MenuItem, UpdateEntry, offer_summary, perform,
};
use gpui::{Empty, Entity, Render};
use ui::{ButtonLike, ContextMenu, PopoverMenu, TintColor, prelude::*};

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
                        context_menu.entry(label.clone(), None, move |window, cx| {
                            perform(&entry, action, window, cx)
                        })
                    }
                };
            }
            context_menu
        },
    ))
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
        let (tint, color) = if label.warning {
            (TintColor::Warning, Color::Warning)
        } else {
            (TintColor::Accent, Color::Accent)
        };
        let button = ButtonLike::new("solo-update-button")
            .style(ButtonStyle::Tinted(tint))
            .child(Label::new(label.text).size(LabelSize::Small).color(color));
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
