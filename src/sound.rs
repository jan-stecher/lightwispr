//! UI sounds, synthesized at startup (no sample files, nothing to license).
//!
//! The character aims at a clean, "creamy" ceramic keycap: a soft rounded thock in the
//! low mids, a few inharmonic ceramic partials that die out quickly, and a muffled
//! (low-passed) contact transient instead of a sharp click.
//!
//! The output stream is opened on demand and closed after a short idle time.

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

/// One keycap hit. `pitch` scales all frequencies, `gain` the loudness.
#[derive(Clone, Copy)]
struct Hit {
    at: f32,
    pitch: f32,
    gain: f32,
}

fn hits(cue: Cue) -> Vec<Hit> {
    match cue {
        // Slightly brighter: "I'm listening".
        Cue::Start => vec![Hit { at: 0.0, pitch: 1.12, gain: 0.9 }],
        // Rounder and lower: "done, sending".
        Cue::Stop => vec![Hit { at: 0.0, pitch: 0.86, gain: 1.0 }],
        // Two soft taps stepping down: "never mind".
        Cue::Cancel => vec![
            Hit { at: 0.0, pitch: 0.95, gain: 0.6 },
            Hit { at: 0.075, pitch: 0.78, gain: 0.5 },
        ],
        // Low double knock.
        Cue::Error => vec![
            Hit { at: 0.0, pitch: 0.62, gain: 0.8 },
            Hit { at: 0.11, pitch: 0.62, gain: 0.7 },
        ],
    }
}

fn render(cue: Cue, rate: u32, volume: f32) -> Vec<f32> {
    let hits = hits(cue);
    let len_s = hits.iter().map(|h| h.at).fold(0.0, f32::max) + 0.12;
    let n = (len_s * rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    let mut seed = 0x9E37_79B9u32;
    for h in hits {
        let off = (h.at * rate as f32) as usize;
        let mut lp = 0.0f32; // one-pole low-pass state for the transient
        for i in 0..n - off {
            let t = i as f32 / rate as f32;
            // ~0.6 ms attack so nothing clicks.
            let attack = (t / 0.0006).min(1.0);
            // Body: the rounded "thock" (low mids, quick decay).
            let body = (std::f32::consts::TAU * 230.0 * h.pitch * t).sin() * (-t / 0.016).exp() * 0.9;
            // Ceramic partials: inharmonic, short, glassy but soft.
            let f0 = 1450.0 * h.pitch;
            let ceramic = [(1.0, 0.42, 0.022), (1.47, 0.22, 0.015), (2.09, 0.10, 0.009)]
                .iter()
                .map(|(m, a, d)| (std::f32::consts::TAU * f0 * m * t).sin() * a * (-t / d).exp())
                .sum::<f32>();
            // Contact transient: noise, heavily low-passed ("muffled").
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
            lp += (noise - lp) * 0.18;
            let transient = lp * (-t / 0.0025).exp() * 0.9;
            out[off + i] += (body + ceramic + transient) * attack * h.gain;
        }
    }
    // Gentle saturation keeps it round, then master volume.
    for s in &mut out {
        *s = (*s * 0.8).tanh() * volume;
    }
    out
}

/// Plays cues on a background thread. Cheap to clone.
#[derive(Clone)]
pub struct Player {
    tx: Sender<Cue>,
}

impl Player {
    pub fn new(volume: f32) -> Self {
        let (tx, rx) = mpsc::channel::<Cue>();
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
                                let pcm = render(cue, o.rate, volume);
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
        Self { tx }
    }

    pub fn play(&self, cue: Cue) {
        let _ = self.tx.send(cue);
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
