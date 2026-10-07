//! lightwispr prototype: record from the default mic (or read a WAV), transcribe with Parakeet, print.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat};
use transcribe_rs::onnx::Quantization;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};

const TARGET_RATE: u32 = 16_000;
const MODEL_NAME: &str = "parakeet-tdt-0.6b-v3-int8";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("record") => cmd_record(),
        Some("transcribe") => {
            let path = args.get(1).context("usage: lightwispr transcribe <file.wav>")?;
            cmd_transcribe(Path::new(path))
        }
        _ => {
            eprintln!("usage: lightwispr record | transcribe <file.wav>");
            std::process::exit(2);
        }
    }
}

fn model_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("lightwispr/models").join(MODEL_NAME)
}

fn load_model() -> Result<ParakeetModel> {
    let dir = model_dir();
    let t = Instant::now();
    let model = ParakeetModel::load(&dir, &Quantization::Int8)
        .map_err(|e| anyhow!("loading model from {}: {e}", dir.display()))?;
    eprintln!("model loaded in {:.2?} (RSS {})", t.elapsed(), rss());
    Ok(model)
}

fn transcribe(model: &mut ParakeetModel, samples: &[f32]) -> Result<String> {
    let secs = samples.len() as f64 / TARGET_RATE as f64;
    let t = Instant::now();
    let result = model
        .transcribe_with(samples, &ParakeetParams::default())
        .map_err(|e| anyhow!("transcription failed: {e}"))?;
    let took = t.elapsed();
    eprintln!(
        "transcribed {secs:.1}s audio in {took:.2?} ({:.1}x real-time, peak RSS {})",
        secs / took.as_secs_f64(),
        peak_rss()
    );
    Ok(result.text.trim().to_string())
}

fn cmd_transcribe(path: &Path) -> Result<()> {
    let samples = transcribe_rs::audio::read_wav_samples(path)
        .map_err(|e| anyhow!("reading {} (needs 16 kHz mono): {e}", path.display()))?;
    let mut model = load_model()?;
    println!("{}", transcribe(&mut model, &samples)?);
    Ok(())
}

fn cmd_record() -> Result<()> {
    let mut model = load_model()?;
    loop {
        eprint!("\n[Enter] start recording, [q Enter] quit: ");
        if read_line()?.trim() == "q" {
            return Ok(());
        }
        let samples = record_until_enter()?;
        if samples.len() < TARGET_RATE as usize / 4 {
            eprintln!("too short, skipped");
            continue;
        }
        println!("{}", transcribe(&mut model, &samples)?);
    }
}

fn read_line() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s)
}

/// Records mono audio from the default input until Enter, resampled to 16 kHz.
fn record_until_enter() -> Result<Vec<f32>> {
    let device = cpal::default_host()
        .default_input_device()
        .context("no input device")?;
    let config = device.default_input_config()?;
    let rate = config.sample_rate();
    let channels = config.channels() as usize;
    let buf: Arc<Mutex<Vec<f32>>> = Arc::default();

    let err_fn = |e| eprintln!("stream error: {e}");
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config.into(), channels, buf.clone(), err_fn)?,
        SampleFormat::I16 => build::<i16>(&device, &config.into(), channels, buf.clone(), err_fn)?,
        SampleFormat::I32 => build::<i32>(&device, &config.into(), channels, buf.clone(), err_fn)?,
        f => bail!("unsupported sample format {f}"),
    };
    stream.play()?;
    eprint!("recording ({rate} Hz, {channels} ch)... [Enter] stop ");
    read_line()?;
    drop(stream);

    let mono = std::mem::take(&mut *buf.lock().unwrap());
    Ok(resample(&mono, rate, TARGET_RATE))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    buf: Arc<Mutex<Vec<f32>>>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config.clone(),
        move |data: &[T], _: &_| {
            let mut out = buf.lock().unwrap();
            out.extend(
                data.chunks(channels)
                    .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32),
            );
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

/// Box-filtered linear resampling. Good enough for speech; replaced by a proper resampler later.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
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

fn proc_status(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with(key)).map(|l| l[key.len()..].trim().to_string()))
        .unwrap_or_default()
}

fn rss() -> String {
    proc_status("VmRSS:")
}

fn peak_rss() -> String {
    proc_status("VmHWM:")
}
