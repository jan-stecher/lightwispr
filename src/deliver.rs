//! Puts text where the user is: into the focused text field, otherwise on the clipboard.
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
    Clipboard,
}

/// How long to wait for a text field to regain focus (e.g. after a bar panel closes).
const REFOCUS_GRACE: Duration = Duration::from_millis(300);

pub fn deliver(ime: Option<&Ime>, text: &str) -> Result<Delivered> {
    if let Some(ime) = ime {
        if ime.wait_typable(REFOCUS_GRACE) {
            ime.commit(text)?;
            return Ok(Delivered::Typed);
        }
    }
    copy_to_clipboard(text)?;
    Ok(Delivered::Clipboard)
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
