//! Microphone capture. The input stream only exists while recording, so the mic is
//! closed (and the privacy indicator off) the rest of the time.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat};

pub const TARGET_RATE: u32 = 16_000;

/// Current input level 0..1 (smoothed RMS), shared with the bar state writer.
#[derive(Clone, Default)]
pub struct Level(Arc<AtomicU32>);

impl Level {
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

pub struct Recording {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    level: Level,
}

impl Recording {
    pub fn start(level: Level) -> Result<Self> {
        let device = cpal::default_host()
            .default_input_device()
            .context("no input device")?;
        let config = device.default_input_config()?;
        let rate = config.sample_rate();
        let channels = config.channels() as usize;
        let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
        let err_fn = |e| eprintln!("input stream error: {e}");
        let cfg: cpal::StreamConfig = config.clone().into();
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, cfg, channels, buf.clone(), level.clone(), err_fn)?,
            SampleFormat::I16 => build::<i16>(&device, cfg, channels, buf.clone(), level.clone(), err_fn)?,
            SampleFormat::I32 => build::<i32>(&device, cfg, channels, buf.clone(), level.clone(), err_fn)?,
            f => bail!("unsupported sample format {f}"),
        };
        stream.play()?;
        Ok(Self { _stream: stream, buf, rate, level })
    }

    /// Stops capture and returns 16 kHz mono samples.
    pub fn finish(self) -> Vec<f32> {
        let mono = std::mem::take(&mut *self.buf.lock().unwrap());
        self.level.set(0.0);
        resample(&mono, self.rate, TARGET_RATE)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    buf: Arc<Mutex<Vec<f32>>>,
    level: Level,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &_| {
            let mut out = buf.lock().unwrap();
            let start = out.len();
            out.extend(
                data.chunks(channels)
                    .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32),
            );
            let chunk = &out[start..];
            if !chunk.is_empty() {
                let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
                // Speech RMS is ~0.01..0.2; map to a pleasant 0..1 with a soft curve.
                let target = (rms * 8.0).sqrt().min(1.0);
                let prev = level.get();
                let k = if target > prev { 0.6 } else { 0.15 }; // fast attack, slow release
                level.set(prev + (target - prev) * k);
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

/// Box-filtered linear resampling. Good enough for speech.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let half = (ratio / 2.0).floor() as isize;
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let center = (i as f64 * ratio) as isize;
            let (lo, hi) = ((center - half).max(0) as usize, ((center + half) as usize).min(input.len() - 1));
            input[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32
        })
        .collect()
}

/// Peak RMS over 50 ms windows, to tell silence from speech.
pub fn peak_rms(samples: &[f32]) -> f32 {
    samples
        .chunks(TARGET_RATE as usize / 20)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt())
        .fold(0.0, f32::max)
}
