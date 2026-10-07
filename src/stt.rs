//! Speech-to-text with Parakeet TDT 0.6B v3 (int8, CPU).

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use transcribe_rs::onnx::Quantization;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};

pub const MODEL_NAME: &str = "parakeet-tdt-0.6b-v3-int8";

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("lightwispr")
}

pub struct Stt(ParakeetModel);

impl Stt {
    pub fn load() -> Result<Self> {
        let dir = data_dir().join("models").join(MODEL_NAME);
        ParakeetModel::load(&dir, &Quantization::Int8)
            .map(Self)
            .map_err(|e| anyhow!("loading model from {}: {e}", dir.display()))
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let result = self
            .0
            .transcribe_with(samples, &ParakeetParams::default())
            .map_err(|e| anyhow!("transcription failed: {e}"))?;
        Ok(result.text.trim().to_string())
    }
}
