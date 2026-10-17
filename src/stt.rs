//! Speech-to-text: Parakeet TDT 0.6B v3 (ONNX, int8, CPU) or Whisper large-v3-turbo
//! (whisper.cpp, CPU or Vulkan). Only the engine from the config is ever loaded.

use anyhow::{Result, anyhow, bail};
use transcribe_rs::onnx::Quantization;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};
use transcribe_rs::whisper_cpp::{WhisperEngine, WhisperInferenceParams, WhisperLoadParams};

use crate::model;

/// Per-take hints; Parakeet ignores them.
#[derive(Default)]
pub struct Hints {
    /// Vocabulary for Whisper's initial prompt (spelling of names and terms).
    pub prompt: Option<String>,
    /// Pinned language ("de"), None = detect.
    pub language: Option<String>,
}

pub enum Stt {
    Parakeet(ParakeetModel),
    Whisper(WhisperEngine),
}

/// Whole outputs Whisper is known to invent on (near) silence.
const HALLUCINATIONS: &[&str] = &[
    "untertitel im auftrag des zdf",
    "untertitelung des zdf",
    "untertitel der amara.org-community",
    "untertitel von stephanie geiges",
    "vielen dank fürs zuschauen",
    "vielen dank für's zuschauen",
    "danke fürs zuschauen",
    "bis zum nächsten mal",
    "thank you for watching",
    "thanks for watching",
    "subtitles by the amara.org community",
];

impl Stt {
    pub fn load(engine: &str, gpu: bool) -> Result<Self> {
        match engine {
            "whisper" => {
                let path = model::WHISPER.path();
                // Flash attention leaves the 30 s padding unmasked (whisper.cpp #3941).
                let params = WhisperLoadParams { use_gpu: gpu, flash_attn: false, ..Default::default() };
                let mut stt = WhisperEngine::load_with_params(&path, params)
                    .map(Self::Whisper)
                    .map_err(|e| anyhow!("loading Whisper from {}: {e}", path.display()))?;
                // The first GPU run compiles shaders (seconds); do it now, not on the first take.
                if gpu {
                    let _ = stt.transcribe(&[0.0; 8000], &Hints::default());
                }
                Ok(stt)
            }
            "parakeet" => {
                let dir = model::PARAKEET.dir();
                ParakeetModel::load(&dir, &Quantization::Int8)
                    .map(Self::Parakeet)
                    .map_err(|e| anyhow!("loading Parakeet from {}: {e}", dir.display()))
            }
            other => bail!("unknown speech engine {other:?} (parakeet | whisper)"),
        }
    }

    pub fn transcribe(&mut self, samples: &[f32], hints: &Hints) -> Result<String> {
        match self {
            Self::Parakeet(m) => {
                let result = m
                    .transcribe_with(samples, &ParakeetParams::default())
                    .map_err(|e| anyhow!("transcription failed: {e}"))?;
                Ok(result.text.trim().to_string())
            }
            Self::Whisper(m) => {
                let params = WhisperInferenceParams {
                    language: hints.language.clone(),
                    initial_prompt: hints.prompt.clone(),
                    n_threads: physical_cores(),
                    suppress_non_speech_tokens: true,
                    ..Default::default()
                };
                let result = m.transcribe_with(samples, &params).map_err(|e| anyhow!("transcription failed: {e}"))?;
                let text = result.text.trim().to_string();
                Ok(if is_hallucination(&text, hints.prompt.as_deref()) { String::new() } else { text })
            }
        }
    }
}

fn is_hallucination(text: &str, prompt: Option<&str>) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches(['.', '!', ',']).to_lowercase();
    let t = norm(text);
    HALLUCINATIONS.iter().any(|h| t.starts_with(h)) || prompt.is_some_and(|p| norm(p) == t)
}

pub fn physical_cores() -> i32 {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut cores = std::collections::HashSet::new();
    let mut phys = "";
    for line in cpuinfo.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "physical id" => phys = v.trim(),
                "core id" => {
                    cores.insert((phys.to_string(), v.trim().to_string()));
                }
                _ => {}
            }
        }
    }
    if cores.is_empty() { 4 } else { cores.len() as i32 }
}
