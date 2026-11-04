mod call_stats_modal;
pub mod channel_view;
pub mod collab_panel;
pub mod notifications;
mod panel_settings;

use std::{any::TypeId, rc::Rc, sync::Arc};

use command_palette_hooks::CommandPaletteFilter;

pub use collab_panel::CollabPanel;
use gpui::{
    App, Pixels, PlatformDisplay, Size, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions, point,
};
pub use panel_settings::CollaborationPanelSettings;
use release_channel::ReleaseChannel;
use ui::px;
use workspace::AppState;

// Another comment, nice.
pub fn init(app_state: &Arc<AppState>, cx: &mut App) {
    call_stats_modal::init(cx);
    channel_view::init(cx);
    collab_panel::init(cx);
    notifications::init(app_state, cx);
    title_bar::init(cx);
    hide_zed_account_actions(cx);
}

/// SOLO: calls, channels, shared projects, and the Zed account behind them are
/// Zed's hosted services, so their commands stay out of the command palette.
fn hide_zed_account_actions(cx: &mut App) {
    CommandPaletteFilter::update_global(cx, |filter, _| {
        filter.hide_namespace("client");
        filter.hide_namespace("collab_panel");
        filter.hide_namespace("channel_modal");
        // `collab` also holds the title bar's menus, so only its call and
        // channel actions are hidden.
        filter.hide_action_types(&[
            TypeId::of::<workspace::OpenChannelNotes>(),
            TypeId::of::<workspace::OpenChannelNotesById>(),
            TypeId::of::<workspace::Mute>(),
            TypeId::of::<workspace::Deafen>(),
            TypeId::of::<workspace::LeaveCall>(),
            TypeId::of::<workspace::ShareProject>(),
            TypeId::of::<workspace::ScreenShare>(),
            TypeId::of::<workspace::CopyRoomId>(),
            TypeId::of::<channel_view::CopyLink>(),
            TypeId::of::<zed_actions::ShowCallStats>(),
            TypeId::of::<zed_actions::OpenAccountSettings>(),
        ]);
    });
}

fn notification_window_options(
    screen: Rc<dyn PlatformDisplay>,
    size: Size<Pixels>,
    cx: &App,
) -> WindowOptions {
    let notification_margin_width = px(16.);
    let notification_margin_height = px(-48.);

    let bounds = gpui::Bounds::<Pixels> {
        origin: screen.bounds().top_right()
            - point(
                size.width + notification_margin_width,
                notification_margin_height,
            ),
        size,
    };

    let app_id = ReleaseChannel::global(cx).app_id();

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        display_id: Some(screen.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some(app_id.to_owned()),
        window_min_size: None,
        window_decorations: Some(WindowDecorations::Client),
        tabbing_identifier: None,
        ..Default::default()
    }
}
