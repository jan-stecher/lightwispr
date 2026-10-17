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
//!
//! After a take, the pipeline thread transcribes (Parakeet or Whisper), applies the
//! vocabulary, optionally lets the LLM worker rewrite the text in the selected mode (with
//! the focused field and window as context), and delivers it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::{self, Level, Recording};
use crate::config::Config;
use crate::context::{self, Field, Recent};
use crate::deliver::{self, Delivered};
use crate::enhance::{self, Enhancer, Failure, Vocabulary};
use crate::ime::Ime;
use crate::sound::{Cue, Player};
use crate::stt::{Hints, Stt};
use crate::{hypr, ipc, model};

const ARM_DELAY: Duration = Duration::from_millis(250);
/// Shorter recordings (after arming) are treated as accidental.
const MIN_SPEECH: Duration = Duration::from_millis(300);
/// Below this peak RMS the recording is considered silence.
const SILENCE_RMS: f32 = 0.004;
const HISTORY_MAX: usize = 10;
/// Safety net: a dictation never runs longer than this.
const MAX_RECORDING: Duration = Duration::from_secs(300);
/// How long to wait for a text field to regain focus (e.g. after a bar panel closes).
const REFOCUS_GRACE: Duration = Duration::from_millis(300);
/// How long to wait for the field to report fresh surrounding text.
const SURROUNDING_WAIT: Duration = Duration::from_millis(60);
/// The mode list and installed models in the state file are refreshed this often.
const CATALOG_TTL: Duration = Duration::from_secs(5);
use crate::hotkey::{self, SUBMAP};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Idle,
    Armed,
    Recording,
    Processing,
    Enhancing,
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
    /// Hyprland reloaded its config: our runtime binds are gone.
    Reloaded,
    Chord,
    /// The transcript went to the LLM.
    Enhancing(u64),
    Done(u64, Result<Delivery>),
    /// A model download finished (id, error).
    Downloaded(String, Option<String>),
    Tick,
}

struct Job {
    generation: u64,
    samples: Vec<f32>,
    /// Enhancement mode, None = off.
    mode: Option<String>,
    context: bool,
    min_words: usize,
    timeout: Duration,
    language: Option<String>,
}

struct Delivery {
    raw: String,
    text: String,
    how: Delivered,
    /// The mode that rewrote the text, if any.
    mode: Option<String>,
}

impl Delivery {
    /// Nothing delivered (empty transcript, or the take was abandoned).
    fn none(raw: String) -> Self {
        Self { raw, text: String::new(), how: Delivered::Clipboard, mode: None }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    text: String,
    at: u64,
    delivered: String,
    /// The transcript before enhancement (only when it differs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
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
    /// Re-exec after the current reply (power off).
    restart: bool,
    /// Enhancement worker status (enhance::STATUS_*), and what the bar last saw.
    enhancer: Arc<AtomicU8>,
    enhancer_written: u8,
    /// The worker the pipeline was told to run: (model, gpu).
    enhancer_running: Option<(PathBuf, bool)>,
    /// Running download: (model id, percent).
    download: Arc<Mutex<Option<(String, u8)>>>,
    download_written: Option<(String, u8)>,
    /// Modes and installed models for the bar, cached for CATALOG_TTL.
    catalog: (Instant, serde_json::Value),
    /// Shared with the pipeline; here only for `field` (diagnostics).
    ime: Option<Arc<Ime>>,
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
    let enhancer = Arc::new(AtomicU8::new(enhance::STATUS_OFF));
    let jobs = spawn_pipeline(tx.clone(), generation.clone(), ime.clone(), ready.clone(), enhancer.clone());
    {
        let tx = tx.clone();
        hypr::watch(move |e| {
            let _ = tx.send(match e {
                hypr::Event::Chord => Msg::Chord,
                hypr::Event::Reloaded => Msg::Reloaded,
            });
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
        let _ = jobs.send(Work::Load { engine: config.stt.clone(), gpu: config.gpu });
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
        restart: false,
        enhancer,
        enhancer_written: enhance::STATUS_OFF,
        enhancer_running: None,
        download: Arc::new(Mutex::new(None)),
        download_written: None,
        catalog: (Instant::now() - CATALOG_TTL, serde_json::Value::Null),
        ime,
    };
    d.sync_enhancer();
    d.register_hotkey();
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
    /// Load the speech engine (power on).
    Load { engine: String, gpu: bool },
    /// Drop the model and hand its memory back to the OS (power off).
    Unload,
    /// Start (or restart with another model) the LLM worker, primed for `mode`.
    StartEnhancer { model: PathBuf, gpu: bool, mode: Option<String> },
    /// Precompute a mode's prompt prefix in the worker (mode switch).
    Prime(String),
    StopEnhancer,
}

/// The pipeline thread owns the speech model and the LLM worker and handles one take at a
/// time: transcribe → vocabulary → [enhance] → join → deliver.
fn spawn_pipeline(
    tx: Sender<Msg>,
    generation: Arc<AtomicU64>,
    ime: Option<Arc<Ime>>,
    ready: Arc<AtomicBool>,
    enhancer_status: Arc<AtomicU8>,
) -> Sender<Work> {
    let (wtx, wrx) = mpsc::channel::<Work>();
    std::thread::Builder::new()
        .name("pipeline".into())
        .spawn(move || {
            let mut stt: Option<Stt> = None;
            let mut enhancer: Option<Enhancer> = None;
            let mut recent = Recent::default();
            for work in wrx {
                match work {
                    Work::Load { engine, gpu } if stt.is_none() => {
                        let t = Instant::now();
                        match Stt::load(&engine, gpu) {
                            Ok(m) => {
                                eprintln!("{engine} loaded in {:.2?}", t.elapsed());
                                stt = Some(m);
                                ready.store(true, Ordering::SeqCst);
                            }
                            Err(e) => eprintln!("{e:#}"),
                        }
                        let _ = tx.send(Msg::Tick);
                    }
                    Work::Load { .. } => {}
                    Work::Unload => {
                        if stt.take().is_some() {
                            ready.store(false, Ordering::SeqCst);
                            release_memory();
                            eprintln!("model unloaded");
                        }
                        let _ = tx.send(Msg::Tick);
                    }
                    Work::StartEnhancer { model, gpu, mode } => {
                        enhancer = None;
                        match Enhancer::spawn(&model, gpu, enhancer_status.clone()) {
                            Ok(mut e) => {
                                // Queued behind the model load in the worker.
                                prime(&mut e, mode.as_deref());
                                enhancer = Some(e);
                            }
                            Err(e) => {
                                eprintln!("{e:#}");
                                enhancer_status.store(enhance::STATUS_ERROR, Ordering::SeqCst);
                            }
                        }
                        let _ = tx.send(Msg::Tick);
                    }
                    Work::StopEnhancer => {
                        enhancer = None;
                        let _ = tx.send(Msg::Tick);
                    }
                    Work::Prime(mode) => {
                        if let Some(e) = enhancer.as_mut() {
                            prime(e, Some(&mode));
                        }
                    }
                    Work::Transcribe(job) => {
                        let result = match stt.as_mut() {
                            None => Err(anyhow::anyhow!("model not loaded")),
                            Some(stt) => process(stt, enhancer.as_mut(), ime.as_deref(), &mut recent, &job, &generation, &tx),
                        };
                        let _ = tx.send(Msg::Done(job.generation, result));
                    }
                }
            }
        })
        .expect("spawn pipeline thread");
    wtx
}

fn prime(enhancer: &mut Enhancer, mode: Option<&str>) {
    if let Some(mode) = mode.and_then(enhance::find_mode) {
        let req = enhance::prime_request(enhancer.next_id(), &mode, &Vocabulary::load());
        enhancer.prime(&req);
    }
}

fn process(
    stt: &mut Stt,
    enhancer: Option<&mut Enhancer>,
    ime: Option<&Ime>,
    recent: &mut Recent,
    job: &Job,
    generation: &AtomicU64,
    tx: &Sender<Msg>,
) -> Result<Delivery> {
    let stale = || generation.load(Ordering::SeqCst) != job.generation;
    let vocab = Vocabulary::load();
    let raw = stt.transcribe(&job.samples, &Hints { prompt: vocab.prompt(), language: job.language.clone() })?;
    if raw.is_empty() || stale() {
        return Ok(Delivery::none(raw));
    }
    let mut text = vocab.canonicalize(&vocab.apply(&raw));

    // Context: the focused field (where the app reports it) and the window.
    let field = ime
        .filter(|_| job.context)
        .filter(|i| i.wait_typable(REFOCUS_GRACE))
        .and_then(|i| i.surrounding(SURROUNDING_WAIT))
        .map(|s| Field::from(&s));
    let window = if job.context { deliver::active_window() } else { None };

    let mut mode_used = None;
    let mode = job.mode.as_deref().and_then(enhance::find_mode);
    if let (Some(mode), Some(enh)) = (mode, enhancer) {
        if enh.is_ready() && text.split_whitespace().count() >= job.min_words {
            let _ = tx.send(Msg::Enhancing(job.generation));
            let mut ctx = enhance::Context::default();
            if let Some(f) = &field {
                (ctx.before, ctx.after) = f.trimmed();
            }
            if let Some(w) = &window {
                ctx.app = Some(context::app_line(w));
                // Apps that report no field text: what was dictated here before.
                if field.is_none() {
                    ctx.earlier = recent.get(&w.address);
                }
            }
            let req = enhance::request(enh.next_id(), &mode, &vocab, &ctx, &text);
            let t = Instant::now();
            match enh.run(&req, job.timeout, stale) {
                Ok(reply) => match enhance::check(&text, &reply.text, &ctx) {
                    Ok(out) => {
                        eprintln!(
                            "{} in {} ms ({} prompt tokens, {} cached, {} generated)",
                            mode.id,
                            t.elapsed().as_millis(),
                            reply.prompt_tokens,
                            reply.cached_tokens,
                            reply.gen_tokens
                        );
                        text = vocab.canonicalize(&out);
                        mode_used = Some(mode.id.clone());
                    }
                    Err(why) => eprintln!("enhancement rejected ({why}), delivering the transcript: {:?}", reply.text),
                },
                Err(Failure::Cancelled) => return Ok(Delivery::none(raw)),
                Err(Failure::Timeout) => eprintln!("enhancement timed out, delivering the transcript"),
                Err(Failure::Other(e)) => eprintln!("enhancement failed, delivering the transcript: {e:#}"),
            }
        }
    }
    if stale() {
        return Ok(Delivery::none(raw));
    }
    let typed = match &field {
        Some(f) => context::join(&text, f),
        None => text.clone(),
    };
    let how = deliver::deliver(ime, &typed)?;
    if let Some(w) = &window {
        recent.add(&w.address, &text);
    }
    Ok(Delivery { raw, text, how, mode: mode_used })
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
                    if self.restart {
                        self.reexec();
                    }
                }
                Msg::Reloaded => self.register_hotkey(),
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
                Msg::Enhancing(g) => {
                    if self.phase == Phase::Processing && self.current() == g {
                        self.set_phase(Phase::Enhancing);
                    }
                }
                Msg::Done(g, result) => {
                    if matches!(self.phase, Phase::Processing | Phase::Enhancing) && self.current() == g {
                        let outcome = match result {
                            Ok(d) if d.text.is_empty() => {
                                self.sound.play(Cue::Cancel);
                                Outcome::Empty
                            }
                            Ok(d) => {
                                self.remember(&d);
                                match d.how {
                                    Delivered::Typed | Delivered::Pasted => Outcome::Typed,
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
                Msg::Downloaded(id, error) => {
                    match error {
                        Some(e) => {
                            eprintln!("download of {id} failed: {e}");
                            self.sound.play(Cue::Error);
                        }
                        None => eprintln!("{id} downloaded"),
                    }
                    self.catalog.0 = Instant::now() - CATALOG_TTL;
                    self.sync_enhancer();
                    self.write_state();
                }
                Msg::Tick => {
                    let ready = self.ready.load(Ordering::SeqCst);
                    let enhancer = self.enhancer.load(Ordering::SeqCst);
                    let download = self.download.lock().unwrap().clone();
                    if self.phase == Phase::Recording
                        || ready != self.ready_written
                        || enhancer != self.enhancer_written
                        || download != self.download_written
                    {
                        self.ready_written = ready;
                        self.enhancer_written = enhancer;
                        self.download_written = download;
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
                Phase::Processing | Phase::Enhancing => {}
            },
            "cancel" => match self.phase {
                Phase::Armed => self.discard(false),
                Phase::Recording => self.discard(true),
                Phase::Processing | Phase::Enhancing => {
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
                    if on {
                        let _ = self.jobs.send(Work::Load { engine: self.config.stt.clone(), gpu: self.config.gpu });
                        self.sync_enhancer();
                    } else {
                        // Dropping the model leaves ONNX Runtime allocations behind (~250 MB
                        // after real use). A fresh process in "off" mode is the only way to
                        // really give that back, so restart right after replying.
                        self.restart = true;
                    }
                    self.write_state();
                }
            }
            "hotkey" => {
                let Some(id) = arg.filter(|id| hotkey::find(id).is_some()) else {
                    let ids: Vec<_> = hotkey::PRESETS.iter().map(|p| p.id).collect();
                    return json!({ "ok": false, "error": format!("usage: hotkey <{}>", ids.join("|")) }).to_string();
                };
                if id != self.config.hotkey {
                    self.config.hotkey = id.to_string();
                    self.config.save();
                    // Drops the old binds; `configreloaded` registers the new ones.
                    if !hotkey::reload_config() {
                        self.register_hotkey();
                    }
                    self.write_state();
                }
            }
            "stt" => {
                let Some(engine) = arg.filter(|e| matches!(*e, "parakeet" | "whisper")) else {
                    return json!({ "ok": false, "error": "usage: stt parakeet|whisper" }).to_string();
                };
                if engine != self.config.stt {
                    let spec = if engine == "whisper" { &model::WHISPER } else { &model::PARAKEET };
                    if !spec.is_installed() {
                        return json!({ "ok": false, "error": format!("{engine} is not downloaded: lightwispr download-model {engine}") }).to_string();
                    }
                    self.switch(|c| c.stt = engine.to_string());
                }
            }
            "gpu" => {
                let on = match arg {
                    Some("on") => true,
                    Some("off") => false,
                    _ => !self.config.gpu,
                };
                if on != self.config.gpu {
                    self.switch(|c| c.gpu = on);
                }
            }
            "mode" => {
                let Some(id) = arg.filter(|id| *id == "off" || enhance::find_mode(id).is_some()) else {
                    let ids: Vec<_> = enhance::modes().into_iter().map(|m| m.id).collect();
                    return json!({ "ok": false, "error": format!("usage: mode off|{}", ids.join("|")) }).to_string();
                };
                if id != self.config.enhance.mode {
                    self.config.enhance.mode = id.to_string();
                    self.config.save();
                    let was_running = self.enhancer_running.is_some();
                    self.sync_enhancer();
                    if was_running && id != "off" {
                        let _ = self.jobs.send(Work::Prime(id.to_string()));
                    }
                    self.write_state();
                }
            }
            "modes" => {
                // The bar asks when its panel opens: pick up new or edited mode files.
                self.catalog.0 = Instant::now() - CATALOG_TTL;
                self.write_state();
            }
            "context" => {
                self.config.enhance.context = match arg {
                    Some("on") => true,
                    Some("off") => false,
                    _ => !self.config.enhance.context,
                };
                self.config.save();
                self.write_state();
            }
            "download" => {
                let spec = match arg {
                    Some("parakeet") => Some(&model::PARAKEET),
                    Some("whisper") => Some(&model::WHISPER),
                    Some("enhance") | None => Some(self.config.llm()),
                    Some(id) => model::find(id),
                };
                let Some(spec) = spec else {
                    return json!({ "ok": false, "error": "usage: download parakeet|whisper|enhance|<model id>" }).to_string();
                };
                if self.download.lock().unwrap().is_some() {
                    return json!({ "ok": false, "error": "a download is already running" }).to_string();
                }
                *self.download.lock().unwrap() = Some((spec.id.to_string(), 0));
                let (progress, tx) = (self.download.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    let result = spec.download(|done, total| {
                        let pct = (done * 100 / total.max(1)) as u8;
                        let mut p = progress.lock().unwrap();
                        if p.as_ref().is_some_and(|(_, old)| *old != pct) {
                            *p = Some((spec.id.to_string(), pct));
                        }
                    });
                    *progress.lock().unwrap() = None;
                    let _ = tx.send(Msg::Downloaded(spec.id.to_string(), result.err().map(|e| format!("{e:#}"))));
                });
                self.write_state();
            }
            "field" => {
                // What the context step would see right now (for checking app support).
                let s = self.ime.as_ref().and_then(|i| i.surrounding(SURROUNDING_WAIT));
                let win = deliver::active_window();
                return json!({
                    "ok": true,
                    "input_method": self.ime.is_some(),
                    "focused": self.ime.as_ref().is_some_and(|i| i.is_active()),
                    "field": s.map(|s| { let f = Field::from(&s); json!({ "before": f.before, "after": f.after }) }),
                    "app": win.as_ref().map(context::app_line),
                })
                .to_string();
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
        let e = &self.config.enhance;
        let _ = self.jobs.send(Work::Transcribe(Job {
            generation: self.current(),
            samples,
            mode: Some(e.mode.clone()).filter(|m| m != "off"),
            context: e.context,
            min_words: e.min_words,
            timeout: Duration::from_millis(e.timeout_ms),
            language: self.config.whisper_language(),
        }));
    }

    /// Changes a setting that needs the models reloaded: save, then restart the process
    /// (the new one loads only what the config says, and no old allocations linger).
    fn switch(&mut self, change: impl FnOnce(&mut Config)) {
        match self.phase {
            Phase::Armed => self.discard(false),
            Phase::Recording => self.discard(true),
            _ => {}
        }
        change(&mut self.config);
        self.config.save();
        if self.config.enabled {
            self.restart = true;
        }
        self.write_state();
    }

    /// Runs the LLM worker exactly when the daemon is on, a mode is selected and its model
    /// is downloaded.
    fn sync_enhancer(&mut self) {
        let spec = self.config.llm();
        let want = self.config.enabled && self.config.enhance.mode != "off" && spec.is_installed();
        let target = want.then(|| (spec.path(), self.config.gpu));
        if target == self.enhancer_running {
            return;
        }
        let mode = Some(self.config.enhance.mode.clone()).filter(|m| m != "off");
        let _ = self.jobs.send(match &target {
            Some((model, gpu)) => Work::StartEnhancer { model: model.clone(), gpu: *gpu, mode },
            None => Work::StopEnhancer,
        });
        self.enhancer_running = target;
    }

    /// Replaces this process with a fresh daemon (same PID, so systemd doesn't notice).
    fn reexec(&mut self) {
        use std::os::unix::process::CommandExt;
        // Give the IPC thread a moment to deliver the reply.
        std::thread::sleep(Duration::from_millis(150));
        self.leave_submap();
        // argv[0] survives a rebuilt binary; /proc/self/exe would point at "(deleted)".
        let exe = std::env::args_os()
            .next()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("lightwispr"));
        let err = std::process::Command::new(&exe).arg("daemon").exec();
        eprintln!("restart failed ({err}); unloading in place instead");
        self.restart = false;
        let _ = self.jobs.send(Work::Unload);
    }

    fn register_hotkey(&self) {
        let preset = hotkey::find(&self.config.hotkey).or_else(|| hotkey::find(hotkey::DEFAULT));
        if let Some(p) = preset {
            if !hotkey::register(p) {
                eprintln!("could not register the hotkey in Hyprland");
            }
        }
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
            "hotkey": self.config.hotkey,
            "sound": self.config.sound,
            "volume": (self.config.volume as f64 * 100.0).round() / 100.0,
            "stt": self.config.stt,
            "gpu": self.config.gpu,
            "mode": self.config.enhance.mode,
            "context": self.config.enhance.context,
            "enhancer": self.enhancer_state(),
            "llm": self.config.llm().label,
            "download": self.download.lock().unwrap().as_ref().map(|(id, pct)| json!({ "model": id, "pct": pct })),
            "catalog": self.catalog.1,
        })
    }

    /// off | loading | ready | error, or "missing" when a mode is on but its model isn't
    /// downloaded yet.
    fn enhancer_state(&self) -> &'static str {
        if self.config.enhance.mode != "off" && self.enhancer_running.is_none() && self.config.enabled {
            return "missing";
        }
        enhance::status_name(self.enhancer.load(Ordering::SeqCst))
    }

    /// Modes and installed models, re-read at most every CATALOG_TTL (the state file is
    /// written many times a second while recording).
    fn refresh_catalog(&mut self) {
        if self.catalog.0.elapsed() < CATALOG_TTL {
            return;
        }
        let modes: Vec<_> =
            enhance::modes().into_iter().map(|m| json!({ "id": m.id, "name": m.name, "glyph": m.glyph })).collect();
        let llm = self.config.llm();
        self.catalog = (
            Instant::now(),
            json!({
                "modes": modes,
                "installed": {
                    "parakeet": model::PARAKEET.is_installed(),
                    "whisper": model::WHISPER.is_installed(),
                    "enhance": llm.is_installed(),
                },
                "sizes": { "parakeet": model::PARAKEET.size_mb(), "whisper": model::WHISPER.size_mb(), "enhance": llm.size_mb() },
            }),
        );
    }

    /// The bar reads this file (no socket round-trip needed for polling).
    fn write_state(&mut self) {
        self.refresh_catalog();
        let path = ipc::runtime_dir().join("state.json");
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, self.status().to_string()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    fn remember(&self, d: &Delivery) {
        let mut h = load_history();
        let text = d.text.trim().to_string();
        h.insert(0, Entry {
            raw: Some(d.raw.clone()).filter(|r| *r != text),
            mode: d.mode.clone(),
            text,
            at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            delivered: serde_json::to_value(d.how).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
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
