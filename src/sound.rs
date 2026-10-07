//! UI sounds, synthesized at startup (no sample files, nothing to license).
//!
//! Modelled on a creamy, clonky ceramic keyboard: each stroke is a handful of damped
//! resonances (modal synthesis) excited by an instant hit — a hollow, woody body around
//! 480 Hz that dies within ~20 ms, two short ceramic modes above it, and a tiny tick of
//! the cap bottoming out. Very short decays make it a knock, not a tone. No noise, no
//! saturation.
//!
//! The output stream is opened on demand and closed after a short idle time.

use std::f32::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Start,
    Stop,
    Cancel,
    Error,
}

impl Cue {
    pub const ALL: [Cue; 4] = [Cue::Start, Cue::Stop, Cue::Cancel, Cue::Error];
}

/// One keystroke: when (s), pitch scale, loudness, and whether it's the softer,
/// brighter up-stroke of a released key.
#[derive(Clone, Copy)]
struct Stroke {
    at: f32,
    pitch: f32,
    gain: f32,
    up: bool,
}

const fn down(at: f32, pitch: f32, gain: f32) -> Stroke {
    Stroke { at, pitch, gain, up: false }
}

const fn up(at: f32, pitch: f32, gain: f32) -> Stroke {
    Stroke { at, pitch, gain, up: true }
}

fn strokes(cue: Cue) -> Vec<Stroke> {
    match cue {
        // A single crisp key press: "listening".
        Cue::Start => vec![down(0.0, 1.12, 1.0)],
        // Press and release of a slightly deeper key: "got it, sending".
        Cue::Stop => vec![down(0.0, 0.94, 1.0), up(0.058, 1.0, 0.32)],
        // Two soft, low taps: "never mind".
        Cue::Cancel => vec![down(0.0, 0.88, 0.6), down(0.075, 0.76, 0.5)],
        // Low double knock.
        Cue::Error => vec![down(0.0, 0.62, 0.9), down(0.12, 0.62, 0.8)],
    }
}

/// Resonant modes of the down-stroke: (Hz, amplitude, decay time constant in s).
const MODES: [(f32, f32, f32); 4] = [
    (480.0, 1.00, 0.018),  // hollow, woody body: the "clonk"
    (1130.0, 0.42, 0.009), // case/plate
    (2650.0, 0.20, 0.0045), // ceramic cap
    (4300.0, 0.09, 0.0022), // bottom-out tick
];
const ATTACK: f32 = 0.0004;
const TAIL: f32 = 0.12;

fn render(cue: Cue, rate: u32, volume: f32) -> Vec<f32> {
    let strokes = strokes(cue);
    let len_s = strokes.iter().map(|s| s.at).fold(0.0, f32::max) + TAIL;
    let n = (len_s * rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    for st in strokes {
        let off = (st.at * rate as f32) as usize;
        for i in 0..n - off {
            let t = i as f32 / rate as f32;
            let attack = if t < ATTACK { 0.5 - 0.5 * (PI * t / ATTACK).cos() } else { 1.0 };
            let mut v = 0.0;
            for (k, (freq, amp, decay)) in MODES.iter().enumerate() {
                // The up-stroke has no body thump: mostly cap and tick, a bit shorter.
                let (amp, decay) = if st.up { (if k == 0 { amp * 0.25 } else { amp * 1.3 }, decay * 0.7) } else { (*amp, *decay) };
                v += (TAU * freq * st.pitch * t).sin() * amp * (-t / decay).exp();
            }
            out[off + i] += v * attack * st.gain;
        }
    }
    let level = match cue {
        Cue::Start | Cue::Stop => 0.85,
        Cue::Cancel => 0.6,
        Cue::Error => 0.75,
    };
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    for s in &mut out {
        *s = *s / peak * level * volume;
    }
    out
}

/// Plays cues on a background thread. Cheap to clone.
#[derive(Clone)]
pub struct Player {
    tx: Sender<Cue>,
    volume: Arc<AtomicU32>,
    enabled: Arc<AtomicBool>,
}

impl Player {
    pub fn new(volume: f32, enabled: bool) -> Self {
        let (tx, rx) = mpsc::channel::<Cue>();
        let volume = Arc::new(AtomicU32::new(volume.to_bits()));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let vol = volume.clone();
        std::thread::Builder::new()
            .name("sound".into())
            .spawn(move || {
                let mut out: Option<Output> = None;
                loop {
                    match rx.recv_timeout(Duration::from_secs(3)) {
                        Ok(cue) => {
                            if out.is_none() {
                                out = Output::open().map_err(|e| eprintln!("sound output: {e:#}")).ok();
                            }
                            if let Some(o) = &out {
                                let pcm = render(cue, o.rate, f32::from_bits(vol.load(Ordering::Relaxed)));
                                let mut q = o.queue.lock().unwrap();
                                // Mix into whatever is still playing.
                                for (i, s) in pcm.into_iter().enumerate() {
                                    match q.get_mut(i) {
                                        Some(x) => *x += s,
                                        None => q.push(s),
                                    }
                                }
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => out = None, // close the device when idle
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn sound thread");
        Self { tx, volume, enabled }
    }

    pub fn play(&self, cue: Cue) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.tx.send(cue);
        }
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }
}

struct Output {
    _stream: cpal::Stream,
    rate: u32,
    queue: Arc<Mutex<Vec<f32>>>,
}

impl Output {
    fn open() -> anyhow::Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no output device"))?;
        let config = device.default_output_config()?;
        let rate = config.sample_rate();
        let channels = config.channels() as usize;
        let queue: Arc<Mutex<Vec<f32>>> = Arc::default();
        let q = queue.clone();
        let stream = device.build_output_stream(
            config.into(),
            move |data: &mut [f32], _: &_| {
                let mut q = q.lock().unwrap();
                let frames = data.len() / channels;
                let take = frames.min(q.len());
                for (f, frame) in data.chunks_mut(channels).enumerate() {
                    let s = if f < take { q[f] } else { 0.0 };
                    frame.fill(s);
                }
                q.drain(..take);
            },
            |e| eprintln!("output stream error: {e}"),
            None,
        )?;
        stream.play()?;
        Ok(Self { _stream: stream, rate, queue })
    }
}
