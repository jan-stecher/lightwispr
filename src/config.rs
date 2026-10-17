//! User settings in ~/.config/lightwispr/config.toml. Written by the daemon when changed
//! from the bar; hand edits are picked up on restart.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Off = models unloaded (frees their RAM) and the hotkey does nothing.
    pub enabled: bool,
    /// Push-to-talk preset (see hotkey.rs): right_ctrl | ctrl_super | super_alt.
    pub hotkey: String,
    /// UI sounds on/off.
    pub sound: bool,
    /// UI sound volume 0..1.
    pub volume: f32,
    /// Speech engine: parakeet | whisper.
    pub stt: String,
    /// Run Whisper and the enhancement model on the GPU (builds with the `vulkan` feature).
    pub gpu: bool,
    pub whisper: Whisper,
    pub enhance: Enhance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Whisper {
    /// "auto" detects per take; "de", "en", … pins the language.
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Enhance {
    /// off | casual | formal | coding | <id of a file in ~/.config/lightwispr/modes/>
    pub mode: String,
    /// Registry id (see model.rs); empty = the default for CPU/GPU.
    pub model: String,
    /// Use the focused text field and window as context.
    pub context: bool,
    /// Shorter dictations are delivered as transcribed.
    pub min_words: usize,
    /// After this, the raw transcript is delivered instead.
    pub timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: crate::hotkey::DEFAULT.to_string(),
            sound: true,
            volume: 0.35,
            stt: "parakeet".into(),
            gpu: cfg!(feature = "vulkan"),
            whisper: Whisper::default(),
            enhance: Enhance::default(),
        }
    }
}

impl Default for Whisper {
    fn default() -> Self {
        Self { language: "auto".into() }
    }
}

impl Default for Enhance {
    fn default() -> Self {
        Self { mode: "off".into(), model: String::new(), context: true, min_words: 3, timeout_ms: 8000 }
    }
}

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("lightwispr")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

impl Config {
    pub fn load() -> Self {
        match std::fs::read_to_string(path()) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                eprintln!("config.toml: {e}; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }

    /// The enhancement model to use (registry entry).
    pub fn llm(&self) -> &'static crate::model::Spec {
        crate::model::find(&self.enhance.model)
            .filter(|s| matches!(s.kind, crate::model::Kind::Llm))
            .unwrap_or_else(|| crate::model::default_llm(self.gpu))
    }

    pub fn whisper_language(&self) -> Option<String> {
        Some(self.whisper.language.clone()).filter(|l| !l.is_empty() && l != "auto")
    }
}
