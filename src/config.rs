//! User settings in ~/.config/lightwispr/config.toml. Written by the daemon when changed
//! from the bar; hand edits are picked up on restart.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// UI sounds on/off.
    pub sound: bool,
    /// UI sound volume 0..1.
    pub volume: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self { sound: true, volume: 0.35 }
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("lightwispr/config.toml")
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
}
