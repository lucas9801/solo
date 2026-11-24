//! The recognition engine. `Recognizer` is what the rest of Solo talks to;
//! transcribe-cpp is the one that ships (`engine` feature), tests use fakes.

use std::{path::Path, sync::Arc};

use anyhow::Result;

pub trait Recognizer: Send + 'static {
    /// Recognizes 16 kHz mono samples. The text is not logged anywhere.
    fn recognize(&mut self, samples: &[f32]) -> Result<String>;
}

/// Loads the model at a path.
pub type LoadRecognizer = Arc<dyn Fn(&Path) -> Result<Box<dyn Recognizer>> + Send + Sync>;

#[cfg(feature = "engine")]
pub fn transcribe_loader() -> LoadRecognizer {
    Arc::new(|path| {
        // The library's own log lines are dropped: they can carry text.
        transcribe_cpp::disable_logging();
        let model = transcribe_cpp::Model::load(path)?;
        let session = model.session()?;
        Ok(Box::new(Transcribe {
            _model: model,
            session,
        }))
    })
}

#[cfg(feature = "engine")]
struct Transcribe {
    _model: transcribe_cpp::Model,
    session: transcribe_cpp::Session,
}

#[cfg(feature = "engine")]
impl Recognizer for Transcribe {
    fn recognize(&mut self, samples: &[f32]) -> Result<String> {
        let result = self
            .session
            .run(samples, &transcribe_cpp::RunOptions::default())?;
        Ok(result.text.trim().to_string())
    }
}

/// The loader of a build without the engine.
pub fn missing_loader() -> LoadRecognizer {
    Arc::new(|_| anyhow::bail!("this build of Solo has no speech recognition engine"))
}
