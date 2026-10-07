//! UI sounds, synthesized at startup (no sample files, nothing to license).
//!
//! Clean ceramic taps: a pure tone with a soft octave and one inharmonic "ceramic"
//! partial, a rounded sub-body, a smooth raised-cosine attack and a slight pitch settle.
//! No noise, no saturation. Cues are short two-note gestures (up = listening,
//! down = done) so they read instantly without being musical jingles.
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

/// One tap: when (s), base frequency (Hz), loudness.
#[derive(Clone, Copy)]
struct Tap {
    at: f32,
    freq: f32,
    gain: f32,
}

fn taps(cue: Cue) -> Vec<Tap> {
    match cue {
        // Rising fourth B5 → E6: "listening".
        Cue::Start => vec![Tap { at: 0.0, freq: 987.8, gain: 0.75 }, Tap { at: 0.06, freq: 1318.5, gain: 0.9 }],
        // Falling fourth E6 → B5: "got it".
        Cue::Stop => vec![Tap { at: 0.0, freq: 1318.5, gain: 0.75 }, Tap { at: 0.06, freq: 987.8, gain: 0.9 }],
        // Lower, quieter fall: "never mind".
        Cue::Cancel => vec![Tap { at: 0.0, freq: 784.0, gain: 0.55 }, Tap { at: 0.07, freq: 587.3, gain: 0.5 }],
        // Low double knock.
        Cue::Error => vec![Tap { at: 0.0, freq: 392.0, gain: 0.8 }, Tap { at: 0.13, freq: 392.0, gain: 0.7 }],
    }
}

/// (frequency multiple, amplitude, decay time constant in s)
const PARTIALS: [(f32, f32, f32); 4] = [
    (1.0, 1.0, 0.050),  // the tone
    (2.0, 0.16, 0.022), // octave: clarity
    (2.76, 0.07, 0.012), // inharmonic: the ceramic "tick"
    (0.5, 0.22, 0.030), // sub: rounded body, no boom
];
const ATTACK: f32 = 0.002;
const TAIL: f32 = 0.22;

fn render(cue: Cue, rate: u32, volume: f32) -> Vec<f32> {
    let taps = taps(cue);
    let len_s = taps.iter().map(|t| t.at).fold(0.0, f32::max) + TAIL;
    let n = (len_s * rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    for tap in taps {
        let off = (tap.at * rate as f32) as usize;
        let mut phase = [0.0f32; PARTIALS.len()];
        for i in 0..n - off {
            let t = i as f32 / rate as f32;
            // Raised-cosine attack: no click, still crisp.
            let attack = if t < ATTACK { 0.5 - 0.5 * (PI * t / ATTACK).cos() } else { 1.0 };
            // Settles 1.5% downwards in the first 30 ms, like a struck object.
            let f = tap.freq * (1.0 + 0.015 * (-t / 0.03).exp());
            let mut v = 0.0;
            for (k, (mult, amp, decay)) in PARTIALS.iter().enumerate() {
                phase[k] += TAU * f * mult / rate as f32;
                v += phase[k].sin() * amp * (-t / decay).exp();
            }
            out[off + i] += v * attack * tap.gain;
        }
    }
    // Normalize, then set each cue's loudness (cancel stays in the background).
    let level = match cue {
        Cue::Start | Cue::Stop => 0.8,
        Cue::Cancel => 0.55,
        Cue::Error => 0.7,
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
