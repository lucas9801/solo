//! The recognition models: where they are downloaded from, how they are
//! checked, and where they are kept.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient};
use sha2::{Digest as _, Sha256};
use util::i18n::tr;

/// A model Solo can download. The files are the Q8_0 GGUF conversions that
/// handy-computer publishes for transcribe-cpp 0.4.x, pinned by revision.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub file_name: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
    pub license: &'static str,
}

pub const SENSE_VOICE_SMALL: ModelInfo = ModelInfo {
    id: "sense-voice-small",
    label: "SenseVoice Small",
    repository: "handy-computer/SenseVoiceSmall-gguf",
    revision: "4a08b8e900b38a977e32eb08d5d0697d6e72ba04",
    file_name: "SenseVoiceSmall-Q8_0.gguf",
    size_bytes: 252_684_608,
    sha256: "6c759ee4c9748c9b3f7a5a60ca74f0f7e685fb9d45d1378fce7cfd62f59adf29",
    license: "FunASR Model License",
};

pub const WHISPER_LARGE_V3_TURBO: ModelInfo = ModelInfo {
    id: "whisper-large-v3-turbo",
    label: "Whisper large-v3-turbo",
    repository: "handy-computer/whisper-large-v3-turbo-gguf",
    revision: "5eaf945c7978e564bae5b28a5b1639dd93c2bfb1",
    file_name: "whisper-large-v3-turbo-Q8_0.gguf",
    size_bytes: 886_381_760,
    sha256: "b2e30cc286bc9f3aba4db9099fc7403543497c05ce7100d0d83091ddfd25a183",
    license: "MIT",
};

pub const WHISPER_TINY: ModelInfo = ModelInfo {
    id: "whisper-tiny",
    label: "Whisper tiny",
    repository: "handy-computer/whisper-tiny-gguf",
    revision: "6687f30c99641ee265df421e582354adbc8848fc",
    file_name: "whisper-tiny-Q8_0.gguf",
    size_bytes: 45_981_088,
    sha256: "325b9c7997cd1eff81ef709d55766565e71be696130cc3a3d444713798706834",
    license: "Apache-2.0",
};

pub const MODELS: [&ModelInfo; 3] = [&SENSE_VOICE_SMALL, &WHISPER_LARGE_V3_TURBO, &WHISPER_TINY];

impl ModelInfo {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}?download=true",
            self.repository, self.revision, self.file_name
        )
    }

    /// The id this model has in `downloads.allowed` and in the questions of
    /// `net_guard`.
    pub fn component_id(&self) -> String {
        format!("voice-model-{}", self.id)
    }

    pub fn component(&self) -> net_guard::Component {
        net_guard::Component::new(
            self.component_id(),
            format!("{} ({})", self.label, self.size_text()),
            Some("huggingface.co"),
        )
    }

    pub fn size_megabytes(&self) -> u64 {
        // The sizes in the model lists are in MB of 10^6 bytes, as the
        // download sources show them.
        (self.size_bytes + 500_000) / 1_000_000
    }

    pub fn size_text(&self) -> String {
        format!("{} MB", self.size_megabytes())
    }

    pub fn languages(&self) -> &'static str {
        match self.id {
            "sense-voice-small" => tr(
                "Chinese, English, Japanese, Korean, Cantonese",
                "中文、英文、日语、韩语、粤语",
            ),
            "whisper-large-v3-turbo" => tr(
                "99 languages, more accurate, slower",
                "99 种语言，更准，更慢",
            ),
            _ => tr(
                "99 languages, smallest, less accurate",
                "99 种语言，最小，准确率低",
            ),
        }
    }

    pub fn path(&self, directory: &Path) -> PathBuf {
        directory.join(self.id).join(self.file_name)
    }

    /// Whether the file is on disk at the size it should have. The hash is
    /// checked once, when the file is downloaded.
    pub fn is_downloaded(&self, directory: &Path) -> bool {
        std::fs::metadata(self.path(directory))
            .is_ok_and(|metadata| metadata.len() == self.size_bytes)
    }
}

pub fn model_by_id(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().copied().find(|model| model.id == id)
}

/// Where the models are kept: Solo's data directory.
pub fn models_directory() -> PathBuf {
    paths::data_dir().join("voice_models")
}

/// Deletes a downloaded model (and a half-finished download of it).
pub fn delete_model(model: &ModelInfo, directory: &Path) -> Result<()> {
    let folder = directory.join(model.id);
    match std::fs::remove_dir_all(&folder) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("deleting {}", folder.display())),
    }
}

/// Downloads `model` into `directory`, calling `progress` with the bytes
/// received so far. The bytes go to a `.part` file that is hashed on the way
/// and only renamed to the final name when its size and SHA-256 match.
pub async fn download_model(
    client: Arc<dyn HttpClient>,
    model: &ModelInfo,
    directory: &Path,
    url: &str,
    mut progress: impl FnMut(u64),
) -> Result<PathBuf> {
    let target = model.path(directory);
    let folder = target
        .parent()
        .context("the model path has no folder")?
        .to_path_buf();
    std::fs::create_dir_all(&folder)?;
    let partial = target.with_extension("gguf.part");

    let mut response = client
        .get(url, AsyncBody::empty(), true)
        .await
        .context("requesting the model")?;
    if !response.status().is_success() {
        bail!("downloading {}: HTTP {}", model.label, response.status());
    }

    let mut file = std::fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    let result: Result<()> = async {
        loop {
            let count = response.body_mut().read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            let chunk = buffer.get(..count).context("short read")?;
            hasher.update(chunk);
            file.write_all(chunk)?;
            received += count as u64;
            progress(received);
        }
        file.flush()?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(error) = result {
        std::fs::remove_file(&partial).ok();
        return Err(error.context(format!("downloading {}", model.label)));
    }

    let digest = format!("{:x}", hasher.finalize());
    if received != model.size_bytes || digest != model.sha256 {
        std::fs::remove_file(&partial).ok();
        bail!(
            "{} did not match its checksum ({received} bytes received)",
            model.label
        );
    }
    std::fs::rename(&partial, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::FakeHttpClient;

    fn test_model(bytes: &[u8]) -> &'static ModelInfo {
        let digest: &'static str =
            Box::leak(format!("{:x}", Sha256::digest(bytes)).into_boxed_str());
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

    fn serving(bytes: &'static [u8]) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |_| async move {
            Ok(http_client::Response::builder()
                .status(200)
                .body(AsyncBody::from(bytes.to_vec()))?)
        })
    }

    #[test]
    fn test_catalog_has_the_three_models_with_hashes() {
        assert_eq!(MODELS.len(), 3);
        for model in MODELS {
            assert_eq!(model.sha256.len(), 64, "{}", model.id);
            assert!(
                model
                    .url()
                    .starts_with("https://huggingface.co/handy-computer/")
            );
            assert!(model.url().contains(model.revision));
        }
        assert_eq!(SENSE_VOICE_SMALL.size_text(), "253 MB");
        assert_eq!(WHISPER_LARGE_V3_TURBO.size_text(), "886 MB");
        assert_eq!(WHISPER_TINY.size_text(), "46 MB");
    }

    #[test]
    fn test_download_checks_the_hash_and_reports_progress() {
        let directory = tempfile_directory("download-ok");
        let bytes: &'static [u8] = b"model bytes for the test";
        let model = test_model(bytes);
        let mut last = 0;
        let path = futures::executor::block_on(download_model(
            serving(bytes),
            model,
            &directory,
            "https://example.test/model",
            |received| last = received,
        ))
        .expect("download");
        assert_eq!(last, bytes.len() as u64);
        assert_eq!(std::fs::read(&path).expect("read"), bytes);
        assert!(model.is_downloaded(&directory));
        assert!(!path.with_extension("gguf.part").exists());
        delete_model(model, &directory).expect("delete");
        assert!(!model.is_downloaded(&directory));
    }

    #[test]
    fn test_download_with_a_wrong_hash_keeps_nothing() {
        let directory = tempfile_directory("download-bad");
        let model = test_model(b"the expected bytes");
        let result = futures::executor::block_on(download_model(
            serving(b"the wrong bytes!!!"),
            model,
            &directory,
            "https://example.test/model",
            |_| {},
        ));
        assert!(result.is_err());
        assert!(!model.is_downloaded(&directory));
        assert!(!model.path(&directory).exists());
        assert!(!model.path(&directory).with_extension("gguf.part").exists());
    }

    #[test]
    fn test_download_of_a_missing_file_fails() {
        let directory = tempfile_directory("download-404");
        let model = test_model(b"x");
        let client: Arc<dyn HttpClient> = FakeHttpClient::with_404_response();
        let result = futures::executor::block_on(download_model(
            client,
            model,
            &directory,
            "https://example.test/model",
            |_| {},
        ));
        assert!(result.is_err());
    }

    fn tempfile_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("solo-voice-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&directory).ok();
        std::fs::create_dir_all(&directory).expect("directory");
        directory
    }
}
