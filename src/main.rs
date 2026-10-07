//! lightwispr: minimal local voice dictation for Linux.

mod audio;
mod config;
mod daemon;
mod deliver;
mod hotkey;
mod hypr;
mod ime;
mod ipc;
mod sound;
mod stt;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

const USAGE: &str = "usage: lightwispr <command>

  daemon                 run the background service
  ptt-down | ptt-up      push-to-talk key pressed / released (for the hotkey)
  toggle                 start or stop a recording
  cancel                 abort the current recording
  status                 print the state as JSON
  history                print the last transcriptions as JSON
  clear-history          forget the stored transcriptions
  power on|off           load / unload the model (off frees its RAM, hotkey does nothing)
  hotkey <preset>        push-to-talk key: right_ctrl | ctrl_super | super_alt
  sound on|off           UI sounds
  volume <0..1>          UI sound volume
  quit                   stop the daemon

  sounds                 play all UI sounds (preview)
  transcribe <file.wav>  transcribe a 16 kHz mono WAV and print the text
  ime-watch              show when text fields gain/lose focus
  deliver <text> [secs]  type <text> into the focused field (else clipboard) after a delay";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("daemon") => daemon::run(),
        Some("ptt-down" | "ptt-up" | "toggle" | "cancel" | "status" | "history" | "clear-history" | "power" | "hotkey" | "sound" | "volume" | "quit") => {
            println!("{}", ipc::request(&args.join(" "))?);
            Ok(())
        }
        Some("sounds") => cmd_sounds(),
        Some("transcribe") => cmd_transcribe(Path::new(arg(1).context("usage: lightwispr transcribe <file.wav>")?)),
        Some("ime-watch") => cmd_ime_watch(),
        Some("deliver") => {
            let text = arg(1).context("usage: lightwispr deliver <text> [delay-secs]")?;
            cmd_deliver(text, arg(2).and_then(|s| s.parse().ok()).unwrap_or(0))
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn cmd_sounds() -> Result<()> {
    let cfg = config::Config::load();
    let player = sound::Player::new(cfg.volume, true);
    for cue in sound::Cue::ALL {
        eprintln!("{cue:?}");
        player.play(cue);
        std::thread::sleep(Duration::from_millis(900));
    }
    Ok(())
}

fn cmd_transcribe(path: &Path) -> Result<()> {
    let samples = transcribe_rs::audio::read_wav_samples(path)
        .map_err(|e| anyhow!("reading {} (needs 16 kHz mono): {e}", path.display()))?;
    let t = Instant::now();
    let mut stt = stt::Stt::load()?;
    eprintln!("model loaded in {:.2?}", t.elapsed());
    let t = Instant::now();
    let text = stt.transcribe(&samples)?;
    eprintln!("transcribed {:.1}s audio in {:.2?}", samples.len() as f64 / audio::TARGET_RATE as f64, t.elapsed());
    println!("{text}");
    Ok(())
}

fn cmd_ime_watch() -> Result<()> {
    let ime = ime::Ime::spawn(true)?;
    eprintln!("watching (focus text fields in other apps, Ctrl+C to stop); now: {}", if ime.is_active() { "focused" } else { "unfocused" });
    while !ime.is_gone() {
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn cmd_deliver(text: &str, delay: u64) -> Result<()> {
    let ime = ime::Ime::spawn(false).map_err(|e| eprintln!("input method unavailable: {e:#}")).ok();
    std::thread::sleep(Duration::from_secs(delay));
    let how = deliver::deliver(ime.as_ref(), text)?;
    eprintln!("{how:?}");
    Ok(())
}
