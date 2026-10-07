//! Watches Hyprland's event socket for workspace/window moves. Those come from
//! Super+Ctrl+<key> binds, so they mean "this was a chord, not push-to-talk".

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const CHORD_EVENTS: &[&str] = &["workspace", "workspacev2", "movewindow", "movewindowv2", "activespecial", "activespecialv2"];

fn socket_path() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("hypr").join(sig).join(".socket2.sock"))
}

/// Calls `on_chord` for every relevant event. Reconnects if Hyprland restarts.
pub fn watch(on_chord: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("hypr-events".into())
        .spawn(move || loop {
            let Some(path) = socket_path() else { return };
            if let Ok(stream) = UnixStream::connect(&path) {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if let Some((event, _)) = line.split_once(">>") {
                        if CHORD_EVENTS.contains(&event) {
                            on_chord();
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        })
        .expect("spawn hypr thread");
}
