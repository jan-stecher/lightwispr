//! lightwispr: minimal local voice dictation for Linux.

mod audio;
mod config;
mod context;
mod daemon;
mod deliver;
mod enhance;
mod eval;
mod hotkey;
mod hypr;
mod ime;
mod ipc;
mod model;
mod sound;
mod stt;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

const USAGE: &str = "usage: lightwispr <command>

  daemon                 run the background service
  download-model [which] fetch a model, checksum-verified: parakeet (default, ~670 MB),
                         whisper (~575 MB), enhance (the configured LLM) or a model id
  ptt-down | ptt-up      push-to-talk key pressed / released (for the hotkey)
  toggle                 start or stop a recording
  cancel                 abort the current recording
  status                 print the state as JSON
  history                print the last transcriptions as JSON
  clear-history          forget the stored transcriptions
  power on|off           load / unload the model (off frees its RAM, hotkey does nothing)
  hotkey <preset>        push-to-talk key: right_ctrl | ctrl_super | super_alt
  stt parakeet|whisper   speech engine (the daemon restarts itself)
  mode off|<mode>        enhancement mode: casual | formal | coding | your own
  context on|off         use the focused text field and window as context
  gpu on|off             Whisper and the enhancement model on the GPU (Vulkan builds)
  download <which>       like download-model, but in the daemon (progress in the bar)
  field                  show what the daemon sees of the focused text field (diagnostics)
  sound on|off           UI sounds
  volume <0..1>          UI sound volume
  quit                   stop the daemon

  sounds                 play all UI sounds (preview)
  transcribe <file.wav> [parakeet|whisper] [--cpu]
                         transcribe a 16 kHz mono WAV and print the text
  enhance [opts] <text>  run one text through the enhancement model and print the result
                         (--mode m, --before t, --after t, --app a, --model id|path, --gpu|--cpu)
  eval [opts] [file]     run the eval set (eval/dictations.toml) against a model
  mode-file <id|base>    path of a mode's prompt file (created from the built-in to edit)
  prompt [mode]          print the full prompt a mode sends to the model
  ime-watch              show when text fields gain/lose focus
  deliver <text> [secs]  type <text> into the focused field (else clipboard) after a delay";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("daemon") => {
            let cfg = config::Config::load();
            let spec = if cfg.stt == "whisper" { &model::WHISPER } else { &model::PARAKEET };
            if !spec.is_installed() {
                eprintln!("model missing: run `lightwispr download-model {}` first", cfg.stt);
            }
            daemon::run()
        }
        Some("download-model") => {
            let cfg = config::Config::load();
            model::download_cli(arg(1), &cfg.enhance.model, cfg.gpu)
        }
        Some(
            "ptt-down" | "ptt-up" | "toggle" | "cancel" | "status" | "history" | "clear-history" | "power" | "hotkey" | "sound"
            | "volume" | "stt" | "mode" | "modes" | "context" | "gpu" | "download" | "field" | "quit",
        ) => {
            println!("{}", ipc::request(&args.join(" "))?);
            Ok(())
        }
        Some("sounds") => cmd_sounds(),
        Some("enhance") => eval::enhance_cli(&args[1..]),
        Some("eval") => eval::eval_cli(&args[1..]),
        Some("prompt") => eval::prompt_cli(arg(1)),
        Some("mode-file") => {
            println!("{}", enhance::mode_file(arg(1).context("usage: lightwispr mode-file <id|base>")?)?.display());
            Ok(())
        }
        Some("transcribe") => {
            let path = arg(1).context("usage: lightwispr transcribe <file.wav> [parakeet|whisper] [--cpu]")?;
            let engine = args.iter().skip(2).find(|a| !a.starts_with("--")).cloned();
            cmd_transcribe(Path::new(path), engine, args.iter().any(|a| a == "--cpu"))
        }
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

fn cmd_transcribe(path: &Path, engine: Option<String>, cpu: bool) -> Result<()> {
    let samples = transcribe_rs::audio::read_wav_samples(path)
        .map_err(|e| anyhow!("reading {} (needs 16 kHz mono): {e}", path.display()))?;
    let cfg = config::Config::load();
    let engine = engine.unwrap_or_else(|| cfg.stt.clone());
    let t = Instant::now();
    let mut stt = stt::Stt::load(&engine, cfg.gpu && !cpu)?;
    eprintln!("{engine} loaded in {:.2?}", t.elapsed());
    let t = Instant::now();
    let hints = stt::Hints { prompt: enhance::Vocabulary::load().prompt(), language: cfg.whisper_language() };
    let text = stt.transcribe(&samples, &hints)?;
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
