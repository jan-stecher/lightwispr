//! Puts text where the user is:
//! 1. a focused text field that speaks text-input-v3 → committed directly (input method),
//! 2. an XWayland window, or a native app known to lack input-method support
//!    (PASTE_APPS) → clipboard + paste shortcut sent by Hyprland, then the previous
//!    clipboard text is restored,
//! 3. anything else (no text field) → left on the clipboard.
//!
//! Password/PIN fields never get typed into (clipboard instead).

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::ime::Ime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivered {
    Typed,
    Pasted,
    Clipboard,
}

/// How long to wait for a text field to regain focus (e.g. after a bar panel closes).
const REFOCUS_GRACE: Duration = Duration::from_millis(300);
/// Give the target a moment to see the new clipboard offer before pasting.
const BEFORE_PASTE: Duration = Duration::from_millis(60);
/// XWayland reads the selection on demand; restoring too early would paste the old text.
const BEFORE_RESTORE: Duration = Duration::from_millis(900);

/// Native Wayland apps that ignore input methods but accept a paste shortcut.
/// Warp: winit IME is never enabled on Linux (warpdotdev/warp#9383).
const PASTE_APPS: &[&str] = &["dev.warp.warp"];

/// Terminals paste with Ctrl+Shift+V (Ctrl+V is a control character there).
const TERMINALS: &[&str] = &[
    "dev.warp.warp",
    "kitty",
    "alacritty",
    "foot",
    "footclient",
    "org.wezfurlong.wezterm",
    "com.mitchellh.ghostty",
    "org.gnome.console",
    "org.gnome.terminal",
    "gnome-terminal-server",
    "org.kde.konsole",
    "konsole",
    "xterm",
    "urxvt",
    "com.gexperts.tilix",
    "terminator",
];

pub fn deliver(ime: Option<&Ime>, text: &str) -> Result<Delivered> {
    if let Some(ime) = ime {
        if ime.wait_typable(REFOCUS_GRACE) {
            ime.commit(text)?;
            return Ok(Delivered::Typed);
        }
    }
    if let Some(win) = active_window() {
        if win.xwayland || PASTE_APPS.contains(&win.class.to_lowercase().as_str()) {
            paste(text, &win.class)?;
            return Ok(Delivered::Pasted);
        }
    }
    copy_to_clipboard(text)?;
    Ok(Delivered::Clipboard)
}

struct Window {
    class: String,
    xwayland: bool,
}

fn active_window() -> Option<Window> {
    let out = Command::new("hyprctl").args(["activewindow", "-j"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(Window {
        class: v.get("class")?.as_str()?.to_string(),
        xwayland: v.get("xwayland")?.as_bool()?,
    })
}

fn paste(text: &str, class: &str) -> Result<()> {
    let previous = clipboard_text();
    copy_to_clipboard(text)?;
    std::thread::sleep(BEFORE_PASTE);
    let mods = if TERMINALS.contains(&class.to_lowercase().as_str()) { "CTRL SHIFT" } else { "CTRL" };
    let status = Command::new("hyprctl")
        .args(["dispatch", &format!("hl.dsp.send_shortcut({{ mods = \"{mods}\", key = \"V\" }})")])
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("hyprctl send_shortcut failed");
    }
    // Put the user's clipboard back once the paste has been served.
    if let Some(prev) = previous {
        let ours = text.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(BEFORE_RESTORE);
            // Only if nobody copied something else in the meantime.
            if clipboard_text().as_deref() == Some(ours.as_str()) {
                let _ = copy_to_clipboard(&prev);
            }
        });
    }
    Ok(())
}

/// Current clipboard as text, if it holds text.
fn clipboard_text() -> Option<String> {
    let types = Command::new("wl-paste").arg("--list-types").output().ok()?;
    let types = String::from_utf8_lossy(&types.stdout);
    if !types.lines().any(|t| t.starts_with("text/plain") || t == "UTF8_STRING" || t == "TEXT" || t == "STRING") {
        return None;
    }
    let out = Command::new("wl-paste").args(["--no-newline", "--type", "text"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("running wl-copy (install wl-clipboard)")?;
    child.stdin.take().context("wl-copy stdin")?.write_all(text.as_bytes())?;
    if !child.wait()?.success() {
        bail!("wl-copy failed");
    }
    Ok(())
}
