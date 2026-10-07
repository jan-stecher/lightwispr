//! The background service: one control thread owns the state machine; recording,
//! transcription, sounds, Wayland and Hyprland events run on helper threads and talk
//! to it through a channel.
//!
//! Push-to-talk (Right Ctrl by default): on key-down the mic opens silently ("armed"), so
//! a quick tap does nothing. After ARM_DELAY it becomes a real recording (start sound) and
//! the daemon switches Hyprland into the "lightwispr" submap: releasing the key finishes,
//! Esc or any other key cancels. Those submap binds also leave the submap on their own, so
//! the keyboard can't get stuck. Workspace/window events (a chord with a bound key)
//! discard an armed take silently.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::{self, Level, Recording};
use crate::config::Config;
use crate::deliver::{self, Delivered};
use crate::ime::Ime;
use crate::sound::{Cue, Player};
use crate::stt::Stt;
use crate::{hypr, ipc};

const ARM_DELAY: Duration = Duration::from_millis(250);
/// Shorter recordings (after arming) are treated as accidental.
const MIN_SPEECH: Duration = Duration::from_millis(300);
/// Below this peak RMS the recording is considered silence.
const SILENCE_RMS: f32 = 0.004;
const HISTORY_MAX: usize = 10;
/// Safety net: a dictation never runs longer than this.
const MAX_RECORDING: Duration = Duration::from_secs(300);
const SUBMAP: &str = "lightwispr";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Idle,
    Armed,
    Recording,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Ptt,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Typed,
    Clipboard,
    Empty,
    Cancelled,
    Error,
}

enum Msg {
    Cmd(String, Sender<String>),
    ArmTimeout(u64),
    MaxDuration(u64),
    Chord,
    Done(u64, Result<(String, Delivered)>),
    Tick,
}

struct Job {
    generation: u64,
    samples: Vec<f32>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    text: String,
    at: u64,
    delivered: String,
}

struct Daemon {
    tx: Sender<Msg>,
    jobs: Sender<Work>,
    sound: Player,
    level: Level,
    phase: Phase,
    trigger: Trigger,
    recording: Option<Recording>,
    recording_since: Instant,
    /// Bumped whenever the current take is abandoned; stale results are dropped.
    generation: Arc<AtomicU64>,
    last: Option<Outcome>,
    last_at: u64,
    in_submap: bool,
    config: Config,
    ready: Arc<AtomicBool>,
    /// The `ready` value the bar last saw in the state file.
    ready_written: bool,
}

pub fn run() -> Result<()> {
    let dir = ipc::runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let sock = ipc::socket_path();
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        anyhow::bail!("already running");
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).context("binding socket")?;

    let (tx, rx) = mpsc::channel::<Msg>();
    let generation = Arc::new(AtomicU64::new(0));

    // Input method for typing into text fields (optional: clipboard works without it).
    let ime = match Ime::spawn(false) {
        Ok(ime) => Some(Arc::new(ime)),
        Err(e) => {
            eprintln!("input method unavailable, clipboard only: {e:#}");
            None
        }
    };

    let ready = Arc::new(AtomicBool::new(false));
    let jobs = spawn_stt_worker(tx.clone(), generation.clone(), ime, ready.clone());
    {
        let tx = tx.clone();
        hypr::watch(move || {
            let _ = tx.send(Msg::Chord);
        });
    }
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("ipc".into()).spawn(move || accept_loop(listener, tx))?;
    }
    {
        // Drives the live level in the bar state while recording.
        let tx = tx.clone();
        std::thread::Builder::new().name("tick".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_millis(80));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        })?;
    }

    let config = Config::load();
    if config.enabled {
        let _ = jobs.send(Work::Load);
    }
    let mut d = Daemon {
        tx,
        jobs,
        sound: Player::new(config.volume, config.sound),
        level: Level::default(),
        phase: Phase::Idle,
        trigger: Trigger::Ptt,
        recording: None,
        recording_since: Instant::now(),
        generation,
        last: None,
        last_at: 0,
        in_submap: false,
        config,
        ready,
        ready_written: false,
    };
    // Recover from a crash mid-dictation.
    if current_submap().as_deref() == Some(SUBMAP) {
        hyprctl_dispatch("hl.dsp.submap('reset')");
    }
    d.write_state();
    eprintln!("lightwispr ready ({})", sock.display());
    d.event_loop(rx);
    Ok(())
}

fn accept_loop(listener: UnixListener, tx: Sender<Msg>) {
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let (rtx, rrx) = mpsc::channel();
            if tx.send(Msg::Cmd(line.trim().to_string(), rtx)).is_err() {
                return;
            }
            if let Ok(reply) = rrx.recv_timeout(Duration::from_secs(5)) {
                let _ = writeln!(&stream, "{reply}");
            }
        });
    }
}

enum Work {
    Transcribe(Job),
    /// Load the model (power on).
    Load,
    /// Drop the model and hand its memory back to the OS (power off).
    Unload,
}

fn spawn_stt_worker(tx: Sender<Msg>, generation: Arc<AtomicU64>, ime: Option<Arc<Ime>>, ready: Arc<AtomicBool>) -> Sender<Work> {
    let (wtx, wrx) = mpsc::channel::<Work>();
    std::thread::Builder::new()
        .name("stt".into())
        .spawn(move || {
            let mut stt: Option<Stt> = None;
            for work in wrx {
                match work {
                    Work::Load if stt.is_none() => {
                        let t = Instant::now();
                        match Stt::load() {
                            Ok(m) => {
                                eprintln!("model loaded in {:.2?}", t.elapsed());
                                stt = Some(m);
                                ready.store(true, Ordering::SeqCst);
                            }
                            Err(e) => eprintln!("{e:#}"),
                        }
                        let _ = tx.send(Msg::Tick);
                    }
                    Work::Load => {}
                    Work::Unload => {
                        if stt.take().is_some() {
                            ready.store(false, Ordering::SeqCst);
                            release_memory();
                            eprintln!("model unloaded");
                        }
                        let _ = tx.send(Msg::Tick);
                    }
                    Work::Transcribe(job) => {
                        let result = match stt.as_mut() {
                            None => Err(anyhow::anyhow!("model not loaded")),
                            Some(stt) => stt.transcribe(&job.samples).and_then(|text| {
                                if text.is_empty() || generation.load(Ordering::SeqCst) != job.generation {
                                    return Ok((text, Delivered::Clipboard));
                                }
                                let how = deliver::deliver(ime.as_deref(), &text)?;
                                Ok((text, how))
                            }),
                        };
                        let _ = tx.send(Msg::Done(job.generation, result));
                    }
                }
            }
        })
        .expect("spawn stt thread");
    wtx
}

/// glibc keeps freed heap pages around; ask it to return them so "off" really frees RAM.
fn release_memory() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
}

impl Daemon {
    fn event_loop(&mut self, rx: Receiver<Msg>) {
        for msg in rx {
            match msg {
                Msg::Cmd(cmd, reply) => {
                    let r = self.command(&cmd);
                    let _ = reply.send(r);
                }
                Msg::MaxDuration(g) => {
                    if self.phase == Phase::Recording && self.current() == g {
                        self.finish();
                    }
                }
                Msg::ArmTimeout(g) => {
                    if self.phase == Phase::Armed && self.current() == g {
                        self.sound.play(Cue::Start);
                        self.set_phase(Phase::Recording);
                    }
                }
                Msg::Chord => match (self.phase, self.trigger) {
                    (Phase::Armed, _) => self.discard(false),
                    (Phase::Recording, Trigger::Ptt) => self.discard(true),
                    _ => {}
                },
                Msg::Done(g, result) => {
                    if self.phase == Phase::Processing && self.current() == g {
                        let outcome = match result {
                            Ok((text, _)) if text.is_empty() => {
                                self.sound.play(Cue::Cancel);
                                Outcome::Empty
                            }
                            Ok((text, how)) => {
                                self.remember(&text, how);
                                match how {
                                    Delivered::Typed => Outcome::Typed,
                                    Delivered::Clipboard => Outcome::Clipboard,
                                }
                            }
                            Err(e) => {
                                eprintln!("{e:#}");
                                self.sound.play(Cue::Error);
                                Outcome::Error
                            }
                        };
                        self.set_last(outcome);
                        self.set_phase(Phase::Idle);
                    }
                }
                Msg::Tick => {
                    let ready = self.ready.load(Ordering::SeqCst);
                    if self.phase == Phase::Recording || ready != self.ready_written {
                        self.ready_written = ready;
                        self.write_state();
                    }
                }
            }
        }
    }

    fn current(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn command(&mut self, line: &str) -> String {
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next();
        match cmd {
            "ptt-down" => {
                if self.phase == Phase::Idle && self.config.enabled {
                    self.begin(Trigger::Ptt);
                }
            }
            "ptt-up" => match (self.phase, self.trigger) {
                (Phase::Armed, Trigger::Ptt) => self.discard(false),
                (Phase::Recording, Trigger::Ptt) => self.finish(),
                _ => {}
            },
            "toggle" => match self.phase {
                Phase::Idle if !self.config.enabled => {}
                Phase::Idle => self.begin(Trigger::Toggle),
                Phase::Armed | Phase::Recording => self.finish(),
                Phase::Processing => {}
            },
            "cancel" => match self.phase {
                Phase::Armed => self.discard(false),
                Phase::Recording => self.discard(true),
                Phase::Processing => {
                    self.generation.fetch_add(1, Ordering::SeqCst);
                    self.sound.play(Cue::Cancel);
                    self.set_last(Outcome::Cancelled);
                    self.set_phase(Phase::Idle);
                }
                Phase::Idle => {}
            },
            "status" => {}
            "history" => return serde_json::to_string(&load_history()).unwrap_or_default(),
            "power" => {
                let on = match arg {
                    Some("on") => true,
                    Some("off") => false,
                    _ => !self.config.enabled,
                };
                if on != self.config.enabled {
                    if !on {
                        match self.phase {
                            Phase::Armed => self.discard(false),
                            Phase::Recording => self.discard(true),
                            _ => {}
                        }
                    }
                    self.config.enabled = on;
                    self.config.save();
                    let _ = self.jobs.send(if on { Work::Load } else { Work::Unload });
                    self.write_state();
                }
            }
            "clear-history" => {
                let _ = std::fs::remove_file(history_path());
            }
            "sound" => {
                self.config.sound = match arg {
                    Some("on") => true,
                    Some("off") => false,
                    _ => !self.config.sound,
                };
                self.sound.set_enabled(self.config.sound);
                self.config.save();
                self.write_state();
            }
            "volume" => {
                let Some(v) = arg.and_then(|a| a.parse::<f32>().ok()) else {
                    return json!({ "ok": false, "error": "usage: volume <0..1>" }).to_string();
                };
                self.config.volume = v.clamp(0.0, 1.0);
                self.sound.set_volume(self.config.volume);
                self.config.save();
                self.write_state();
                // Let the user hear the new level.
                self.sound.play(Cue::Start);
            }
            "quit" => {
                self.leave_submap();
                std::process::exit(0);
            }
            other => return json!({ "ok": false, "error": format!("unknown command: {other}") }).to_string(),
        }
        self.status().to_string()
    }

    fn begin(&mut self, trigger: Trigger) {
        let g = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        match Recording::start(self.level.clone()) {
            Ok(rec) => {
                self.recording = Some(rec);
                self.recording_since = Instant::now();
                self.trigger = trigger;
                match trigger {
                    Trigger::Toggle => {
                        self.sound.play(Cue::Start);
                        self.set_phase(Phase::Recording);
                    }
                    Trigger::Ptt => {
                        self.set_phase(Phase::Armed);
                        let tx = self.tx.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(ARM_DELAY);
                            let _ = tx.send(Msg::ArmTimeout(g));
                        });
                    }
                }
            }
            Err(e) => {
                eprintln!("recording failed: {e:#}");
                self.sound.play(Cue::Error);
                self.set_last(Outcome::Error);
                self.set_phase(Phase::Idle);
            }
        }
    }

    /// Throws the current take away. `audible` plays the cancel sound (not for silent chords).
    fn discard(&mut self, audible: bool) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(rec) = self.recording.take() {
            drop(rec.finish());
        }
        if audible {
            self.sound.play(Cue::Cancel);
            self.set_last(Outcome::Cancelled);
        }
        self.set_phase(Phase::Idle);
    }

    fn finish(&mut self) {
        let Some(rec) = self.recording.take() else { return };
        let armed_for = self.recording_since.elapsed();
        let samples = rec.finish();
        let min = match self.trigger {
            Trigger::Ptt => ARM_DELAY + MIN_SPEECH,
            Trigger::Toggle => MIN_SPEECH,
        };
        if armed_for < min || audio::peak_rms(&samples) < SILENCE_RMS {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.sound.play(Cue::Cancel);
            self.set_last(Outcome::Empty);
            self.set_phase(Phase::Idle);
            return;
        }
        self.sound.play(Cue::Stop);
        self.set_phase(Phase::Processing);
        let _ = self.jobs.send(Work::Transcribe(Job { generation: self.current(), samples }));
    }

    fn set_last(&mut self, outcome: Outcome) {
        self.last = Some(outcome);
        self.last_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    }

    fn set_phase(&mut self, phase: Phase) {
        let entering = phase == Phase::Recording && self.phase != Phase::Recording;
        self.phase = phase;
        if entering {
            self.enter_submap();
            let (tx, g) = (self.tx.clone(), self.current());
            std::thread::spawn(move || {
                std::thread::sleep(MAX_RECORDING);
                let _ = tx.send(Msg::MaxDuration(g));
            });
        } else if phase != Phase::Recording {
            self.leave_submap();
        }
        self.write_state();
    }

    /// While recording, keys belong to the dictation (release = done, anything else = cancel).
    fn enter_submap(&mut self) {
        self.in_submap = hyprctl_dispatch(&format!("hl.dsp.submap('{SUBMAP}')"));
    }

    fn leave_submap(&mut self) {
        if self.in_submap {
            // The submap binds usually reset it already; only reset if we're still in it,
            // so we never kick the user out of a submap of their own.
            if current_submap().as_deref() == Some(SUBMAP) {
                hyprctl_dispatch("hl.dsp.submap('reset')");
            }
            self.in_submap = false;
        }
    }

    fn status(&self) -> serde_json::Value {
        json!({
            "ok": true,
            "enabled": self.config.enabled,
            "ready": self.ready.load(Ordering::SeqCst),
            "phase": self.phase,
            "level": if self.phase == Phase::Recording { (self.level.get() as f64 * 100.0).round() / 100.0 } else { 0.0 },
            "last": self.last,
            "last_at": self.last_at,
            "sound": self.config.sound,
            "volume": (self.config.volume as f64 * 100.0).round() / 100.0,
        })
    }

    /// The bar reads this file (no socket round-trip needed for polling).
    fn write_state(&self) {
        let path = ipc::runtime_dir().join("state.json");
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, self.status().to_string()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    fn remember(&self, text: &str, how: Delivered) {
        let mut h = load_history();
        h.insert(0, Entry {
            text: text.to_string(),
            at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            delivered: serde_json::to_value(how).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
        });
        h.truncate(HISTORY_MAX);
        let path = history_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(&h).unwrap_or_default());
    }
}

fn hyprctl_dispatch(lua: &str) -> bool {
    std::process::Command::new("hyprctl")
        .args(["dispatch", lua])
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn current_submap() -> Option<String> {
    let out = std::process::Command::new("hyprctl").arg("submap").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn history_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("lightwispr/history.json")
}

fn load_history() -> Vec<Entry> {
    std::fs::read_to_string(history_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
