use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};

use gpui::{BorrowAppContext as _, TestAppContext};
use http_client::{AsyncBody, FakeHttpClient};
use net_guard::{Blocked, Decision};
use settings::SettingsStore;
use sha2::{Digest as _, Sha256};

use super::*;

const SPOKEN: &str = "让 reparent() 拒绝把节点挂到它自己的子孙下面";

struct FakeCapture(Vec<f32>);

impl Capture for FakeCapture {
    fn level(&self) -> f32 {
        0.25
    }

    fn finish(self: Box<Self>) -> Vec<f32> {
        self.0
    }
}

struct FakeRecognizer {
    text: String,
    fail: bool,
}

impl Recognizer for FakeRecognizer {
    fn recognize(&mut self, samples: &[f32]) -> Result<String> {
        assert!(!samples.is_empty());
        if self.fail {
            anyhow::bail!("the engine broke");
        }
        Ok(self.text.clone())
    }
}

struct Fixture {
    voice_input: Entity<VoiceInput>,
    delivered: Rc<RefCell<Vec<String>>>,
    microphones_opened: Arc<AtomicUsize>,
    models_loaded: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    directory: PathBuf,
    model: &'static ModelInfo,
}

#[derive(Clone)]
struct Options {
    microphone_works: bool,
    engine_fails: bool,
    model_present: bool,
    rules: Decision,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            microphone_works: true,
            engine_fails: false,
            model_present: true,
            rules: Decision::Allow,
        }
    }
}

fn test_model(bytes: &'static [u8]) -> &'static ModelInfo {
    let digest: &'static str = Box::leak(format!("{:x}", Sha256::digest(bytes)).into_boxed_str());
    Box::leak(Box::new(ModelInfo {
        id: "test-model",
        label: "Test model",
        repository: "test/test",
        revision: "0",
        file_name: "test.gguf",
        size_bytes: bytes.len() as u64,
        sha256: digest,
        license: "MIT",
    }))
}

const MODEL_BYTES: &[u8] = b"pretend this is a model file";

fn fixture(options: Options, cx: &mut TestAppContext) -> Fixture {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "solo-voice-input-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_dir_all(&directory).ok();
    let model = test_model(MODEL_BYTES);
    if options.model_present {
        let path = model.path(&directory);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, MODEL_BYTES).expect("model file");
    }

    let microphones_opened = Arc::new(AtomicUsize::new(0));
    let models_loaded = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let engine_fails = options.engine_fails;
    let microphone_works = options.microphone_works;
    let rules = options.rules;

    let parts = Parts {
        load_recognizer: Arc::new({
            let models_loaded = models_loaded.clone();
            move |_| {
                models_loaded.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(FakeRecognizer {
                    text: format!("  {SPOKEN}\n"),
                    fail: engine_fails,
                }))
            }
        }),
        open_capture: Arc::new({
            let microphones_opened = microphones_opened.clone();
            move |_| {
                microphones_opened.fetch_add(1, Ordering::Relaxed);
                if microphone_works {
                    Ok(Box::new(FakeCapture(vec![0.1; 16_000])) as Box<dyn Capture>)
                } else {
                    Err(RecordError::Unavailable("no device".into()))
                }
            }
        }),
        download_rules: Arc::new(move |_| rules),
        remember_download: Arc::new(|_, _| {}),
        models_directory: directory.clone(),
    };

    let voice_input = cx.update(|cx| {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        let client = FakeHttpClient::create({
            let requests = requests.clone();
            move |_| {
                let requests = requests.clone();
                async move {
                    requests.fetch_add(1, Ordering::Relaxed);
                    Ok(http_client::Response::builder()
                        .status(200)
                        .body(AsyncBody::from(MODEL_BYTES.to_vec()))?)
                }
            }
        });
        cx.set_http_client(client);
        let voice_input = install(parts, cx);
        voice_input.update(cx, |voice_input, _| {
            voice_input.set_model_override(Some(model))
        });
        voice_input
    });
    Fixture {
        voice_input,
        delivered: Rc::default(),
        microphones_opened,
        models_loaded,
        requests,
        directory,
        model,
    }
}

impl Fixture {
    fn destination(&self) -> Option<Destination> {
        let delivered = self.delivered.clone();
        Some(Destination {
            origin: self.voice_input.entity_id(),
            deliver: Box::new(move |text, _| delivered.borrow_mut().push(text.to_string())),
        })
    }

    fn press(&self, at: Instant, cx: &mut TestAppContext) {
        let destination = self.destination();
        self.voice_input.update(cx, |voice_input, cx| {
            voice_input.press(at, Some("m".into()), || destination, cx)
        });
        cx.run_until_parked();
    }

    fn release(&self, at: Instant, cx: &mut TestAppContext) {
        self.voice_input
            .update(cx, |voice_input, cx| voice_input.key_up(at, "m", cx));
        cx.run_until_parked();
    }

    fn phase(&self, cx: &mut TestAppContext) -> Phase {
        self.voice_input
            .read_with(cx, |voice_input, _| voice_input.phase().clone())
    }

    fn delivered(&self) -> Vec<String> {
        self.delivered.borrow().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).ok();
    }
}

#[gpui::test]
fn test_default_settings_turn_voice_input_on_with_sense_voice(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        let settings = VoiceInputSettings::get_global(cx);
        assert!(settings.enabled);
        assert_eq!(settings.model_info().id, "sense-voice-small");
        assert_eq!(settings.microphone, None);
        assert!(!is_enabled(cx), "nothing is installed yet");
    });
}

#[gpui::test]
fn test_tap_starts_and_the_second_tap_fills_the_text(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();

    fixture.press(start, cx);
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
    fixture.release(start + Duration::from_millis(120), cx);
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
    assert!(fixture.delivered().is_empty());

    fixture.press(start + Duration::from_secs(4), cx);
    assert_eq!(fixture.phase(cx), Phase::Idle);
    // Trimmed, and without a line break: nothing sends it.
    assert_eq!(fixture.delivered(), vec![SPOKEN.to_string()]);
    assert_eq!(fixture.microphones_opened.load(Ordering::Relaxed), 1);
}

#[gpui::test]
fn test_hold_listens_until_the_key_comes_up(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();

    fixture.press(start, cx);
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
    fixture.release(start + Duration::from_millis(900), cx);

    assert_eq!(fixture.phase(cx), Phase::Idle);
    assert_eq!(fixture.delivered(), vec![SPOKEN.to_string()]);
}

#[gpui::test]
fn test_key_repeat_while_held_does_not_stop_the_recording(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();

    fixture.press(start, cx);
    for step in 1..=10 {
        fixture.press(start + Duration::from_millis(600 + step * 33), cx);
    }
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
    fixture.release(start + Duration::from_millis(1100), cx);
    assert_eq!(fixture.delivered(), vec![SPOKEN.to_string()]);
}

#[gpui::test]
fn test_escape_cancels_and_fills_nothing(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();

    fixture.press(start, cx);
    fixture.release(start + Duration::from_millis(100), cx);
    fixture
        .voice_input
        .update(cx, |voice_input, cx| voice_input.cancel(cx));
    cx.run_until_parked();

    assert_eq!(fixture.phase(cx), Phase::Idle);
    assert!(fixture.delivered().is_empty());
    assert_eq!(fixture.models_loaded.load(Ordering::Relaxed), 0);

    // The key can be used again afterwards.
    fixture.press(start + Duration::from_secs(5), cx);
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
}

#[gpui::test]
fn test_escape_while_recognizing_drops_the_text(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();
    fixture.press(start, cx);
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
    // Stopped and cancelled before the engine answers.
    fixture.voice_input.update(cx, |voice_input, cx| {
        voice_input.key_up(start + Duration::from_secs(2), "m", cx);
        assert_eq!(*voice_input.phase(), Phase::Recognizing);
        voice_input.cancel(cx);
    });
    cx.run_until_parked();
    assert!(fixture.delivered().is_empty());
    assert_eq!(fixture.phase(cx), Phase::Idle);
}

#[gpui::test]
fn test_escape_is_used_only_while_voice_input_is_busy(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    assert!(
        !cx.update(|cx| escape(cx)),
        "idle: Esc belongs to the focused view"
    );

    let start = Instant::now();
    fixture.press(start, cx);
    assert!(matches!(fixture.phase(cx), Phase::Listening { .. }));
    assert!(cx.update(|cx| escape(cx)));
    cx.run_until_parked();
    assert_eq!(fixture.phase(cx), Phase::Idle);
    assert!(fixture.delivered().is_empty());
    assert!(!cx.update(|cx| escape(cx)));
}

#[gpui::test]
fn test_text_goes_to_where_the_recording_started(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();
    // The place that had the focus when the recording started.
    let target: Rc<RefCell<String>> = Rc::default();
    let other: Rc<RefCell<String>> = Rc::default();
    let destination = Destination {
        origin: fixture.voice_input.entity_id(),
        deliver: Box::new({
            let target = target.clone();
            move |text, _| target.borrow_mut().push_str(text)
        }),
    };
    fixture.voice_input.update(cx, |voice_input, cx| {
        voice_input.press(start, Some("m".into()), || Some(destination), cx)
    });
    cx.run_until_parked();
    fixture.release(start + Duration::from_millis(100), cx);

    // The focus moves; a press somewhere else only stops, and its own
    // destination is not even built.
    let built = Rc::new(RefCell::new(false));
    fixture.voice_input.update(cx, |voice_input, cx| {
        voice_input.press(
            start + Duration::from_secs(5),
            Some("m".into()),
            || {
                *built.borrow_mut() = true;
                Some(Destination {
                    origin: fixture.voice_input.entity_id(),
                    deliver: Box::new({
                        let other = other.clone();
                        move |text, _| other.borrow_mut().push_str(text)
                    }),
                })
            },
            cx,
        )
    });
    cx.run_until_parked();

    assert_eq!(*target.borrow(), SPOKEN);
    assert!(other.borrow().is_empty());
    assert!(!*built.borrow());
}

#[gpui::test]
fn test_no_microphone_shows_the_notice_and_fills_nothing(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            microphone_works: false,
            ..Options::default()
        },
        cx,
    );
    fixture.press(Instant::now(), cx);

    assert_eq!(
        fixture.phase(cx),
        Phase::Notice(Notice::MicrophoneUnavailable)
    );
    assert!(fixture.delivered().is_empty());
    assert_eq!(fixture.models_loaded.load(Ordering::Relaxed), 0);

    // It goes away by itself, and can be closed.
    cx.executor()
        .advance_clock(NOTICE_DURATION + Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(fixture.phase(cx), Phase::Idle);
}

#[gpui::test]
fn test_offline_mode_downloads_nothing(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            model_present: false,
            rules: Decision::Block(Blocked::Offline),
            ..Options::default()
        },
        cx,
    );
    fixture.press(Instant::now(), cx);

    let Phase::Notice(Notice::DownloadBlocked(message)) = fixture.phase(cx) else {
        panic!("expected the blocked notice, got {:?}", fixture.phase(cx));
    };
    assert!(
        message.contains("Offline") || message.contains("离线"),
        "{message}"
    );
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.microphones_opened.load(Ordering::Relaxed), 0);
    assert!(!fixture.model.is_downloaded(&fixture.directory));
}

#[gpui::test]
fn test_downloads_off_downloads_nothing(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            model_present: false,
            rules: Decision::Block(Blocked::Never),
            ..Options::default()
        },
        cx,
    );
    fixture.press(Instant::now(), cx);
    assert!(matches!(
        fixture.phase(cx),
        Phase::Notice(Notice::DownloadBlocked(_))
    ));
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 0);
}

#[gpui::test]
fn test_first_use_asks_before_downloading_then_listens(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            model_present: false,
            rules: Decision::Ask,
            ..Options::default()
        },
        cx,
    );
    let start = Instant::now();
    fixture.press(start, cx);

    assert_eq!(fixture.phase(cx), Phase::AskDownload);
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 0);
    // While the question is open the shortcut does nothing more.
    fixture.press(start + Duration::from_secs(2), cx);
    assert_eq!(fixture.phase(cx), Phase::AskDownload);

    fixture
        .voice_input
        .update(cx, |voice_input, cx| voice_input.confirm_download(cx));
    cx.run_until_parked();

    assert_eq!(fixture.requests.load(Ordering::Relaxed), 1);
    assert!(fixture.model.is_downloaded(&fixture.directory));
    assert!(
        matches!(fixture.phase(cx), Phase::Listening { .. }),
        "listening starts once the download is done"
    );
    // The key was let go long ago, so one more press ends the recording.
    fixture.press(start + Duration::from_secs(60), cx);
    assert_eq!(fixture.delivered(), vec![SPOKEN.to_string()]);
}

#[gpui::test]
fn test_declining_the_download_keeps_everything_as_it_was(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            model_present: false,
            rules: Decision::Ask,
            ..Options::default()
        },
        cx,
    );
    fixture.press(Instant::now(), cx);
    fixture
        .voice_input
        .update(cx, |voice_input, cx| voice_input.decline_download(cx));
    cx.run_until_parked();

    assert_eq!(fixture.phase(cx), Phase::Idle);
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 0);
    assert!(fixture.delivered().is_empty());
}

#[gpui::test]
fn test_a_broken_download_fails_without_a_half_file(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            model_present: false,
            rules: Decision::Allow,
            ..Options::default()
        },
        cx,
    );
    // The server sends different bytes than the pinned hash.
    cx.update(|cx| {
        cx.set_http_client(FakeHttpClient::create(|_| async {
            Ok(http_client::Response::builder()
                .status(200)
                .body(AsyncBody::from(b"not the model".to_vec()))?)
        }))
    });
    fixture.press(Instant::now(), cx);

    assert!(matches!(
        fixture.phase(cx),
        Phase::Notice(Notice::Failed(_))
    ));
    assert!(!fixture.model.is_downloaded(&fixture.directory));
    assert_eq!(fixture.microphones_opened.load(Ordering::Relaxed), 0);
}

#[gpui::test]
fn test_a_failing_engine_shows_a_notice(cx: &mut TestAppContext) {
    let fixture = fixture(
        Options {
            engine_fails: true,
            ..Options::default()
        },
        cx,
    );
    let start = Instant::now();
    fixture.press(start, cx);
    fixture.release(start + Duration::from_secs(2), cx);

    assert!(matches!(
        fixture.phase(cx),
        Phase::Notice(Notice::Failed(_))
    ));
    assert!(fixture.delivered().is_empty());
}

#[gpui::test]
fn test_a_recording_that_is_too_short_fills_nothing(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    cx.update(|cx| {
        fixture.voice_input.update(cx, |voice_input, _| {
            voice_input.open_capture = Arc::new(|_| Ok(Box::new(FakeCapture(vec![0.1; 100]))));
        })
    });
    let start = Instant::now();
    fixture.press(start, cx);
    fixture.release(start + Duration::from_secs(2), cx);

    assert_eq!(fixture.phase(cx), Phase::Notice(Notice::NothingHeard));
    assert!(fixture.delivered().is_empty());
    assert_eq!(fixture.models_loaded.load(Ordering::Relaxed), 0);
}

#[gpui::test]
fn test_the_model_is_loaded_once_and_unloaded_when_unused(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();
    assert!(
        !fixture
            .voice_input
            .read_with(cx, |v, _| v.is_model_loaded())
    );

    for round in 0..2u64 {
        let at = start + Duration::from_secs(round * 10);
        fixture.press(at, cx);
        fixture.release(at + Duration::from_secs(1), cx);
    }
    assert_eq!(fixture.delivered().len(), 2);
    assert_eq!(fixture.models_loaded.load(Ordering::Relaxed), 1);
    assert!(
        fixture
            .voice_input
            .read_with(cx, |v, _| v.is_model_loaded())
    );

    cx.executor()
        .advance_clock(IDLE_UNLOAD - Duration::from_secs(10));
    cx.run_until_parked();
    assert!(
        fixture
            .voice_input
            .read_with(cx, |v, _| v.is_model_loaded()),
        "still loaded before the five minutes are over"
    );

    cx.executor().advance_clock(Duration::from_secs(20));
    cx.run_until_parked();
    assert!(
        !fixture
            .voice_input
            .read_with(cx, |v, _| v.is_model_loaded())
    );
}

#[gpui::test]
fn test_turned_off_in_settings_it_does_nothing(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    cx.update(|cx| {
        cx.update_global::<SettingsStore, _>(|store, cx| {
            store.update_user_settings(cx, |content| {
                content.voice_input.get_or_insert_default().enabled = Some(false);
            });
        });
        assert!(!is_enabled(cx));
    });
    fixture.press(Instant::now(), cx);

    assert_eq!(fixture.phase(cx), Phase::Idle);
    assert_eq!(fixture.microphones_opened.load(Ordering::Relaxed), 0);
}

#[gpui::test]
fn test_without_a_destination_nothing_starts(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    fixture.voice_input.update(cx, |voice_input, cx| {
        voice_input.press(Instant::now(), Some("m".into()), || None, cx)
    });
    cx.run_until_parked();
    assert_eq!(fixture.phase(cx), Phase::Idle);
    assert_eq!(fixture.microphones_opened.load(Ordering::Relaxed), 0);
}

#[gpui::test]
fn test_the_mic_is_only_open_while_listening(cx: &mut TestAppContext) {
    let fixture = fixture(Options::default(), cx);
    let start = Instant::now();
    fixture.press(start, cx);
    fixture.release(start + Duration::from_secs(2), cx);
    assert!(
        fixture
            .voice_input
            .read_with(cx, |v, _| v.capture.is_none())
    );
    assert_eq!(fixture.microphones_opened.load(Ordering::Relaxed), 1);
}
