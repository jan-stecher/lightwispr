//! `lightwispr enhance` (one request) and `lightwispr eval` (the eval set in
//! eval/dictations.toml) against a model, without the daemon.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::config::Config;
use crate::context;
use crate::enhance::{self, Enhancer, Failure, Vocabulary};
use crate::model;

const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Opts {
    /// Registry id or path to a GGUF; default: the configured model.
    pub model: Option<String>,
    pub gpu: Option<bool>,
    pub mode: Option<String>,
    pub before: String,
    pub after: String,
    pub app: Option<String>,
}

pub fn parse_opts(args: &[String]) -> Result<(Opts, Vec<String>)> {
    let mut o = Opts { model: None, gpu: None, mode: None, before: String::new(), after: String::new(), app: None };
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--model" => o.model = Some(val()?),
            "--mode" => o.mode = Some(val()?),
            "--before" => o.before = val()?,
            "--after" => o.after = val()?,
            "--app" => o.app = Some(val()?),
            "--gpu" => o.gpu = Some(true),
            "--cpu" => o.gpu = Some(false),
            _ if a.starts_with("--") => bail!("unknown option {a}"),
            _ => rest.push(a.clone()),
        }
    }
    Ok((o, rest))
}

fn spawn(o: &Opts) -> Result<(Enhancer, String)> {
    let cfg = Config::load();
    let gpu = o.gpu.unwrap_or(cfg.gpu);
    let (path, name) = match &o.model {
        Some(m) if Path::new(m).is_file() => (PathBuf::from(m), m.clone()),
        Some(id) => {
            let spec = model::find(id).with_context(|| format!("unknown model {id:?}"))?;
            (spec.path(), spec.id.to_string())
        }
        None => (cfg.llm().path(), cfg.llm().id.to_string()),
    };
    if !path.is_file() {
        bail!("{} is not downloaded: lightwispr download-model {name}", path.display());
    }
    let t = Instant::now();
    let enh = Enhancer::spawn(&path, gpu, Arc::new(AtomicU8::new(0)))?;
    enh.wait_ready(TIMEOUT)?;
    eprintln!("{name} ({}) loaded in {:.1?}", if gpu { "GPU" } else { "CPU" }, t.elapsed());
    Ok((enh, name))
}

fn run_one(enh: &mut Enhancer, mode: &str, vocab: &Vocabulary, ctx: &enhance::Context, text: &str) -> Result<(String, enhance::Reply)> {
    let mode = enhance::find_mode(mode).with_context(|| format!("unknown mode {mode:?}"))?;
    let req = enhance::request(enh.next_id(), &mode, vocab, ctx, text);
    match enh.run(&req, TIMEOUT, || false) {
        Ok(r) => Ok((r.text.clone(), r)),
        Err(Failure::Timeout) => Err(anyhow!("timed out")),
        Err(Failure::Cancelled) => Err(anyhow!("cancelled")),
        Err(Failure::Other(e)) => Err(e),
    }
}

pub fn enhance_cli(args: &[String]) -> Result<()> {
    let (o, rest) = parse_opts(args)?;
    let text = rest.join(" ");
    if text.is_empty() {
        bail!("usage: lightwispr enhance [--mode m] [--before t] [--after t] [--app a] [--model id|path] [--gpu|--cpu] <text>");
    }
    let mode = o.mode.clone().unwrap_or_else(|| Some(Config::load().enhance.mode).filter(|m| m != "off").unwrap_or("casual".into()));
    let (mut enh, _) = spawn(&o)?;
    let ctx = enhance::Context { before: o.before.clone(), after: o.after.clone(), app: o.app.clone(), earlier: Vec::new() };
    let vocab = Vocabulary::load();
    let input = vocab.apply(&text);
    let (out, r) = run_one(&mut enh, &mode, &vocab, &ctx, &input)?;
    eprintln!(
        "{mode}: prefill {} ms ({} tokens, {} cached), generate {} ms ({} tokens)",
        r.prefill_ms, r.prompt_tokens, r.cached_tokens, r.gen_ms, r.gen_tokens
    );
    match enhance::check(&input, &out, &ctx) {
        Ok(t) => println!("{}", vocab.canonicalize(&t)),
        Err(why) => {
            eprintln!("rejected ({why}), the daemon would deliver the transcript; model said:");
            println!("{out}");
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct EvalFile {
    #[serde(default)]
    vocabulary: Vec<String>,
    #[serde(rename = "case")]
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    mode: String,
    input: String,
    #[serde(default)]
    before: String,
    #[serde(default)]
    after: String,
    app: Option<String>,
    #[serde(default)]
    earlier: Vec<String>,
    #[serde(default)]
    must: Vec<String>,
    #[serde(default)]
    any: Vec<String>,
    #[serde(default)]
    must_not: Vec<String>,
    starts: Option<String>,
    not_ends: Option<String>,
}

impl Case {
    /// Why the output fails this case, if it does.
    fn verdict(&self, out: &str, ctx: &enhance::Context) -> Option<String> {
        let checked = match enhance::check(&self.input, out, ctx) {
            // As the daemon types it (joined to the field text), minus the join spaces.
            Ok(t) => context::join(&t, &context::Field { before: self.before.clone(), after: self.after.clone() }).trim().to_string(),
            Err(why) => return Some(format!("rejected: {why}")),
        };
        if let Some(m) = self.must.iter().find(|m| !checked.contains(m.as_str())) {
            return Some(format!("missing {m:?}"));
        }
        if !self.any.is_empty() && !self.any.iter().any(|a| checked.contains(a.as_str())) {
            return Some(format!("none of {:?}", self.any));
        }
        if let Some(m) = self.must_not.iter().find(|m| checked.contains(m.as_str())) {
            return Some(format!("contains {m:?}"));
        }
        if let Some(s) = self.starts.as_ref().filter(|s| !checked.starts_with(s.as_str())) {
            return Some(format!("doesn't start with {s:?}"));
        }
        if let Some(e) = self.not_ends.as_ref().filter(|e| checked.ends_with(e.as_str())) {
            return Some(format!("ends with {e:?}"));
        }
        None
    }
}

pub fn eval_cli(args: &[String]) -> Result<()> {
    let (o, rest) = parse_opts(args)?;
    let file = rest.first().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("eval/dictations.toml"));
    let src = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let set: EvalFile = toml::from_str(&src).with_context(|| format!("parsing {}", file.display()))?;
    let vocab = Vocabulary::from_terms(&set.vocabulary);
    let (mut enh, name) = spawn(&o)?;
    let (mut passed, mut total, mut times) = (0, 0, Vec::new());
    for (i, c) in set.cases.iter().enumerate() {
        if o.mode.as_ref().is_some_and(|m| *m != c.mode) {
            continue;
        }
        total += 1;
        let ctx = enhance::Context { before: c.before.clone(), after: c.after.clone(), app: c.app.clone(), earlier: c.earlier.clone() };
        let t = Instant::now();
        let (verdict, out) = match run_one(&mut enh, &c.mode, &vocab, &ctx, &c.input) {
            Ok((out, _)) => {
                let out = vocab.canonicalize(&out);
                (c.verdict(&out, &ctx), out)
            }
            Err(e) => (Some(format!("error: {e:#}")), String::new()),
        };
        let ms = t.elapsed().as_millis() as u64;
        times.push(ms);
        let mark = if verdict.is_none() {
            passed += 1;
            "PASS".to_string()
        } else {
            format!("FAIL ({})", verdict.unwrap())
        };
        println!("{:>2} {:<7} {:>5} ms  {mark}\n   in:  {}\n   out: {}", i + 1, c.mode, ms, c.input, out.replace('\n', "⏎"));
    }
    times.sort_unstable();
    let p = |q: f64| times.get(((times.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0);
    println!("\n{name}: {passed}/{total} passed · median {} ms · p90 {} ms · max {} ms", p(0.5), p(0.9), p(1.0));
    Ok(())
}

/// `lightwispr prompt [mode]`: the system prompt and examples exactly as the model gets them
/// (before the model's chat template is applied).
pub fn prompt_cli(mode: Option<&str>) -> Result<()> {
    let id = mode.map(String::from).unwrap_or_else(|| Some(Config::load().enhance.mode).filter(|m| m != "off").unwrap_or("casual".into()));
    let mode = enhance::find_mode(&id).with_context(|| format!("unknown mode {id:?}"))?;
    let req = enhance::prime_request(0, &mode, &Vocabulary::load());
    println!("═══ system ═══\n{}\n", req["system"].as_str().unwrap_or_default());
    for (i, ex) in req["examples"].as_array().into_iter().flatten().enumerate() {
        println!("═══ example {} · user ═══\n{}\n═══ example {} · assistant ═══\n{}\n", i + 1, ex[0].as_str().unwrap_or_default(), i + 1, ex[1].as_str().unwrap_or_default());
    }
    println!("═══ user (per dictation) ═══\n<app>…</app> <earlier>…</earlier> <before>…</before> <after>…</after>   (when available)\n<dictation>…</dictation>");
    Ok(())
}
