//! Push-to-talk hotkey presets, registered in Hyprland by the daemon itself (via
//! `hyprctl eval`), so nobody has to edit their Hyprland config. Hyprland drops runtime
//! binds on a config reload; the daemon re-registers on the `configreloaded` event.

use std::process::{Command, Stdio};

pub struct Preset {
    pub id: &'static str,
    /// Binds that start a take (key or "MODS + key").
    down: &'static [&'static str],
    /// Keys whose release ends it. Hyprland skips a release bind when another key was
    /// pressed in between, so chords (Super+Ctrl+Left …) never end up here.
    up: &'static [&'static str],
}

pub const PRESETS: &[Preset] = &[
    Preset { id: "right_ctrl", down: &["Control_R"], up: &["Control_R"] },
    Preset { id: "ctrl_super", down: &["SUPER + Control_L", "CONTROL + Super_L"], up: &["Control_L", "Super_L"] },
    Preset { id: "super_alt", down: &["SUPER + Alt_L", "ALT + Super_L"], up: &["Alt_L", "Super_L"] },
];

pub const DEFAULT: &str = "right_ctrl";
pub const SUBMAP: &str = "lightwispr";

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Registers the preset's binds plus the "lightwispr" submap used while recording:
/// releasing the key finishes, any other key cancels. Both submap binds leave the
/// submap themselves, so the keyboard can never get stuck, even without the daemon.
pub fn register(preset: &Preset) -> bool {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "lightwispr".into());
    let mut lua = format!(
        "local lw = {exe:?} .. ' '\n\
         local function leave(cmd) return function() hl.dispatch(hl.dsp.submap('reset')); hl.exec_cmd(lw .. cmd) end end\n"
    );
    for key in preset.down {
        let ignore = !key.contains('+');
        lua += &format!(
            "hl.bind({key:?}, hl.dsp.exec_cmd(lw .. 'ptt-down'), {{ ignore_mods = {ignore}, non_consuming = true, description = 'Dictation: hold to talk' }})\n"
        );
    }
    for key in preset.up {
        lua += &format!(
            "hl.bind({key:?}, hl.dsp.exec_cmd(lw .. 'ptt-up'), {{ release = true, ignore_mods = true, non_consuming = true, description = 'Dictation: release to transcribe' }})\n"
        );
    }
    lua += &format!("hl.define_submap('{SUBMAP}', function()\n");
    for key in preset.up {
        lua += &format!(
            "  hl.bind({key:?}, leave('ptt-up'), {{ release = true, ignore_mods = true, description = 'Dictation: release to transcribe' }})\n"
        );
    }
    lua += "  hl.bind('catchall', leave('cancel'), { description = 'Dictation: any other key cancels' })\nend)\n";
    hyprctl(&["eval", &lua])
}

/// Switching presets: reload the config (drops the old runtime binds); the daemon
/// registers the new preset when Hyprland reports `configreloaded`.
pub fn reload_config() -> bool {
    hyprctl(&["reload", "config-only"])
}

fn hyprctl(args: &[&str]) -> bool {
    Command::new("hyprctl")
        .args(args)
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
