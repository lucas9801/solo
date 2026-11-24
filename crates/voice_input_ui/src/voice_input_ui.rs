//! SOLO: the cards of voice input: the question before the model is
//! downloaded, the download's progress, and the messages (no microphone
//! permission, nothing could be recognized). The state itself lives in
//! `voice_input`; the listening state shows in the input box.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Window, px,
};
use theme::ActiveTheme as _;
use ui::{Button, ButtonStyle, prelude::*};
use util::i18n::tr;
use voice_input::{Notice, Phase, VoiceInput};
use workspace::notifications::{
    Notification, NotificationId, SuppressEvent, dismiss_app_notification, show_app_notification,
};

pub fn init(cx: &mut App) {
    let Some(voice_input) = voice_input::global(cx) else {
        return;
    };
    let mut shown = false;
    cx.observe(&voice_input, move |voice_input, cx| {
        let wanted = shows_card(voice_input.read(cx).phase());
        if wanted && !shown {
            shown = true;
            show_app_notification(notification_id(), cx, move |cx| {
                cx.new(|cx| VoiceCard::new(voice_input.clone(), cx))
            });
        } else if !wanted && shown {
            shown = false;
            dismiss_app_notification(&notification_id(), cx);
        }
    })
    .detach();
}

fn notification_id() -> NotificationId {
    NotificationId::named("voice-input".into())
}

fn shows_card(phase: &Phase) -> bool {
    matches!(
        phase,
        Phase::AskDownload | Phase::Downloading { .. } | Phase::Notice(_)
    )
}

struct VoiceCard {
    voice_input: Entity<VoiceInput>,
    focus_handle: FocusHandle,
    _subscription: gpui::Subscription,
}

impl VoiceCard {
    fn new(voice_input: Entity<VoiceInput>, cx: &mut Context<Self>) -> Self {
        let _subscription = cx.observe(&voice_input, |_, _, cx| cx.notify());
        Self {
            voice_input,
            focus_handle: cx.focus_handle(),
            _subscription,
        }
    }
}

impl EventEmitter<DismissEvent> for VoiceCard {}
impl EventEmitter<SuppressEvent> for VoiceCard {}
impl Notification for VoiceCard {}

impl Focusable for VoiceCard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn megabytes(bytes: u64) -> u64 {
    bytes / 1_000_000
}

struct Card {
    icon: IconName,
    icon_color: Color,
    title: String,
    body: Option<String>,
    progress: Option<(f32, String)>,
    buttons: Vec<AnyElement>,
}

impl Render for VoiceCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = self.voice_input.read(cx).phase().clone();
        let model = self.voice_input.read(cx).model_to_download(cx);
        let colors = cx.theme().colors();
        let voice_input = self.voice_input.clone();

        let card = match phase {
            Phase::AskDownload => Card {
                icon: IconName::Mic,
                icon_color: Color::Default,
                title: tr(
                    "Voice input needs to download a recognition model",
                    "语音输入要先下载识别模型",
                )
                .to_string(),
                body: Some(if util::i18n::is_chinese() {
                    format!(
                        "{}，{}。支持{}。下载后不联网也能用，声音不会离开这台电脑。",
                        model.label,
                        model.size_text(),
                        model.languages()
                    )
                } else {
                    format!(
                        "{}, {}. Supports {}. It works offline once downloaded, and your voice never leaves this computer.",
                        model.label,
                        model.size_text(),
                        model.languages()
                    )
                }),
                progress: None,
                buttons: vec![
                    Button::new("voice-cancel", tr("Cancel", "取消"))
                        .style(ButtonStyle::Subtle)
                        .on_click({
                            let voice_input = voice_input.clone();
                            move |_, _, cx| {
                                voice_input
                                    .update(cx, |voice_input, cx| voice_input.decline_download(cx))
                            }
                        })
                        .into_any_element(),
                    Button::new("voice-download", tr("Download", "下载"))
                        .style(ButtonStyle::Filled)
                        .on_click({
                            move |_, _, cx| {
                                voice_input
                                    .update(cx, |voice_input, cx| voice_input.confirm_download(cx))
                            }
                        })
                        .into_any_element(),
                ],
            },
            Phase::Downloading { received, total } => Card {
                icon: IconName::Download,
                icon_color: Color::Default,
                title: tr("Downloading the recognition model", "正在下载识别模型").to_string(),
                body: None,
                progress: Some((
                    if total == 0 {
                        0.0
                    } else {
                        (received as f32 / total as f32).clamp(0.0, 1.0)
                    },
                    format!("{} / {} MB", megabytes(received), megabytes(total)),
                )),
                buttons: vec![
                    Button::new("voice-cancel-download", tr("Cancel", "取消"))
                        .style(ButtonStyle::Subtle)
                        .on_click({
                            move |_, _, cx| {
                                voice_input.update(cx, |voice_input, cx| voice_input.cancel(cx))
                            }
                        })
                        .into_any_element(),
                ],
            },
            Phase::Notice(notice) => {
                let microphone = matches!(notice, Notice::MicrophoneUnavailable);
                let mut buttons = vec![
                    Button::new("voice-close", tr("Close", "关闭"))
                        .style(ButtonStyle::Subtle)
                        .on_click({
                            let voice_input = voice_input.clone();
                            move |_, _, cx| {
                                voice_input.update(cx, |voice_input, cx| voice_input.dismiss(cx))
                            }
                        })
                        .into_any_element(),
                ];
                if microphone {
                    buttons.push(
                        Button::new(
                            "voice-open-settings",
                            tr("Open Windows settings", "打开 Windows 设置"),
                        )
                        .style(ButtonStyle::Filled)
                        .on_click({
                            move |_, _, cx| {
                                cx.open_url("ms-settings:privacy-microphone");
                                voice_input.update(cx, |voice_input, cx| voice_input.dismiss(cx))
                            }
                        })
                        .into_any_element(),
                    );
                }
                Card {
                    icon: if microphone {
                        IconName::MicMute
                    } else {
                        IconName::Warning
                    },
                    icon_color: Color::Warning,
                    title: if microphone {
                        tr("The microphone can't be used", "用不了麦克风").to_string()
                    } else {
                        tr("Voice input", "语音输入").to_string()
                    },
                    body: Some(notice.text()),
                    progress: None,
                    buttons,
                }
            }
            _ => Card {
                icon: IconName::Mic,
                icon_color: Color::Default,
                title: String::new(),
                body: None,
                progress: None,
                buttons: Vec::new(),
            },
        };

        v_flex()
            .id("voice-input-card")
            .track_focus(&self.focus_handle)
            .w(px(380.))
            .p_3()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(card.icon)
                            .size(IconSize::Small)
                            .color(card.icon_color),
                    )
                    .child(Label::new(card.title)),
            )
            .children(
                card.body
                    .map(|body| Label::new(body).size(LabelSize::Small).color(Color::Muted)),
            )
            .children(card.progress.map(|(fraction, text)| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .h(px(6.))
                            .w_full()
                            .rounded_full()
                            .bg(colors.element_background)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(fraction))
                                    .rounded_full()
                                    .bg(colors.text_accent),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Label::new(text).size(LabelSize::XSmall).color(Color::Muted))
                            .child(
                                Label::new(tr(
                                    "Listening starts when it is done",
                                    "下载完就开始听",
                                ))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                            ),
                    )
            }))
            .child(h_flex().justify_end().gap_2().children(card.buttons))
    }
}
