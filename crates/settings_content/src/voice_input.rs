//! SOLO: the settings of voice input.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// SOLO: voice input: a shortcut and a microphone button fill what you say
/// into the input box, a terminal program or an editor. Recognition runs on
/// this computer.
#[with_fallible_options]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct VoiceInputSettingsContent {
    /// Whether the microphone button shows and the shortcut works.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// The recognition model, downloaded the first time it is used.
    ///
    /// Default: sense_voice_small
    pub model: Option<VoiceModel>,
    /// The id of the microphone to record from. When it is not set, or the
    /// microphone is gone, the system default microphone is used.
    ///
    /// Default: null
    pub microphone: Option<MicrophoneId>,
}

/// SOLO: the id of an audio input device.
#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
#[serde(transparent)]
pub struct MicrophoneId(pub String);

/// SOLO: the models voice input can use.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum VoiceModel {
    /// SenseVoice Small, 253 MB: Chinese, English, Japanese, Korean, Cantonese.
    #[default]
    #[strum(serialize = "SenseVoice Small (253 MB)")]
    SenseVoiceSmall,
    /// Whisper large-v3-turbo, 886 MB: 99 languages, more accurate, slower.
    #[strum(serialize = "Whisper large-v3-turbo (886 MB)")]
    WhisperLargeV3Turbo,
    /// Whisper tiny, 46 MB: 99 languages, the smallest and least accurate.
    #[strum(serialize = "Whisper tiny (46 MB)")]
    WhisperTiny,
}
