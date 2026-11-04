use std::sync::Arc;

use client::{Client, UserStore};
use cloud_api_types::Plan;
use gpui::{Action, AnyElement, Entity, IntoElement, ParentElement};
use language_model::{LanguageModelRegistry, ZED_CLOUD_PROVIDER_ID};
use ui::{List, ListBulletItem, Tooltip, prelude::*};
use util::i18n::tr;

use crate::{AgentPanelOnboardingCard, ApiKeysWithoutProviders, ZedAiOnboarding};

pub struct AgentPanelOnboarding {
    user_store: Entity<UserStore>,
    client: Arc<Client>,
    has_configured_providers: bool,
    continue_with_zed_ai: Arc<dyn Fn(&mut Window, &mut App)>,
}

impl AgentPanelOnboarding {
    pub fn new(
        user_store: Entity<UserStore>,
        client: Arc<Client>,
        continue_with_zed_ai: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(
            &LanguageModelRegistry::global(cx),
            |this: &mut Self, _registry, event: &language_model::Event, cx| match event {
                language_model::Event::ProviderStateChanged(_)
                | language_model::Event::AddedProvider(_)
                | language_model::Event::RemovedProvider(_)
                | language_model::Event::ProvidersChanged => {
                    this.has_configured_providers = Self::has_configured_providers(cx)
                }
                _ => {}
            },
        )
        .detach();

        Self {
            user_store,
            client,
            has_configured_providers: Self::has_configured_providers(cx),
            continue_with_zed_ai: Arc::new(continue_with_zed_ai),
        }
    }

    fn has_configured_providers(cx: &App) -> bool {
        LanguageModelRegistry::read_global(cx)
            .visible_providers()
            .iter()
            .any(|provider| provider.is_authenticated(cx) && provider.id() != ZED_CLOUD_PROVIDER_ID)
    }
}

/// SOLO: Zed's hosted AI, with its sign-in, plans and trials, isn't offered.
const SHOW_ZED_AI_ONBOARDING: bool = false;

impl AgentPanelOnboarding {
    /// SOLO: AI in Solo comes from the user's own API keys or an external agent.
    fn render_solo_onboarding(&self) -> AnyElement {
        let dismiss = self.continue_with_zed_ai.clone();
        v_flex()
            .w_full()
            .relative()
            .gap_1()
            .child(Headline::new(tr("Set Up AI", "设置 AI")))
            .child(
                Label::new(tr(
                    "Solo uses your own API keys or an external agent. No account needed.",
                    "Solo 使用你自己的 API 密钥或外部智能体，无需账号。",
                ))
                .color(Color::Muted)
                .mb_2(),
            )
            .child(
                List::new()
                    .child(ListBulletItem::new(tr(
                        "Add an API key for a provider such as Anthropic, OpenAI, or Ollama.",
                        "为 Anthropic、OpenAI、Ollama 等提供商添加 API 密钥。",
                    )))
                    .child(ListBulletItem::new(tr(
                        "Or add an external agent such as Claude Code or Codex.",
                        "或添加 Claude Code、Codex 等外部智能体。",
                    ))),
            )
            .child(
                Button::new("configure-providers", "Configure Providers")
                    .full_width()
                    .style(ButtonStyle::Tinted(ui::TintColor::Accent))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(zed_actions::agent::OpenSettings.boxed_clone(), cx);
                    }),
            )
            .child(
                h_flex().absolute().top_0().right_0().child(
                    IconButton::new("dismiss_onboarding", IconName::Close)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Dismiss"))
                        .on_click(move |_, window, cx| dismiss(window, cx)),
                ),
            )
            .into_any_element()
    }
}

impl Render for AgentPanelOnboarding {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !SHOW_ZED_AI_ONBOARDING {
            return AgentPanelOnboardingCard::new()
                .child(self.render_solo_onboarding())
                .into_any_element();
        }

        let enrolled_in_trial = self
            .user_store
            .read(cx)
            .plan()
            .is_some_and(|plan| plan == Plan::ZedProTrial);

        let is_pro_user = self
            .user_store
            .read(cx)
            .plan()
            .is_some_and(|plan| plan == Plan::ZedPro);

        let onboarding = ZedAiOnboarding::new(
            self.client.clone(),
            &self.user_store,
            self.continue_with_zed_ai.clone(),
            cx,
        )
        .with_dismiss({
            let callback = self.continue_with_zed_ai.clone();
            move |window, cx| callback(window, cx)
        });

        AgentPanelOnboardingCard::new()
            .child(onboarding)
            .map(|this| {
                if enrolled_in_trial || is_pro_user || self.has_configured_providers {
                    this
                } else {
                    this.child(ApiKeysWithoutProviders::new())
                }
            })
            .into_any_element()
    }
}
