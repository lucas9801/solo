//! SOLO: voice input. A shortcut (or the microphone on the input box) records
//! from the microphone, an engine on this computer recognizes it, and the
//! text goes where the focus was when the recording started: the input box
//! or an editor (at the cursor), or the program running in a terminal (as a
//! paste). Nothing is sent: the text is never followed by Enter.
//!
//! The sound stays in memory; it is not written to disk or uploaded. The
//! recognized text is not logged.

mod engine;
mod hotkey;
mod models;
mod recorder;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, Global, KeyDownEvent, KeyUpEvent, Task,
    Window, actions,
};
use parking_lot::Mutex;
use settings::{RegisterSetting, Settings, update_settings_file};
use util::{ResultExt as _, i18n::tr};

#[cfg(feature = "engine")]
pub use engine::transcribe_loader;
pub use engine::{LoadRecognizer, Recognizer, missing_loader};
pub use hotkey::{Effect, Event, HOLD_THRESHOLD, Hotkey};
pub use models::{
    MODELS, ModelInfo, SENSE_VOICE_SMALL, WHISPER_LARGE_V3_TURBO, WHISPER_TINY, delete_model,
    download_model, model_by_id, models_directory,
};
pub use recorder::{Downsampler, InputDevice, RecordError, Recorder, SAMPLE_RATE, input_devices};

actions!(
    voice_input,
    [
        /// Starts or stops voice input: press once to start and once to stop,
        /// or hold to talk and let go to stop.
        ToggleVoiceInput,
    ]
);

/// How long a recording can be before it is too short to recognize.
const MINIMUM_SAMPLES: usize = SAMPLE_RATE as usize / 4;
/// The model leaves memory this long after its last use.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);
/// How long a message stays in the input box.
const NOTICE_DURATION: Duration = Duration::from_secs(6);
/// A key press this soon after the last one is the key repeating, not a new
/// press (Windows waits up to a second before it repeats).
const REPEAT_WINDOW: Duration = Duration::from_millis(1200);

#[derive(Clone, Debug, RegisterSetting)]
pub struct VoiceInputSettings {
    pub enabled: bool,
    pub model: settings::VoiceModel,
    pub microphone: Option<String>,
}

impl Settings for VoiceInputSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let voice = content.voice_input.clone().unwrap_or_default();
        Self {
            enabled: voice.enabled.unwrap_or(true),
            model: voice.model.unwrap_or_default(),
            microphone: voice
                .microphone
                .map(|microphone| microphone.0)
                .filter(|id| !id.is_empty()),
        }
    }
}

impl VoiceInputSettings {
    pub fn model_info(&self) -> &'static ModelInfo {
        match self.model {
            settings::VoiceModel::SenseVoiceSmall => &SENSE_VOICE_SMALL,
            settings::VoiceModel::WhisperLargeV3Turbo => &WHISPER_LARGE_V3_TURBO,
            settings::VoiceModel::WhisperTiny => &WHISPER_TINY,
        }
    }
}

/// Why nothing was filled in.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// No microphone, or Windows doesn't let desktop apps use it.
    MicrophoneUnavailable,
    /// The model can't be downloaded (offline mode, downloads off, ...).
    DownloadBlocked(String),
    Failed(String),
    NothingHeard,
}

impl Notice {
    pub fn text(&self) -> String {
        match self {
            Self::MicrophoneUnavailable => tr(
                "Windows doesn't allow desktop apps to use the microphone, or no microphone is connected.",
                "Windows 没有允许桌面应用使用麦克风，或者没接麦克风。",
            )
            .to_string(),
            Self::DownloadBlocked(reason) => reason.clone(),
            Self::Failed(message) => message.clone(),
            Self::NothingHeard => tr("Heard nothing", "没听到声音").to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// The model isn't on this computer; waiting for the user's answer.
    AskDownload,
    Downloading {
        received: u64,
        total: u64,
    },
    Listening {
        since: Instant,
    },
    Recognizing,
    Notice(Notice),
}

/// Where the text goes: the place that had the focus when the recording
/// started.
pub struct Destination {
    /// The entity the voice input belongs to, so its view can show the state.
    pub origin: EntityId,
    /// Puts the text in place. Does nothing when the place is gone.
    pub deliver: Box<dyn FnOnce(&str, &mut App)>,
}

/// A recording in progress.
pub trait Capture: Send {
    fn level(&self) -> f32;
    /// Stops recording and returns the 16 kHz mono samples.
    fn finish(self: Box<Self>) -> Vec<f32>;
}

impl Capture for Recorder {
    fn level(&self) -> f32 {
        Recorder::level(self)
    }

    fn finish(self: Box<Self>) -> Vec<f32> {
        Recorder::finish(*self)
    }
}

/// Opens the microphone with the given id (`None`: the system default).
pub type OpenCapture =
    Arc<dyn Fn(Option<String>) -> Result<Box<dyn Capture>, RecordError> + Send + Sync>;

/// The download rules for a model.
pub type DownloadRules = Arc<dyn Fn(&ModelInfo) -> net_guard::Decision>;

pub fn open_microphone() -> OpenCapture {
    Arc::new(|device| {
        let device = device.and_then(|id| id.parse::<cpal::DeviceId>().ok());
        Recorder::start(device).map(|recorder| Box::new(recorder) as Box<dyn Capture>)
    })
}

/// Keeps a model's component id in `downloads.allowed` of the user settings.
pub type RememberDownload = Arc<dyn Fn(String, &mut App)>;

pub fn remember_in_settings() -> RememberDownload {
    Arc::new(|component_id, cx| {
        update_settings_file(<dyn fs::Fs>::global(cx), cx, move |content, _| {
            content
                .downloads
                .get_or_insert_default()
                .allowed
                .get_or_insert_default()
                .insert(component_id, true);
        });
    })
}

pub fn download_rules() -> DownloadRules {
    Arc::new(|model| net_guard::global().decision(&model.component(), false, true))
}

struct LoadedEngine {
    model_id: &'static str,
    recognizer: Box<dyn Recognizer>,
}

pub struct VoiceInput {
    phase: Phase,
    hotkey: Hotkey,
    destination: Option<Destination>,
    capture: Option<Box<dyn Capture>>,
    /// Asked to stop (or cancel) before the microphone finished opening.
    stop_early: Option<bool>,
    level: f32,
    /// When the shortcut's key last went down, and which key it is.
    trigger: Option<(Instant, String)>,
    trigger_down: bool,
    /// Changes whenever a recording is started or cancelled, so late results
    /// of an older one are dropped.
    generation: u64,
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    last_used: Instant,
    load_recognizer: LoadRecognizer,
    open_capture: OpenCapture,
    download_rules: DownloadRules,
    remember_download: RememberDownload,
    models_directory: PathBuf,
    idle_unload: Duration,
    model_override: Option<&'static ModelInfo>,
    download_task: Option<Task<()>>,
    open_task: Option<Task<()>>,
    tick_task: Option<Task<()>>,
    recognize_task: Option<Task<()>>,
    notice_task: Option<Task<()>>,
    unload_task: Option<Task<()>>,
}

struct GlobalVoiceInput(Entity<VoiceInput>);

impl Global for GlobalVoiceInput {}

/// The parts of voice input that tests replace.
pub struct Parts {
    pub load_recognizer: LoadRecognizer,
    pub open_capture: OpenCapture,
    pub download_rules: DownloadRules,
    pub remember_download: RememberDownload,
    pub models_directory: PathBuf,
}

impl Parts {
    pub fn production() -> Self {
        #[cfg(feature = "engine")]
        let load_recognizer = transcribe_loader();
        #[cfg(not(feature = "engine"))]
        let load_recognizer = missing_loader();
        Self {
            load_recognizer,
            open_capture: open_microphone(),
            download_rules: download_rules(),
            remember_download: remember_in_settings(),
            models_directory: models_directory(),
        }
    }
}

pub fn init(cx: &mut App) {
    install(Parts::production(), cx);
}

pub fn install(parts: Parts, cx: &mut App) -> Entity<VoiceInput> {
    let voice_input = cx.new(|_| VoiceInput::new(parts));
    cx.set_global(GlobalVoiceInput(voice_input.clone()));
    voice_input
}

pub fn global(cx: &App) -> Option<Entity<VoiceInput>> {
    cx.try_global::<GlobalVoiceInput>()
        .map(|global| global.0.clone())
}

/// Whether voice input is on (the setting), so its button and shortcut show.
pub fn is_enabled(cx: &App) -> bool {
    global(cx).is_some() && VoiceInputSettings::get_global(cx).enabled
}

/// What the shortcut or the microphone button asks for, with the place the
/// text should go. Pass `destination` as a function so nothing is built when
/// the press only stops a recording that is already going.
pub fn toggle(
    window: Option<&Window>,
    destination: impl FnOnce() -> Option<Destination>,
    cx: &mut App,
) {
    let Some(voice_input) = global(cx) else {
        return;
    };
    let trigger_key = window.and_then(trigger_key);
    voice_input.update(cx, |voice_input, cx| {
        voice_input.press(Instant::now(), trigger_key, destination, cx)
    });
}

/// The key of the shortcut that runs `ToggleVoiceInput` in this window.
fn trigger_key(window: &Window) -> Option<String> {
    let binding = window.highest_precedence_binding_for_action(&ToggleVoiceInput)?;
    binding
        .keystrokes()
        .last()
        .map(|keystroke| keystroke.key().to_string())
}

pub fn handle_key_up(event: &KeyUpEvent, _: &mut Window, cx: &mut App) {
    let Some(voice_input) = global(cx) else {
        return;
    };
    if !voice_input.read(cx).trigger_down {
        return;
    }
    let key = event.keystroke.key.clone();
    voice_input.update(cx, |voice_input, cx| {
        voice_input.key_up(Instant::now(), &key, cx)
    });
}

/// Escape cancels listening, recognizing and the download. Returns whether it
/// was used, so the caller can keep it from reaching the focused view.
pub fn handle_key_down(event: &KeyDownEvent, _: &mut Window, cx: &mut App) -> bool {
    let keystroke = &event.keystroke;
    if keystroke.key != "escape" || keystroke.modifiers.modified() {
        return false;
    }
    escape(cx)
}

/// Esc: cancels what voice input is doing. Returns false, and leaves Esc to
/// whatever has the focus, when it isn't doing anything.
pub fn escape(cx: &mut App) -> bool {
    let Some(voice_input) = global(cx) else {
        return false;
    };
    if matches!(voice_input.read(cx).phase, Phase::Idle | Phase::Notice(_)) {
        return false;
    }
    voice_input.update(cx, |voice_input, cx| voice_input.cancel(cx));
    true
}

impl VoiceInput {
    pub fn new(parts: Parts) -> Self {
        Self {
            phase: Phase::Idle,
            hotkey: Hotkey::default(),
            destination: None,
            capture: None,
            stop_early: None,
            level: 0.0,
            trigger: None,
            trigger_down: false,
            generation: 0,
            engine: Arc::new(Mutex::new(None)),
            last_used: Instant::now(),
            load_recognizer: parts.load_recognizer,
            open_capture: parts.open_capture,
            download_rules: parts.download_rules,
            remember_download: parts.remember_download,
            models_directory: parts.models_directory,
            idle_unload: IDLE_UNLOAD,
            model_override: None,
            download_task: None,
            open_task: None,
            tick_task: None,
            recognize_task: None,
            notice_task: None,
            unload_task: None,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The view that started what is going on, if anything is.
    pub fn origin(&self) -> Option<EntityId> {
        self.destination
            .as_ref()
            .map(|destination| destination.origin)
    }

    pub fn is_listening(&self) -> bool {
        matches!(self.phase, Phase::Listening { .. })
    }

    /// The volume of the last fraction of a second, 0 to about 1.
    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn models_directory(&self) -> &PathBuf {
        &self.models_directory
    }

    pub fn is_model_loaded(&self) -> bool {
        self.engine.lock().is_some()
    }

    pub fn set_idle_unload(&mut self, duration: Duration) {
        self.idle_unload = duration;
    }

    /// Uses `model` instead of the one in the settings. For tests.
    pub fn set_model_override(&mut self, model: Option<&'static ModelInfo>) {
        self.model_override = model;
    }

    fn model(&self, cx: &App) -> &'static ModelInfo {
        self.model_override
            .unwrap_or_else(|| VoiceInputSettings::get_global(cx).model_info())
    }

    /// The model the download question is about.
    pub fn model_to_download(&self, cx: &App) -> &'static ModelInfo {
        self.model(cx)
    }

    fn set_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        self.phase = phase;
        cx.notify();
    }

    /// The shortcut or button was pressed.
    pub fn press(
        &mut self,
        now: Instant,
        trigger_key: Option<String>,
        destination: impl FnOnce() -> Option<Destination>,
        cx: &mut Context<Self>,
    ) {
        if !VoiceInputSettings::get_global(cx).enabled {
            return;
        }
        if self.trigger_down
            && self
                .trigger
                .as_ref()
                .is_some_and(|(last, _)| now.saturating_duration_since(*last) < REPEAT_WINDOW)
        {
            // The key is repeating while it is held.
            self.trigger = self.trigger.take().map(|(_, key)| (now, key));
            return;
        }
        self.trigger_down = trigger_key.is_some();
        self.trigger = trigger_key.map(|key| (now, key));

        match self.phase {
            Phase::AskDownload | Phase::Downloading { .. } | Phase::Recognizing => {}
            Phase::Idle | Phase::Notice(_) => {
                if self.hotkey.handle(Event::Press, now) == Effect::Start {
                    let Some(destination) = destination() else {
                        self.hotkey.reset();
                        return;
                    };
                    self.destination = Some(destination);
                    self.begin(cx);
                }
            }
            Phase::Listening { .. } => {
                if self.hotkey.handle(Event::Press, now) == Effect::Stop {
                    self.stop(cx);
                }
            }
        }
    }

    /// The shortcut's key came up.
    pub fn key_up(&mut self, now: Instant, key: &str, cx: &mut Context<Self>) {
        let is_trigger = self
            .trigger
            .as_ref()
            .is_some_and(|(_, trigger)| trigger == key);
        if !is_trigger {
            return;
        }
        self.trigger_down = false;
        if self.hotkey.handle(Event::Release, now) == Effect::Stop {
            self.stop(cx);
        }
    }

    /// Escape: stop and fill nothing in.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.hotkey.handle(Event::Escape, Instant::now());
        self.hotkey.reset();
        self.generation += 1;
        self.download_task = None;
        if self.capture.is_none() && matches!(self.phase, Phase::Listening { .. }) {
            self.stop_early = Some(true);
        }
        self.capture.take();
        self.destination = None;
        self.set_phase(Phase::Idle, cx);
    }

    /// Closes a message (or the mic permission card).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Notice(_)) {
            self.destination = None;
            self.set_phase(Phase::Idle, cx);
        }
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        let model = self.model(cx);
        if model.is_downloaded(&self.models_directory) {
            self.start_listening(cx);
            return;
        }
        match (self.download_rules)(model) {
            net_guard::Decision::Allow => self.start_download(cx),
            net_guard::Decision::Ask => self.set_phase(Phase::AskDownload, cx),
            net_guard::Decision::Block(reason) => {
                let message = net_guard::DownloadBlocked {
                    name: model.label.to_string(),
                    reason,
                }
                .to_string();
                self.fail(Notice::DownloadBlocked(message), cx);
            }
        }
    }

    /// The user said yes to the download question.
    pub fn confirm_download(&mut self, cx: &mut Context<Self>) {
        if self.phase != Phase::AskDownload {
            return;
        }
        let model = self.model(cx);
        (self.remember_download.clone())(model.component_id(), cx);
        self.start_download(cx);
    }

    /// The user said no to the download question.
    pub fn decline_download(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::AskDownload {
            self.hotkey.reset();
            self.destination = None;
            self.set_phase(Phase::Idle, cx);
        }
    }

    fn start_download(&mut self, cx: &mut Context<Self>) {
        let model = self.model(cx);
        if let Err(blocked) = net_guard::ensure_online(model.label) {
            self.fail(Notice::DownloadBlocked(blocked.to_string()), cx);
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.set_phase(
            Phase::Downloading {
                received: 0,
                total: model.size_bytes,
            },
            cx,
        );
        let client = cx.http_client();
        let directory = self.models_directory.clone();
        let (progress_sender, mut progress) = mpsc::unbounded::<u64>();
        let download = cx.background_spawn(async move {
            download_model(client, model, &directory, &model.url(), move |received| {
                progress_sender.unbounded_send(received).ok();
            })
            .await
        });
        let task = cx.spawn(async move |this, cx| {
            while let Some(received) = progress.next().await {
                let current = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        this.set_phase(
                            Phase::Downloading {
                                received,
                                total: model.size_bytes,
                            },
                            cx,
                        );
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
            }
            let result = download.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(_) => {
                        // The key was let go long ago: the next press stops.
                        let now = Instant::now();
                        this.hotkey.reset();
                        this.hotkey.handle(Event::Press, now);
                        this.hotkey.handle(Event::Release, now);
                        this.start_listening(cx);
                    }
                    Err(error) => {
                        log::error!("downloading the voice model failed: {error:#}");
                        this.fail(
                            Notice::Failed(
                                tr(
                                    "Couldn't download the recognition model",
                                    "识别模型下载失败",
                                )
                                .to_string(),
                            ),
                            cx,
                        );
                    }
                }
            })
            .log_err();
        });
        self.download_task = Some(task);
    }

    fn start_listening(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.stop_early = None;
        self.level = 0.0;
        self.set_phase(
            Phase::Listening {
                since: Instant::now(),
            },
            cx,
        );
        let open = self.open_capture.clone();
        let device = VoiceInputSettings::get_global(cx).microphone.clone();
        let opening = cx.background_spawn(async move { open(device) });
        let opened = cx.spawn(async move |this, cx| {
            let result = opening.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(capture) => {
                        this.capture = Some(capture);
                        if let Some(cancel) = this.stop_early.take() {
                            if cancel {
                                this.cancel(cx);
                            } else {
                                this.stop(cx);
                            }
                        }
                    }
                    Err(error) => {
                        log::warn!("voice input: {error}");
                        this.fail(Notice::MicrophoneUnavailable, cx);
                    }
                }
            })
            .log_err();
        });
        let ticks = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.generation != generation || !this.is_listening() {
                            return false;
                        }
                        if let Some(capture) = &this.capture {
                            this.level = capture.level();
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        });
        self.open_task = Some(opened);
        self.tick_task = Some(ticks);
    }

    /// Stops listening and recognizes what was said.
    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.is_listening() {
            return;
        }
        let Some(capture) = self.capture.take() else {
            self.stop_early = Some(false);
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.set_phase(Phase::Recognizing, cx);

        let engine = self.engine.clone();
        let load = self.load_recognizer.clone();
        let model = self.model(cx);
        let path = model.path(&self.models_directory);
        let recognition = cx.background_spawn(async move {
            let samples = capture.finish();
            if samples.len() < MINIMUM_SAMPLES {
                return Ok(String::new());
            }
            let mut engine = engine.lock();
            if engine
                .as_ref()
                .is_none_or(|loaded| loaded.model_id != model.id)
            {
                *engine = None;
                *engine = Some(LoadedEngine {
                    model_id: model.id,
                    recognizer: load(&path)?,
                });
            }
            match engine.as_mut() {
                Some(loaded) => loaded.recognizer.recognize(&samples),
                None => anyhow::bail!("the recognition engine is not loaded"),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let result = recognition.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.last_used = Instant::now();
                this.schedule_unload(cx);
                this.hotkey.reset();
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        let destination = this.destination.take();
                        this.set_phase(Phase::Idle, cx);
                        if let Some(destination) = destination {
                            (destination.deliver)(text.trim(), cx);
                        }
                    }
                    Ok(_) => this.fail(Notice::NothingHeard, cx),
                    Err(error) => {
                        log::error!("voice recognition failed: {error:#}");
                        this.fail(
                            Notice::Failed(
                                tr("Couldn't recognize the speech", "识别失败").to_string(),
                            ),
                            cx,
                        );
                    }
                }
            })
            .log_err();
        });
        self.recognize_task = Some(task);
    }

    /// Shows a message and forgets what was going on.
    fn fail(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.hotkey.reset();
        self.capture.take();
        self.generation += 1;
        let generation = self.generation;
        self.set_phase(Phase::Notice(notice), cx);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_DURATION).await;
            this.update(cx, |this, cx| {
                if this.generation == generation && matches!(this.phase, Phase::Notice(_)) {
                    this.destination = None;
                    this.set_phase(Phase::Idle, cx);
                }
            })
            .log_err();
        });
        self.notice_task = Some(task);
    }

    /// Unloads the model once it hasn't been used for a while.
    fn schedule_unload(&mut self, cx: &mut Context<Self>) {
        let last_used = self.last_used;
        let wait = self.idle_unload;
        let engine = self.engine.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let unload = this
                .update(cx, |this, _| {
                    this.last_used == last_used && !matches!(this.phase, Phase::Recognizing)
                })
                .unwrap_or(false);
            if unload {
                cx.background_spawn(async move {
                    engine.lock().take();
                })
                .await;
                this.update(cx, |_, cx| cx.notify()).log_err();
            }
        });
        self.unload_task = Some(task);
    }

    /// Deletes the downloaded model, unloading it first.
    pub fn delete_downloaded(&mut self, model: &'static ModelInfo, cx: &mut Context<Self>) {
        self.engine.lock().take();
        delete_model(model, &self.models_directory).log_err();
        cx.notify();
    }
}

/// Voice input with a fake microphone and engine, for tests of the views that
/// start it.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::*;

    struct Silence;

    impl Capture for Silence {
        fn level(&self) -> f32 {
            0.0
        }

        fn finish(self: Box<Self>) -> Vec<f32> {
            vec![0.1; SAMPLE_RATE as usize]
        }
    }

    struct Says(&'static str);

    impl Recognizer for Says {
        fn recognize(&mut self, _: &[f32]) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    static FAKE_MODEL: ModelInfo = ModelInfo {
        id: "fake-model",
        label: "Fake model",
        repository: "",
        revision: "",
        file_name: "fake.gguf",
        size_bytes: 4,
        sha256: "",
        license: "",
    };

    /// Installs voice input whose engine always hears `spoken`.
    pub fn install_saying(spoken: &'static str, cx: &mut App) -> Entity<VoiceInput> {
        let directory =
            std::env::temp_dir().join(format!("solo-voice-fake-{}", std::process::id()));
        let path = FAKE_MODEL.path(&directory);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).log_err();
        }
        std::fs::write(&path, b"fake").log_err();
        let voice_input = install(
            Parts {
                load_recognizer: Arc::new(move |_| Ok(Box::new(Says(spoken)))),
                open_capture: Arc::new(|_| Ok(Box::new(Silence))),
                download_rules: Arc::new(|_| net_guard::Decision::Allow),
                remember_download: Arc::new(|_, _| {}),
                models_directory: directory,
            },
            cx,
        );
        voice_input.update(cx, |voice_input, _| {
            voice_input.set_model_override(Some(&FAKE_MODEL))
        });
        voice_input
    }
}

#[cfg(test)]
mod tests;
