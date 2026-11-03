//! SOLO: a viewer for Mermaid diagrams printed in the terminal. The whole
//! diagram fits the pane when it opens; the wheel zooms about the pointer and
//! dragging pans, so large architecture diagrams can be read in full.

use std::{
    hash::{DefaultHasher, Hash as _, Hasher as _},
    sync::Arc,
    time::Duration,
};

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, Corners, CursorStyle, DispatchPhase, Entity,
    EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParsedSvg, PinchEvent, Point, RenderImage,
    SMOOTH_SVG_SCALE_FACTOR, ScrollDelta, ScrollEvent, Size, Task, Window, canvas, point, px, size,
};
use settings::Settings as _;
use theme_settings::ThemeSettings;
use ui::{Tooltip, prelude::*};
use util::{
    ResultExt as _,
    i18n::{is_chinese, tr},
};
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

pub(crate) const MIN_ZOOM: f32 = 0.05;
pub(crate) const MAX_ZOOM: f32 = 8.0;
/// Each wheel notch or zoom button press scales by this factor.
const ZOOM_STEP: f32 = 1.2;
/// Pixels of precise (trackpad) scroll that make up one wheel notch.
const PIXELS_PER_ZOOM_STEP: f32 = 50.0;
/// Space kept around the diagram when fitting it to the pane.
const FIT_PADDING: Pixels = px(16.);
/// Zooming re-rasterizes the diagram once it settles; until then the last
/// raster is shown stretched.
const RERASTERIZE_DEBOUNCE: Duration = Duration::from_millis(150);
/// The largest raster side, in device pixels; GPU textures are limited to
/// 16384, and memory use grows with the square. Beyond it, zooming in further
/// stretches the raster.
const MAX_RASTER_SIDE: f32 = 8192.;
/// Assumed when the SVG's size can't be read from it; corrected from the
/// first raster.
const GUESSED_NATURAL_SIZE: Size<Pixels> = Size {
    width: px(1000.),
    height: px(1000.),
};

pub(crate) fn diagram_hash(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.trim().hash(&mut hasher);
    hasher.finish()
}

/// The zoom at which a diagram of `natural` size fits in `viewport` with
/// padding around it, never enlarging it past its natural size.
pub(crate) fn fit_zoom(natural: Size<Pixels>, viewport: Size<Pixels>) -> f32 {
    let available_width = f32::from(viewport.width - FIT_PADDING * 2.).max(1.);
    let available_height = f32::from(viewport.height - FIT_PADDING * 2.).max(1.);
    let natural_width = f32::from(natural.width).max(1.);
    let natural_height = f32::from(natural.height).max(1.);
    clamp_zoom(
        (available_width / natural_width)
            .min(available_height / natural_height)
            .min(1.),
    )
}

pub(crate) fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.
    }
}

/// The offset of a diagram of `natural` size shown at `zoom` that centers it
/// in `viewport`.
pub(crate) fn centered_offset(
    natural: Size<Pixels>,
    zoom: f32,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    point(
        (viewport.width - natural.width * zoom) / 2.,
        (viewport.height - natural.height * zoom) / 2.,
    )
}

/// The offset that keeps the point of the diagram under `anchor` (relative
/// to the viewport) in place when zooming from `zoom` to `new_zoom`.
pub(crate) fn offset_zoomed_about(
    offset: Point<Pixels>,
    zoom: f32,
    new_zoom: f32,
    anchor: Point<Pixels>,
) -> Point<Pixels> {
    let ratio = new_zoom / zoom;
    point(
        anchor.x - (anchor.x - offset.x) * ratio,
        anchor.y - (anchor.y - offset.y) * ratio,
    )
}

/// How many zoom steps a scroll event makes. A wheel notch is one step
/// however many lines the platform reports for it; trackpad scrolls arrive
/// as many small pixel deltas, each a fraction of a step.
pub(crate) fn scroll_zoom_steps(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Lines(lines) => lines.y.clamp(-1., 1.),
        ScrollDelta::Pixels(pixels) => (f32::from(pixels.y) / PIXELS_PER_ZOOM_STEP).clamp(-1., 1.),
    }
}

/// The scale to rasterize at for showing a diagram at `zoom`: the zoom
/// itself, unless that raster would be too large.
fn raster_scale(natural: Size<Pixels>, zoom: f32) -> f32 {
    let longest_side = f32::from(natural.width.max(natural.height)).max(1.);
    zoom.min(MAX_RASTER_SIDE / (longest_side * SMOOTH_SVG_SCALE_FACTOR))
        .max(0.001)
}

fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    value
        .parse::<f32>()
        .ok()
        .filter(|length| length.is_finite() && *length > 0.)
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(index) = rest.find(name) {
        let preceded_by_space = rest[..index]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let after = rest[index + name.len()..].trim_start();
        rest = &rest[index + name.len()..];
        if !preceded_by_space {
            continue;
        }
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start();
        let quote = after.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value = &after[1..];
        return value.find(quote).map(|end| &value[..end]);
    }
    None
}

/// The size an SVG is drawn at by default, as read from its root element:
/// its width and height, or failing those, its view box.
pub(crate) fn svg_natural_size(svg: &str) -> Option<Size<Pixels>> {
    let start = svg.find("<svg")?;
    let tag = &svg[start..];
    let tag = &tag[..tag.find('>')?];
    let width = attribute(tag, "width").and_then(parse_length);
    let height = attribute(tag, "height").and_then(parse_length);
    let view_box = attribute(tag, "viewBox").and_then(|view_box| {
        let numbers: Vec<f32> = view_box
            .split(|char: char| char.is_whitespace() || char == ',')
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse().ok())
            .collect();
        match numbers.as_slice() {
            [_, _, width, height] if *width > 0. && *height > 0. => Some((*width, *height)),
            _ => None,
        }
    });
    let (width, height) = match (width, height, view_box) {
        (Some(width), Some(height), _) => (width, height),
        (Some(width), None, Some((box_width, box_height))) => {
            (width, width * box_height / box_width)
        }
        (None, Some(height), Some((box_width, box_height))) => {
            (height * box_width / box_height, height)
        }
        (None, None, Some(view_box)) => view_box,
        _ => return None,
    };
    Some(size(px(width), px(height)))
}

/// A short title for a diagram's tab: its `title`, if it has one.
pub(crate) fn diagram_title(source: &str) -> SharedString {
    let title = source.lines().find_map(|line| {
        let line = line.trim();
        let title = line
            .strip_prefix("title:")
            .or_else(|| line.strip_prefix("title "))?
            .trim()
            .trim_matches('"');
        (!title.is_empty()).then_some(title)
    });
    match title {
        Some(title) => {
            let mut short: String = title.chars().take(32).collect();
            if title.chars().count() > 32 {
                short.push('…');
            }
            format!("Mermaid: {short}").into()
        }
        None => "Mermaid".into(),
    }
}

fn mermaid_font_family(font_family: &str) -> String {
    let font_family = gpui::font_name_with_fallbacks(font_family, "system-ui");
    if font_family
        .split(',')
        .any(|family| family.trim().eq_ignore_ascii_case("sans-serif"))
    {
        font_family.to_string()
    } else {
        format!("{font_family}, sans-serif")
    }
}

/// The theme the Markdown preview draws diagrams in (see
/// `markdown::mermaid::build_mermaid_theme`), so they look the same here.
fn build_mermaid_theme(cx: &App) -> mermaid_render::MermaidTheme {
    let colors = cx.theme().colors();
    let theme_settings = ThemeSettings::get_global(cx);
    let is_dark = !cx.theme().appearance.is_light();

    let players = cx.theme().players();
    let git_branch_colors = std::array::from_fn(|i| players.0[i % players.0.len()].cursor);
    let git_branch_label_colors = git_branch_colors.map(mermaid_render::text_color_for_background);

    mermaid_render::MermaidTheme {
        dark_mode: is_dark,
        font_family: mermaid_font_family(theme_settings.mermaid_font_family().as_ref()),
        background: colors.editor_background,
        primary_color: colors.surface_background,
        primary_text_color: colors.text,
        primary_border_color: colors.border,
        secondary_color: colors.element_background,
        tertiary_color: colors.ghost_element_hover,
        // SOLO: edges in the border color hardly show on a dark background.
        line_color: colors.text_muted,
        text_color: colors.text,
        edge_label_background: colors.editor_background,
        cluster_background: colors.panel_background,
        cluster_border: colors.border_variant,
        note_background: colors.surface_background,
        note_border: colors.border_variant,
        actor_background: colors.element_background,
        actor_border: colors.border,
        activation_background: colors.ghost_element_hover,
        activation_border: colors.border,
        git_branch_colors,
        git_branch_label_colors,
        er_attr_bg_odd: colors.surface_background,
        er_attr_bg_even: colors.element_background,
        error_color: cx.theme().status().error,
        warning_color: cx.theme().status().warning,
        accent_colors: players
            .0
            .iter()
            .map(|player| mermaid_render::AccentColor {
                foreground: player.cursor,
                background: player.background,
            })
            .collect(),
    }
}

enum DiagramState {
    Rendering,
    Failed(SharedString),
    Ready {
        parsed: Arc<ParsedSvg>,
        natural: Size<Pixels>,
        /// Whether `natural` is a guess, to be corrected from the first raster.
        natural_is_guess: bool,
    },
}

struct Raster {
    image: Arc<RenderImage>,
    scale: f32,
}

struct Drag {
    start_position: Point<Pixels>,
    start_offset: Point<Pixels>,
}

pub struct MermaidViewer {
    source: SharedString,
    source_hash: u64,
    title: SharedString,
    state: DiagramState,
    raster: Option<Raster>,
    pending_raster_scale: Option<f32>,
    zoom: f32,
    /// Where the diagram's top left corner is, relative to the viewport's.
    offset: Point<Pixels>,
    /// Whether the diagram is fitted to the pane, and so refitted when the
    /// pane is resized, until zoomed or panned.
    fitted: bool,
    viewport: Option<Bounds<Pixels>>,
    drag: Option<Drag>,
    show_source: bool,
    focus_handle: FocusHandle,
    _render_task: Task<()>,
    _raster_task: Task<()>,
}

impl MermaidViewer {
    /// Opens `source` in a viewer, or activates the one already showing it.
    pub(crate) fn open(
        workspace: &mut Workspace,
        source: String,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<MermaidViewer> {
        let source_hash = diagram_hash(&source);
        let existing = workspace
            .items_of_type::<MermaidViewer>(cx)
            .find(|viewer| viewer.read(cx).source_hash == source_hash);
        if let Some(existing) = existing {
            workspace.activate_item(&existing, true, true, window, cx);
            return existing;
        }
        let viewer = cx.new(|cx| MermaidViewer::new(source, cx));
        // Where `Workspace::open_paths` opens files by default, as the
        // Markdown preview did.
        if !workspace.add_item_to_center(Box::new(viewer.clone()), window, cx) {
            workspace.add_item_to_active_pane(Box::new(viewer.clone()), None, true, window, cx);
        }
        viewer
    }

    fn new(source: String, cx: &mut Context<Self>) -> Self {
        let source: SharedString = source.trim_end().to_string().into();
        let source_hash = diagram_hash(&source);
        let title = diagram_title(&source);
        let theme = build_mermaid_theme(cx);
        let svg_renderer = cx.svg_renderer();
        let render_task = cx.spawn({
            let source = source.clone();
            async move |this, cx| {
                let result = cx
                    .background_spawn(async move {
                        let svg = mermaid_render::render_to_svg(&source, &theme)?;
                        let natural = svg_natural_size(&svg);
                        let parsed = svg_renderer
                            .parse_svg(svg.as_bytes())
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                        anyhow::Ok((Arc::new(parsed), natural))
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.state = match result {
                        Ok((parsed, natural)) => DiagramState::Ready {
                            parsed,
                            natural: natural.unwrap_or(GUESSED_NATURAL_SIZE),
                            natural_is_guess: natural.is_none(),
                        },
                        Err(error) => {
                            log::warn!("failed to render Mermaid diagram: {error:#}");
                            DiagramState::Failed(format!("{error:#}").into())
                        }
                    };
                    if this.viewport.is_some() {
                        this.fit_to_window(cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        });
        cx.on_release(|this, cx| {
            if let Some(raster) = this.raster.take() {
                cx.drop_image(raster.image, None);
            }
        })
        .detach();
        Self {
            source,
            source_hash,
            title,
            state: DiagramState::Rendering,
            raster: None,
            pending_raster_scale: None,
            zoom: 1.,
            offset: Point::default(),
            fitted: true,
            viewport: None,
            drag: None,
            show_source: false,
            focus_handle: cx.focus_handle(),
            _render_task: render_task,
            _raster_task: Task::ready(()),
        }
    }

    fn natural_size(&self) -> Option<Size<Pixels>> {
        match &self.state {
            DiagramState::Ready { natural, .. } => Some(*natural),
            _ => None,
        }
    }

    pub(crate) fn fit_to_window(&mut self, cx: &mut Context<Self>) {
        self.fitted = true;
        let (Some(natural), Some(viewport)) = (self.natural_size(), self.viewport) else {
            return;
        };
        self.zoom = fit_zoom(natural, viewport.size);
        self.offset = centered_offset(natural, self.zoom, viewport.size);
        self.ensure_raster(cx);
        cx.notify();
    }

    /// Zooms to `new_zoom` keeping the point under `anchor` (relative to the
    /// viewport) in place; the viewport's center without one.
    pub(crate) fn zoom_to(
        &mut self,
        new_zoom: f32,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let new_zoom = clamp_zoom(new_zoom);
        let Some(viewport) = self.viewport else {
            return;
        };
        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        let anchor =
            anchor.unwrap_or_else(|| point(viewport.size.width / 2., viewport.size.height / 2.));
        self.offset = offset_zoomed_about(self.offset, self.zoom, new_zoom, anchor);
        self.zoom = new_zoom;
        self.fitted = false;
        self.ensure_raster(cx);
        cx.notify();
    }

    fn zoom_by_steps(&mut self, steps: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        self.zoom_to(self.zoom * ZOOM_STEP.powf(steps), anchor, cx);
    }

    fn actual_size(&mut self, cx: &mut Context<Self>) {
        let (Some(natural), Some(viewport)) = (self.natural_size(), self.viewport) else {
            return;
        };
        self.zoom = 1.;
        self.offset = centered_offset(natural, 1., viewport.size);
        self.fitted = false;
        self.ensure_raster(cx);
        cx.notify();
    }

    /// Called as the viewport is laid out, before it's painted.
    fn set_viewport(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let resized = self
            .viewport
            .is_none_or(|viewport| viewport.size != bounds.size);
        self.viewport = Some(bounds);
        if resized && self.fitted && self.natural_size().is_some() {
            let zoom = self.zoom;
            self.fit_to_window(cx);
            // The toolbar showing the zoom has been rendered for this frame;
            // notifying while drawing doesn't render it again.
            if (zoom - self.zoom).abs() > f32::EPSILON {
                let this = cx.weak_entity();
                cx.defer(move |cx| {
                    this.update(cx, |_, cx| cx.notify()).ok();
                });
            }
        }
    }

    /// Rasterizes the diagram anew if the current raster isn't at the scale
    /// it's shown at. The first raster is made at once; later ones when
    /// zooming settles.
    fn ensure_raster(&mut self, cx: &mut Context<Self>) {
        let DiagramState::Ready {
            parsed, natural, ..
        } = &self.state
        else {
            return;
        };
        let target = raster_scale(*natural, self.zoom);
        let close = |scale: f32| (scale - target).abs() / target < 0.01;
        if self.pending_raster_scale.is_some_and(close)
            || (self.pending_raster_scale.is_none()
                && self
                    .raster
                    .as_ref()
                    .is_some_and(|raster| close(raster.scale)))
        {
            return;
        }
        self.pending_raster_scale = Some(target);
        let parsed = parsed.clone();
        let debounce = self.raster.is_some();
        let svg_renderer = cx.svg_renderer();
        self._raster_task = cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(RERASTERIZE_DEBOUNCE).await;
            }
            let image = cx
                .background_spawn(async move {
                    svg_renderer
                        .render_parsed(&parsed, target)
                        .map_err(|error| anyhow::anyhow!("{error}"))
                })
                .await;
            this.update(cx, |this, cx| {
                this.pending_raster_scale = None;
                let Some(image) = image.log_err() else {
                    return;
                };
                if let DiagramState::Ready {
                    natural,
                    natural_is_guess,
                    ..
                } = &mut this.state
                    && *natural_is_guess
                {
                    let device_size = image.size(0);
                    let device_scale = SMOOTH_SVG_SCALE_FACTOR * target;
                    *natural = size(
                        px(device_size.width.0 as f32 / device_scale),
                        px(device_size.height.0 as f32 / device_scale),
                    );
                    *natural_is_guess = false;
                    if this.fitted {
                        this.fit_to_window(cx);
                    }
                }
                if let Some(old) = this.raster.replace(Raster {
                    image,
                    scale: target,
                }) {
                    cx.drop_image(old.image, None);
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn copy_source(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.source.to_string()));
    }

    fn on_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let Some(viewport) = self.viewport else {
            return;
        };
        let steps = scroll_zoom_steps(event.delta);
        if steps != 0. {
            self.zoom_by_steps(steps, Some(event.position - viewport.origin), cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        if event.button == MouseButton::Left && event.click_count >= 2 {
            self.drag = None;
            self.fit_to_window(cx);
            return;
        }
        self.drag = Some(Drag {
            start_position: event.position,
            start_offset: self.offset,
        });
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        let offset = drag.start_offset + (position - drag.start_position);
        if offset != self.offset {
            self.offset = offset;
            self.fitted = false;
            cx.notify();
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        match event.keystroke.key.as_str() {
            "0" => self.fit_to_window(cx),
            "1" => self.actual_size(cx),
            "=" | "+" => self.zoom_by_steps(1., None, cx),
            "-" => self.zoom_by_steps(-1., None, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let percent = format!("{}%", (self.zoom * 100.).round() as i32);
        let ready = self.natural_size().is_some();
        let hint = tr(
            "Scroll to zoom · drag to pan · double-click to fit",
            "滚轮缩放 · 拖动平移 · 双击适应窗口",
        );
        h_flex()
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().toolbar_background)
            .child(
                IconButton::new("mermaid-fit", IconName::Maximize)
                    .icon_size(IconSize::Small)
                    .disabled(!ready || self.show_source)
                    .tooltip(Tooltip::text(tr("Fit to window (0)", "适应窗口 (0)")))
                    .on_click(cx.listener(|this, _, _, cx| this.fit_to_window(cx))),
            )
            .child(
                Button::new("mermaid-actual-size", "100%")
                    .label_size(LabelSize::Small)
                    .disabled(!ready || self.show_source)
                    .tooltip(Tooltip::text(tr("Actual size (1)", "实际大小 (1)")))
                    .on_click(cx.listener(|this, _, _, cx| this.actual_size(cx))),
            )
            .child(
                IconButton::new("mermaid-zoom-out", IconName::Dash)
                    .icon_size(IconSize::Small)
                    .disabled(!ready || self.show_source)
                    .tooltip(Tooltip::text(tr("Zoom out (-)", "缩小 (-)")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by_steps(-1., None, cx))),
            )
            .child(
                div().min_w(rems(3.)).flex().justify_center().child(
                    Label::new(percent)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                IconButton::new("mermaid-zoom-in", IconName::Plus)
                    .icon_size(IconSize::Small)
                    .disabled(!ready || self.show_source)
                    .tooltip(Tooltip::text(tr("Zoom in (=)", "放大 (=)")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by_steps(1., None, cx))),
            )
            .child(div().w_2())
            .child(
                IconButton::new("mermaid-copy-source", IconName::Copy)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(tr(
                        "Copy Mermaid source",
                        "复制 Mermaid 源码",
                    )))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_source(cx))),
            )
            .child(
                IconButton::new("mermaid-toggle-source", IconName::Code)
                    .icon_size(IconSize::Small)
                    .toggle_state(self.show_source)
                    .tooltip(Tooltip::text(tr("Show source", "显示源码")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_source = !this.show_source;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Label::new(hint)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .truncate(),
            )
    }

    fn render_source(&self, cx: &mut Context<Self>) -> AnyElement {
        let font = ThemeSettings::get_global(cx).buffer_font.family.clone();
        div()
            .id("mermaid-source")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .font_family(font)
            .text_sm()
            .text_color(cx.theme().colors().text)
            .children(
                self.source
                    .lines()
                    .map(|line| div().whitespace_nowrap().child(line.to_string())),
            )
            .into_any_element()
    }

    fn render_diagram(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        div()
            .id("mermaid-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor(if self.drag.is_some() {
                CursorStyle::ClosedHand
            } else {
                CursorStyle::OpenHand
            })
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                this.on_scroll(event, cx);
                cx.stop_propagation();
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| {
                if let Some(viewport) = this.viewport {
                    this.zoom_to(
                        this.zoom * (1. + event.delta),
                        Some(event.position - viewport.origin),
                        cx,
                    );
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.on_mouse_down(event, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event, window, cx| this.on_mouse_down(event, window, cx)),
            )
            .child(
                canvas(
                    {
                        let this = this.clone();
                        move |bounds, _, cx| {
                            this.update(cx, |this, cx| {
                                this.set_viewport(bounds, cx);
                                let natural = this.natural_size()?;
                                let raster = this.raster.as_ref()?;
                                Some((
                                    raster.image.clone(),
                                    Bounds::new(
                                        bounds.origin + this.offset,
                                        size(natural.width * this.zoom, natural.height * this.zoom),
                                    ),
                                ))
                            })
                        }
                    },
                    move |bounds, image, window, _| {
                        if let Some((image, image_bounds)) = image {
                            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                                window
                                    .paint_image(
                                        bounds,
                                        image_bounds,
                                        Corners::default(),
                                        image,
                                        0,
                                        false,
                                    )
                                    .log_err();
                            });
                        }
                        // Dragging goes on when the pointer leaves the pane.
                        window.on_mouse_event({
                            let this = this.clone();
                            move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    this.update(cx, |this, cx| {
                                        if event.pressed_button.is_some() {
                                            this.drag_to(event.position, cx);
                                        } else {
                                            this.end_drag(cx);
                                        }
                                    });
                                }
                            }
                        });
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                this.update(cx, |this, cx| this.end_drag(cx));
                            }
                        });
                    },
                )
                .size_full(),
            )
            .when(self.natural_size().is_none(), |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Label::new(tr("Rendering diagram…", "正在渲染图…")).color(Color::Muted),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_error(&self, error: &SharedString, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .p_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Label::new(tr(
                            "This Mermaid diagram couldn't be rendered",
                            "无法渲染这张 Mermaid 图",
                        ))
                        .color(Color::Error),
                    )
                    .child(
                        Label::new(error.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.render_source(cx)))
            .into_any_element()
    }
}

impl EventEmitter<ItemEvent> for MermaidViewer {}

impl Focusable for MermaidViewer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for MermaidViewer {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Image).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        let first_line = self.source.lines().find(|line| !line.trim().is_empty())?;
        Some(if is_chinese() {
            format!("Mermaid 图：{}", first_line.trim()).into()
        } else {
            format!("Mermaid diagram: {}", first_line.trim()).into()
        })
    }

    fn to_item_events(event: &ItemEvent, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for MermaidViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            DiagramState::Failed(error) => {
                let error = error.clone();
                self.render_error(&error, cx)
            }
            _ if self.show_source => self.render_source(cx),
            _ => self.render_diagram(cx),
        };
        v_flex()
            .key_context("MermaidViewer")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .on_key_down(cx.listener(|this, event, _, cx| this.on_key_down(event, cx)))
            // Zed's font size bindings (ctrl-= / ctrl-- / ctrl-0) zoom the
            // diagram instead while it's focused.
            .on_action(
                cx.listener(|this, _: &zed_actions::IncreaseBufferFontSize, _, cx| {
                    this.zoom_by_steps(1., None, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &zed_actions::DecreaseBufferFontSize, _, cx| {
                    this.zoom_by_steps(-1., None, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &zed_actions::ResetBufferFontSize, _, cx| {
                    this.fit_to_window(cx)
                }),
            )
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::Project;
    use settings::SettingsStore;

    #[test]
    fn test_fit_zoom() {
        // A tall diagram fits by height, not width.
        let zoom = fit_zoom(size(px(400.), px(4000.)), size(px(832.), px(632.)));
        assert!((zoom - 600. / 4000.).abs() < 1e-6, "{zoom}");
        // A wide one by width.
        let zoom = fit_zoom(size(px(3000.), px(200.)), size(px(632.), px(632.)));
        assert!((zoom - 600. / 3000.).abs() < 1e-6, "{zoom}");
        // A small one isn't enlarged.
        assert_eq!(
            fit_zoom(size(px(100.), px(50.)), size(px(800.), px(600.))),
            1.
        );
        // A huge one stops at the smallest zoom.
        assert_eq!(
            fit_zoom(size(px(100_000.), px(100.)), size(px(800.), px(600.))),
            MIN_ZOOM
        );

        let natural = size(px(400.), px(4000.));
        let viewport = size(px(832.), px(632.));
        let zoom = fit_zoom(natural, viewport);
        let offset = centered_offset(natural, zoom, viewport);
        assert!((f32::from(offset.y) - 16.).abs() < 1e-3, "{offset:?}");
        assert!(
            (f32::from(offset.x) - (832. - 60.) / 2.).abs() < 1e-3,
            "{offset:?}"
        );
    }

    #[test]
    fn test_zoom_about_anchor_keeps_point_in_place() {
        let offset = point(px(10.), px(20.));
        let anchor = point(px(110.), px(70.));
        let zoom = 0.5;
        let new_zoom = 2.;
        // The diagram point under the anchor before zooming...
        let diagram_x = (anchor.x - offset.x) / zoom;
        let diagram_y = (anchor.y - offset.y) / zoom;
        let new_offset = offset_zoomed_about(offset, zoom, new_zoom, anchor);
        // ...is under it after.
        assert!((f32::from(new_offset.x + diagram_x * new_zoom) - 110.).abs() < 1e-3);
        assert!((f32::from(new_offset.y + diagram_y * new_zoom) - 70.).abs() < 1e-3);
        assert_eq!(new_offset, point(px(-290.), px(-130.)));
    }

    #[test]
    fn test_zoom_clamped() {
        assert_eq!(clamp_zoom(0.001), MIN_ZOOM);
        assert_eq!(clamp_zoom(100.), MAX_ZOOM);
        assert_eq!(clamp_zoom(f32::NAN), 1.);
        assert_eq!(clamp_zoom(1.5), 1.5);
        // One step per wheel notch, however many lines it reports.
        assert_eq!(scroll_zoom_steps(ScrollDelta::Lines(point(0., 3.))), 1.);
        assert_eq!(scroll_zoom_steps(ScrollDelta::Lines(point(0., -1.))), -1.);
        assert_eq!(
            scroll_zoom_steps(ScrollDelta::Pixels(point(px(0.), px(25.)))),
            0.5
        );
        // Rasters of huge diagrams are kept within GPU limits.
        let scale = raster_scale(size(px(10_000.), px(500.)), 2.);
        assert!(10_000. * scale * SMOOTH_SVG_SCALE_FACTOR <= MAX_RASTER_SIDE + 1.);
        assert_eq!(raster_scale(size(px(500.), px(500.)), 2.), 2.);
    }

    #[test]
    fn test_svg_natural_size() {
        assert_eq!(
            svg_natural_size(
                r#"<?xml version="1.0"?><svg xmlns="x" width="120.5" height="80px" viewBox="0 0 10 10"><g/></svg>"#
            ),
            Some(size(px(120.5), px(80.)))
        );
        assert_eq!(
            svg_natural_size(
                r#"<svg id="a" stroke-width="2" viewBox="-8 -8 300 150" style="max-width: 300px;">"#
            ),
            Some(size(px(300.), px(150.)))
        );
        assert_eq!(
            svg_natural_size(r#"<svg width="100%" viewBox="0,0,300,150">"#),
            Some(size(px(300.), px(150.)))
        );
        assert_eq!(
            svg_natural_size(r#"<svg width="600" viewBox="0 0 300 150">"#),
            Some(size(px(600.), px(300.)))
        );
        assert_eq!(svg_natural_size(r#"<svg width="100%">"#), None);
        assert_eq!(svg_natural_size("not svg"), None);
    }

    #[test]
    fn test_diagram_title() {
        assert_eq!(diagram_title("graph TD\n  A --> B"), "Mermaid");
        assert_eq!(
            diagram_title("---\ntitle: Scene layers\n---\ngraph TD"),
            "Mermaid: Scene layers"
        );
        assert_eq!(
            diagram_title("pie title \"Pets\"\n  \"Dogs\": 3"),
            "Mermaid"
        );
        assert_eq!(
            diagram_title("pie\n  title Pets\n  \"Dogs\": 3"),
            "Mermaid: Pets"
        );
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    #[gpui::test]
    async fn test_open_reuses_tab_for_same_diagram(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = project::FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let diagram = "graph TD\n  A[Start] --> B[End]";
        let first = workspace.update_in(cx, |workspace, window, cx| {
            MermaidViewer::open(workspace, diagram.to_string(), window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();

        first.read_with(cx, |viewer, _| {
            assert!(
                matches!(
                    viewer.state,
                    DiagramState::Ready {
                        natural_is_guess: false,
                        ..
                    }
                ),
                "the diagram should render, its size read from the SVG"
            );
            assert!(viewer.viewport.is_some());
            assert!(viewer.fitted);
            assert!(viewer.zoom <= 1. && viewer.zoom >= MIN_ZOOM);
            assert!(viewer.raster.is_some(), "the diagram should be rasterized");
        });

        // Zooming about a point leaves the fitted state; fitting restores it.
        first.update(cx, |viewer, cx| {
            viewer.zoom_to(viewer.zoom * 2., Some(point(px(0.), px(0.))), cx);
            assert!(!viewer.fitted);
            viewer.fit_to_window(cx);
            assert!(viewer.fitted);
        });

        // The same diagram (trailing whitespace aside) reuses the tab.
        let second = workspace.update_in(cx, |workspace, window, cx| {
            MermaidViewer::open(workspace, format!("{diagram}\n"), window, cx)
        });
        assert_eq!(first.entity_id(), second.entity_id());
        let count = workspace.read_with(cx, |workspace, cx| {
            workspace.items_of_type::<MermaidViewer>(cx).count()
        });
        assert_eq!(count, 1);

        // Another diagram gets its own.
        let third = workspace.update_in(cx, |workspace, window, cx| {
            MermaidViewer::open(workspace, "graph LR\n  X --> Y".to_string(), window, cx)
        });
        assert_ne!(first.entity_id(), third.entity_id());
        let count = workspace.read_with(cx, |workspace, cx| {
            workspace.items_of_type::<MermaidViewer>(cx).count()
        });
        assert_eq!(count, 2);

        // An invalid diagram shows its error.
        let broken = workspace.update_in(cx, |workspace, window, cx| {
            MermaidViewer::open(workspace, "graph TD\n  A --> ((".to_string(), window, cx)
        });
        cx.run_until_parked();
        broken.read_with(cx, |viewer, _| {
            assert!(
                matches!(viewer.state, DiagramState::Failed(_)),
                "an invalid diagram should fail to render"
            );
        });
    }
}
