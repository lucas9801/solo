//! Recording from the microphone, on a thread of its own. The sound is kept
//! in memory only, as 16 kHz mono samples, which is what the engines take.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use cpal::DeviceId;
use parking_lot::Mutex;

pub const SAMPLE_RATE: u32 = 16_000;
/// The longest recording, so a forgotten shortcut doesn't fill the memory.
const MAX_SECONDS: usize = 300;
pub const MAX_RECORDING: Duration = Duration::from_secs(MAX_SECONDS as u64);
/// The samples between two level readings.
const LEVEL_WINDOW: usize = 800;

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// No microphone, or Windows doesn't let desktop apps use it.
    #[error("the microphone can't be used: {0}")]
    Unavailable(String),
}

/// Turns interleaved samples of any rate and channel count into 16 kHz mono.
pub struct Downsampler {
    channels: usize,
    channel: usize,
    frame_sum: f32,
    step: f64,
    phase: f64,
    bucket_sum: f32,
    bucket_count: u32,
    /// For input slower than 16 kHz: the last frame, and where the next
    /// output sample lies after it, in frames.
    previous: f32,
    has_previous: bool,
    position: f64,
}

impl Downsampler {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            channel: 0,
            frame_sum: 0.0,
            step: f64::from(sample_rate.max(1)) / f64::from(SAMPLE_RATE),
            phase: 0.0,
            bucket_sum: 0.0,
            bucket_count: 0,
            previous: 0.0,
            has_previous: false,
            position: 0.0,
        }
    }

    pub fn push(&mut self, sample: f32, output: &mut Vec<f32>) {
        self.frame_sum += sample;
        self.channel += 1;
        if self.channel < self.channels {
            return;
        }
        let mono = self.frame_sum / self.channels as f32;
        self.channel = 0;
        self.frame_sum = 0.0;

        if self.step < 1.0 {
            // Slower than 16 kHz: interpolate between neighbouring frames.
            if !self.has_previous {
                self.previous = mono;
                self.has_previous = true;
                return;
            }
            while self.position < 1.0 {
                output.push(self.previous + (mono - self.previous) * self.position as f32);
                self.position += self.step;
            }
            self.position -= 1.0;
            self.previous = mono;
            return;
        }

        self.bucket_sum += mono;
        self.bucket_count += 1;
        self.phase += 1.0;
        if self.phase >= self.step {
            self.phase -= self.step;
            output.push(self.bucket_sum / self.bucket_count as f32);
            self.bucket_sum = 0.0;
            self.bucket_count = 0;
        }
    }
}

/// A microphone Windows lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
}

/// The audio input devices, for the choice of microphone.
pub fn input_devices() -> Vec<InputDevice> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _};

    let Ok(devices) = cpal::default_host().devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| {
            let id = device.id().ok()?;
            let description = device.description().ok()?;
            description.supports_input().then(|| InputDevice {
                id: id.to_string(),
                name: description.name().to_string(),
            })
        })
        .collect()
}

/// How long a list of devices is reused: listing them is slow, and the
/// settings page draws often.
const DEVICE_LIST_LIFETIME: Duration = Duration::from_secs(10);

/// `input_devices`, listed again only when the last listing is old.
pub fn cached_input_devices() -> Vec<InputDevice> {
    static CACHE: Mutex<Option<(Instant, Vec<InputDevice>)>> = Mutex::new(None);

    let mut cache = CACHE.lock();
    if let Some((listed, devices)) = cache.as_ref()
        && listed.elapsed() < DEVICE_LIST_LIFETIME
    {
        return devices.clone();
    }
    let devices = input_devices();
    *cache = Some((Instant::now(), devices.clone()));
    devices
}

/// A running recording.
pub struct Recorder {
    samples: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

impl Recorder {
    /// Opens the microphone (`None`: the system default) and starts
    /// recording. Blocks while Windows opens the device, so call it off the
    /// main thread.
    pub fn start(device: Option<DeviceId>) -> Result<Self, RecordError> {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let level = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (opened_sender, opened) = mpsc::channel();

        std::thread::Builder::new()
            .name("voice-recorder".into())
            .spawn({
                let samples = samples.clone();
                let level = level.clone();
                let stop = stop.clone();
                move || {
                    let microphone = match audio::open_input_stream(device) {
                        Ok(microphone) => microphone,
                        Err(error) => {
                            opened_sender.send(Err(format!("{error:#}"))).ok();
                            return;
                        }
                    };
                    let config = microphone.config();
                    let mut downsampler = Downsampler::new(
                        config.sample_rate.get(),
                        usize::from(config.channel_count.get()),
                    );
                    opened_sender.send(Ok(())).ok();

                    let mut pending = Vec::with_capacity(4096);
                    let mut window_energy = 0.0f32;
                    let mut window_count = 0usize;
                    let mut total = 0usize;
                    for sample in microphone {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        downsampler.push(sample, &mut pending);
                        for value in &pending {
                            window_energy += value * value;
                            window_count += 1;
                        }
                        if window_count >= LEVEL_WINDOW {
                            let rms = (window_energy / window_count as f32).sqrt();
                            level.store(rms.to_bits(), Ordering::Relaxed);
                            window_energy = 0.0;
                            window_count = 0;
                        }
                        if total < MAX_SECONDS * SAMPLE_RATE as usize {
                            total += pending.len();
                            samples.lock().append(&mut pending);
                        }
                        pending.clear();
                    }
                }
            })
            .map_err(|error| RecordError::Unavailable(error.to_string()))?;

        match opened.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                samples,
                level,
                stop,
                started: Instant::now(),
            }),
            Ok(Err(message)) => Err(RecordError::Unavailable(message)),
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                Err(RecordError::Unavailable(error.to_string()))
            }
        }
    }

    /// The volume of the last fraction of a second, 0 to about 1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Stops recording and returns what was recorded.
    pub fn finish(self) -> Vec<f32> {
        self.stop.store(true, Ordering::Relaxed);
        std::mem::take(&mut *self.samples.lock())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(sample_rate: u32, channels: usize, input: &[f32]) -> Vec<f32> {
        let mut downsampler = Downsampler::new(sample_rate, channels);
        let mut output = Vec::new();
        for sample in input {
            downsampler.push(*sample, &mut output);
        }
        output
    }

    #[test]
    fn test_48k_stereo_becomes_16k_mono() {
        let frames = 4800;
        let input = (0..frames).flat_map(|_| [0.5, 0.1]).collect::<Vec<_>>();
        let output = run(48_000, 2, &input);
        assert_eq!(output.len(), 1600);
        assert!(output.iter().all(|value| (value - 0.3).abs() < 1e-5));
    }

    #[test]
    fn test_8k_is_stretched_to_16k() {
        let output = run(8_000, 1, &vec![0.2; 8_000]);
        assert!(
            (output.len() as i64 - 16_000).abs() <= 2,
            "{}",
            output.len()
        );
        assert!(output.iter().all(|value| (value - 0.2).abs() < 1e-5));
    }

    #[test]
    fn test_upsampling_interpolates_between_frames() {
        let output = run(8_000, 1, &[0.0, 1.0, 0.0]);
        assert_eq!(output, vec![0.0, 0.5, 1.0, 0.5]);
    }

    #[test]
    fn test_16k_mono_passes_through() {
        let input = vec![0.25; 1000];
        assert_eq!(run(16_000, 1, &input), input);
    }

    #[test]
    fn test_44100_hz_gives_the_right_length() {
        let output = run(44_100, 1, &vec![0.1; 44_100]);
        assert!(
            (output.len() as i64 - 16_000).abs() <= 1,
            "{}",
            output.len()
        );
    }
}
