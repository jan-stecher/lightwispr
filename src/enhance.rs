//! Enhancement: modes (= prompt files), the vocabulary, the request sent to the LLM
//! worker (`lightwispr-llm`, its own process), and the checks its answer has to pass
//! before it replaces the transcript.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config;

/// Built-in modes in the order the bar shows them; a file with the same id in
/// ~/.config/lightwispr/modes/ overrides one, other files add modes.
const BUILTIN: &[(&str, &str)] = &[
    ("casual", include_str!("../prompts/casual.toml")),
    ("formal", include_str!("../prompts/formal.toml")),
    ("coding", include_str!("../prompts/coding.toml")),
];
const BASE: &str = include_str!("../prompts/base.toml");

pub struct Mode {
    pub id: String,
    pub name: String,
    pub glyph: String,
    prompt: String,
    examples: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct ModeFile {
    name: Option<String>,
    glyph: Option<String>,
    prompt: String,
    #[serde(default, rename = "example")]
    examples: Vec<Example>,
}

#[derive(Deserialize)]
struct Example {
    input: String,
    output: String,
}

#[derive(Deserialize)]
struct BaseFile {
    prompt: String,
    context: String,
    #[serde(default)]
    vocabulary: String,
}

pub fn modes_dir() -> PathBuf {
    config::dir().join("modes")
}

fn parse_mode(id: &str, src: &str) -> Result<Mode> {
    let f: ModeFile = toml::from_str(src).with_context(|| format!("mode {id}"))?;
    Ok(Mode {
        id: id.to_string(),
        name: f.name.unwrap_or_else(|| id.to_string()),
        glyph: f.glyph.unwrap_or_else(|| "sparkles".into()),
        prompt: f.prompt.trim().to_string(),
        examples: f.examples.into_iter().map(|e| (e.input, e.output)).collect(),
    })
}

/// All modes, re-read from disk on every call (a handful of small files), so prompt edits
/// apply to the next dictation.
pub fn modes() -> Vec<Mode> {
    let user = |id: &str| std::fs::read_to_string(modes_dir().join(format!("{id}.toml"))).ok();
    let load = |id: &str, src: &str| {
        parse_mode(id, src).map_err(|e| eprintln!("{e:#}")).ok()
    };
    let mut out: Vec<Mode> = BUILTIN
        .iter()
        .filter_map(|(id, src)| user(id).and_then(|s| load(id, &s)).or_else(|| load(id, src)))
        .collect();
    let mut extra: Vec<String> = std::fs::read_dir(modes_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
        .filter(|id| id != "base" && id != "off" && !BUILTIN.iter().any(|(b, _)| b == id))
        .collect();
    extra.sort();
    out.extend(extra.iter().filter_map(|id| load(id, &user(id)?)));
    out
}

pub fn find_mode(id: &str) -> Option<Mode> {
    modes().into_iter().find(|m| m.id == id)
}

/// Path of the user's file for a mode, created from the built-in one if missing (so it
/// can be edited). "base" is the shared rules.
pub fn mode_file(id: &str) -> Result<PathBuf> {
    let path = modes_dir().join(format!("{id}.toml"));
    if !path.exists() {
        let src = if id == "base" {
            BASE
        } else {
            BUILTIN.iter().find(|(b, _)| *b == id).map(|(_, s)| *s).with_context(|| format!("no mode {id:?}"))?
        };
        std::fs::create_dir_all(modes_dir())?;
        std::fs::write(&path, src)?;
    }
    Ok(path)
}

fn base() -> BaseFile {
    let user = std::fs::read_to_string(modes_dir().join("base.toml")).ok();
    user.and_then(|s| toml::from_str(&s).map_err(|e| eprintln!("modes/base.toml: {e}")).ok())
        .unwrap_or_else(|| toml::from_str(BASE).expect("built-in base.toml"))
}

// ── Vocabulary ──────────────────────────────────────────────────────────────

/// ~/.config/lightwispr/vocabulary.txt: one term per line (spelled exactly like this), or
/// `wrong -> right` for a recurring mishearing (replaced right after transcription).
#[derive(Default)]
pub struct Vocabulary {
    terms: Vec<String>,
    fixes: Vec<(String, String)>,
}

impl Vocabulary {
    pub fn load() -> Self {
        let src = std::fs::read_to_string(config::dir().join("vocabulary.txt")).unwrap_or_default();
        Self::parse(&src)
    }

    fn parse(src: &str) -> Self {
        let mut v = Self::default();
        for line in src.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match line.split_once("->") {
                Some((wrong, right)) if !wrong.trim().is_empty() && !right.trim().is_empty() => {
                    v.fixes.push((wrong.trim().to_string(), right.trim().to_string()));
                    v.add_term(right.trim());
                }
                _ => v.add_term(line),
            }
        }
        v
    }

    fn add_term(&mut self, t: &str) {
        if !self.terms.iter().any(|x| x == t) {
            self.terms.push(t.to_string());
        }
    }

    pub fn from_terms(terms: &[String]) -> Self {
        Self { terms: terms.to_vec(), fixes: Vec::new() }
    }

    /// Replaces the `wrong -> right` phrases (case-insensitive, whole words).
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (wrong, right) in &self.fixes {
            out = replace_words(&out, wrong, right);
        }
        out
    }

    /// Brings the vocabulary's terms into their exact spelling ("Lightwispr" → "lightwispr",
    /// "claude code" → "Claude Code"). A lowercase match of a capitalized single word is left
    /// alone: that is usually an ordinary word ("whisper", "warp").
    pub fn canonicalize(&self, text: &str) -> String {
        let mut out = text.to_string();
        for term in &self.terms {
            let mut chars = term.chars();
            let capitalized = chars.next().is_some_and(char::is_uppercase) && chars.all(|c| !c.is_uppercase());
            let single = !term.contains(char::is_whitespace);
            out = replace_words_with(&out, term, |found| {
                let keep = found == term || (capitalized && single && found.chars().all(|c| !c.is_uppercase()));
                if keep { found.to_string() } else { term.clone() }
            });
        }
        out
    }

    /// For Whisper's initial prompt.
    pub fn prompt(&self) -> Option<String> {
        (!self.terms.is_empty()).then(|| format!("{}.", self.terms.join(", ")))
    }
}

fn replace_words(text: &str, wrong: &str, right: &str) -> String {
    replace_words_with(text, wrong, |_| right.to_string())
}

/// Replaces whole-word, case-insensitive matches of `needle` with `with(matched text)`.
fn replace_words_with(text: &str, needle: &str, with: impl Fn(&str) -> String) -> String {
    let wrong = needle;
    let lower = text.to_lowercase();
    let needle = wrong.to_lowercase();
    // Lowercasing can change byte lengths (rare: ẞ, İ); skip rather than cut wrongly.
    if lower.len() != text.len() || needle.is_empty() {
        return text.to_string();
    }
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
    let (mut out, mut last) = (String::new(), 0);
    for (i, _) in lower.match_indices(&needle) {
        let end = i + needle.len();
        if i < last || is_word(text[..i].chars().last()) || is_word(text[end..].chars().next()) {
            continue;
        }
        out.push_str(&text[last..i]);
        out.push_str(&with(&text[i..end]));
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

// ── Request ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Context {
    pub before: String,
    pub after: String,
    pub app: Option<String>,
    pub earlier: Vec<String>,
}

/// The part of every request that only depends on the mode and vocabulary: the worker
/// caches the model state after it.
fn prefix(mode: &Mode, vocab: &Vocabulary) -> (String, Vec<(String, String)>) {
    let base = base();
    let mut system = format!("{}\n\n{}\n\n{}", base.prompt.trim(), mode.prompt, base.context.trim());
    if !vocab.terms.is_empty() {
        system += &format!("\n\n{} {}", base.vocabulary.trim(), vocab.terms.join(", "));
    }
    // Examples that show context carry their own tags.
    let examples = mode
        .examples
        .iter()
        .map(|(i, o)| (if i.contains("<dictation>") { i.trim().to_string() } else { user_message(&Context::default(), i) }, o.clone()))
        .collect();
    (system, examples)
}

pub fn request(id: u64, mode: &Mode, vocab: &Vocabulary, ctx: &Context, text: &str) -> Value {
    let (system, examples) = prefix(mode, vocab);
    // German runs at ~3 characters per token; leave room for a longer formal version.
    let max_tokens = text.len() * 2 / 3 + 64;
    json!({ "id": id, "system": system, "examples": examples, "user": user_message(ctx, text), "max_tokens": max_tokens })
}

/// Asks the worker to compute the mode's prefix now (after loading, on a mode switch), so
/// the first dictation doesn't wait for it.
pub fn prime_request(id: u64, mode: &Mode, vocab: &Vocabulary) -> Value {
    let (system, examples) = prefix(mode, vocab);
    json!({ "id": id, "system": system, "examples": examples, "prime": true })
}

fn user_message(ctx: &Context, text: &str) -> String {
    let mut s = String::new();
    if let Some(app) = &ctx.app {
        s += &format!("<app>{app}</app>\n");
    }
    if !ctx.earlier.is_empty() {
        s += &format!("<earlier>\n{}\n</earlier>\n", ctx.earlier.join("\n"));
    }
    if !ctx.before.is_empty() {
        s += &format!("<before>{}</before>\n", ctx.before);
    }
    if !ctx.after.is_empty() {
        s += &format!("<after>{}</after>\n", ctx.after);
    }
    s + &format!("<dictation>{text}</dictation>")
}

// ── Checks ──────────────────────────────────────────────────────────────────

const PREFACES: &[&str] = &["hier ist", "hier der", "hier die", "here is", "here's", "bereinigt:", "cleaned:", "korrigiert:"];
const TAGS: &[&str] = &["<dictation>", "</dictation>", "<before>", "</before>", "<after>", "</after>", "<app>", "</app>", "<earlier>", "</earlier>"];

/// Accepts the model's output only if it still is the dictation, cleaned up: not empty, no
/// preface, roughly the same length, mostly the same words. Context the model repeated is
/// cut off first. Otherwise says why not.
pub fn check(input: &str, output: &str, ctx: &Context) -> Result<String, &'static str> {
    let mut out = output.trim().to_string();
    for t in TAGS {
        out = out.replace(t, "");
    }
    out = strip_echo(out.trim(), &ctx.before, &ctx.after);
    let quoted = |s: &str| s.len() > 1 && (s.starts_with('"') && s.ends_with('"') || s.starts_with('„') && s.ends_with('“'));
    if quoted(&out) && !quoted(input.trim()) {
        out = out.trim_matches(['"', '„', '“']).trim().to_string();
    }
    if out.is_empty() {
        return Err("empty");
    }
    let lower = out.to_lowercase();
    if PREFACES.iter().any(|p| lower.starts_with(p)) {
        return Err("preface");
    }
    let (n_in, n_out) = (input.chars().count(), out.chars().count());
    if n_out * 10 < n_in * 4 || n_out > n_in * 2 + 40 {
        return Err("length");
    }
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 2).map(|w| w.to_lowercase()).collect()
    };
    let (wi, wo) = (words(input), words(&out));
    if wo.len() >= 4 {
        // A word counts as kept if an input word shares its first four letters (inflection,
        // case, compounds), so small fixes pass and answers don't.
        let stem = |w: &str| w.chars().take(4).collect::<String>();
        let kept = wo.iter().filter(|w| wi.iter().any(|i| stem(i) == stem(w))).count();
        if kept * 2 < wo.len() {
            return Err("different words");
        }
    }
    Ok(out)
}

/// Small models like to rewrite the whole sentence, field text included. Cuts a repeated
/// end of `before` from the start and a repeated start of `after` from the end (at least
/// 8 characters, whole words, ignoring case).
fn strip_echo(out: &str, before: &str, after: &str) -> String {
    const MIN: usize = 8;
    let mut out = out.to_string();
    let b = before.trim_end().trim_start_matches('…');
    // Longest tail of `before` first: candidates start at word boundaries.
    let starts = std::iter::once(0).chain(b.char_indices().filter(|(_, c)| c.is_whitespace()).map(|(i, c)| i + c.len_utf8()));
    for i in starts {
        let tail = b[i..].trim_start();
        if tail.len() < MIN {
            break;
        }
        if out.len() >= tail.len() && out.is_char_boundary(tail.len()) && out[..tail.len()].to_lowercase() == tail.to_lowercase() {
            let rest = &out[tail.len()..];
            if rest.chars().next().is_none_or(|c| !c.is_alphanumeric()) {
                out = rest.trim_start().to_string();
                break;
            }
        }
    }
    let a = after.trim_start().trim_end_matches('…');
    let ends: Vec<usize> = a.char_indices().filter(|(_, c)| c.is_whitespace()).map(|(i, _)| i).chain(std::iter::once(a.len())).collect();
    for &j in ends.iter().rev() {
        let head = a[..j].trim_end();
        if head.len() < MIN {
            break;
        }
        let cut = out.len().wrapping_sub(head.len());
        if out.len() >= head.len() && out.is_char_boundary(cut) && out[cut..].to_lowercase() == head.to_lowercase() {
            let rest = &out[..cut];
            if rest.chars().last().is_none_or(|c| !c.is_alphanumeric()) {
                out = rest.trim_end().to_string();
                break;
            }
        }
    }
    out
}

// ── Worker process ──────────────────────────────────────────────────────────

pub const STATUS_OFF: u8 = 0;
pub const STATUS_LOADING: u8 = 1;
pub const STATUS_READY: u8 = 2;
pub const STATUS_ERROR: u8 = 3;

pub fn status_name(s: u8) -> &'static str {
    match s {
        STATUS_LOADING => "loading",
        STATUS_READY => "ready",
        STATUS_ERROR => "error",
        _ => "off",
    }
}

#[derive(Debug, Deserialize)]
pub struct Reply {
    pub text: String,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub gen_tokens: u64,
    #[serde(default)]
    pub prefill_ms: u64,
    #[serde(default)]
    pub gen_ms: u64,
}

pub enum Failure {
    Cancelled,
    Timeout,
    Other(anyhow::Error),
}

pub struct Enhancer {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Value>,
    status: Arc<AtomicU8>,
    next_id: u64,
}

/// `lightwispr-llm` next to our own executable (or next to what a symlinked one points
/// to, e.g. ~/.local/bin/lightwispr → a build tree), else from PATH.
fn worker_exe() -> PathBuf {
    let exe = std::env::args_os().next().map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| std::env::current_exe().ok());
    let beside = |p: &Path| p.parent().map(|d| d.join("lightwispr-llm")).filter(|w| w.is_file());
    exe.as_deref()
        .and_then(|e| beside(e).or_else(|| beside(&std::fs::canonicalize(e).ok()?)))
        .unwrap_or_else(|| PathBuf::from("lightwispr-llm"))
}

impl Enhancer {
    pub fn spawn(model: &Path, gpu: bool, status: Arc<AtomicU8>) -> Result<Self> {
        let mut cmd = Command::new(worker_exe());
        cmd.arg(model).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        if gpu {
            cmd.arg("--gpu");
        } else {
            // CPU mode doesn't touch the GPU at all (not even a Vulkan context).
            cmd.env("GGML_VK_VISIBLE_DEVICES", "");
        }
        status.store(STATUS_LOADING, Ordering::SeqCst);
        let mut child = cmd.spawn().context("starting lightwispr-llm (is it installed next to lightwispr?)")?;
        let stdin = child.stdin.take().context("worker stdin")?;
        let stdout = child.stdout.take().context("worker stdout")?;
        let (tx, replies) = mpsc::channel();
        {
            let status = status.clone();
            std::thread::Builder::new().name("llm-reader".into()).spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                    match v.get("event").and_then(Value::as_str) {
                        Some("ready") => {
                            eprintln!("enhancement model loaded in {} ms", v["load_ms"]);
                            status.store(STATUS_READY, Ordering::SeqCst);
                        }
                        Some("error") => {
                            eprintln!("enhancement worker: {}", v["message"].as_str().unwrap_or_default());
                            if status.load(Ordering::SeqCst) == STATUS_LOADING {
                                status.store(STATUS_ERROR, Ordering::SeqCst);
                            }
                        }
                        _ => {
                            let _ = tx.send(v);
                        }
                    }
                }
                // Exited: crashed, or killed on purpose (then status is already off).
                let _ = status.compare_exchange(STATUS_READY, STATUS_ERROR, Ordering::SeqCst, Ordering::SeqCst);
                let _ = status.compare_exchange(STATUS_LOADING, STATUS_ERROR, Ordering::SeqCst, Ordering::SeqCst);
            })?;
        }
        Ok(Self { child, stdin, replies, status, next_id: 1 })
    }

    pub fn is_ready(&self) -> bool {
        self.status.load(Ordering::SeqCst) == STATUS_READY
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Sends a `prime_request` without waiting; its reply is skipped like any stale one.
    pub fn prime(&mut self, req: &Value) {
        let _ = writeln!(self.stdin, "{req}").and_then(|_| self.stdin.flush());
    }

    /// Sends a request built with `request(id, …)` and waits for its reply. `abort` is
    /// polled while waiting (the user cancelled); on abort or timeout the worker is told to
    /// stop generating.
    pub fn run(&mut self, req: &Value, timeout: Duration, abort: impl Fn() -> bool) -> Result<Reply, Failure> {
        let id = req["id"].as_u64().unwrap_or_default();
        writeln!(self.stdin, "{req}").and_then(|_| self.stdin.flush()).map_err(|e| Failure::Other(e.into()))?;
        let end = Instant::now() + timeout;
        loop {
            match self.replies.recv_timeout(Duration::from_millis(40)) {
                Ok(v) if v["id"].as_u64() == Some(id) => {
                    if let Some(e) = v.get("error").and_then(Value::as_str) {
                        return Err(Failure::Other(anyhow!("{e}")));
                    }
                    if v.get("cancelled").is_some() {
                        return Err(Failure::Cancelled);
                    }
                    return serde_json::from_value(v).map_err(|e| Failure::Other(e.into()));
                }
                Ok(_) => {} // a late reply to an earlier, abandoned request
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Failure::Other(anyhow!("worker exited"))),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let failure = if abort() {
                Failure::Cancelled
            } else if Instant::now() >= end {
                Failure::Timeout
            } else {
                continue;
            };
            let _ = writeln!(self.stdin, "{}", json!({ "cancel": id })).and_then(|_| self.stdin.flush());
            return Err(failure);
        }
    }

    /// Waits until the model is loaded (CLI use).
    pub fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let end = Instant::now() + timeout;
        loop {
            match self.status.load(Ordering::SeqCst) {
                STATUS_READY => return Ok(()),
                STATUS_ERROR => bail!("the enhancement model failed to load (see above)"),
                _ if Instant::now() >= end => bail!("the enhancement model didn't load in time"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

impl Drop for Enhancer {
    fn drop(&mut self) {
        self.status.store(STATUS_OFF, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_replaces_whole_words_only() {
        let v = Vocabulary::parse("lightwispr\nLight Whisper -> lightwispr\nBibecoding -> Vibecoding\n# comment\n");
        assert_eq!(v.apply("Ein Modell für Light Whisper, light whisper!"), "Ein Modell für lightwispr, lightwispr!");
        assert_eq!(v.apply("Der Bibecoding Modus, nicht Bibecodings."), "Der Vibecoding Modus, nicht Bibecodings.");
        assert_eq!(v.prompt().as_deref(), Some("lightwispr, Vibecoding."));
        let v = Vocabulary::parse("lightwispr\nClaude Code\nWhisper\nCachyOS\n");
        assert_eq!(
            v.canonicalize("Lightwispr mit claude code und Whisper, whisper leise, auf cachyos."),
            "lightwispr mit Claude Code und Whisper, whisper leise, auf CachyOS."
        );
    }

    #[test]
    fn check_rejects_answers_and_accepts_cleanups() {
        let input = "Kannst du mal in der Daemon Punkt RS nachschauen, warum der Hotkey nicht geht?";
        let none = Context::default();
        assert!(check(input, "Kannst du in `daemon.rs` nachschauen, warum der Hotkey nicht geht?", &none).is_ok());
        assert_eq!(check(input, "", &none), Err("empty"));
        assert_eq!(check(input, "Hier ist der bereinigte Text: Kannst du …", &none), Err("preface"));
        assert_eq!(
            check(input, "Der Hotkey geht nicht, weil Hyprland beim Reload alle Laufzeit-Binds verwirft; registriere sie neu.", &none),
            Err("different words")
        );
        assert_eq!(
            check("Ja, gerne.", "Ja, gerne. Ich komme dann morgen früh um acht Uhr vorbei und bringe alle Unterlagen mit, die du brauchst.", &none),
            Err("length")
        );
        assert_eq!(check("ja gerne bis morgen dann", "\"Ja, gerne. Bis morgen dann.\"", &none).unwrap(), "Ja, gerne. Bis morgen dann.");
    }

    #[test]
    fn repeated_context_is_cut() {
        let ctx = |b: &str, a: &str| Context { before: b.into(), after: a.into(), ..Default::default() };
        let c = ctx("Das Treffen ist am ", " im Büro.");
        assert_eq!(check("Montag um 10 Uhr.", "Das Treffen ist am Montag um 10 Uhr im Büro.", &c).unwrap(), "Montag um 10 Uhr");
        let c = ctx("Ich komme morgen ", "");
        assert_eq!(check("Gegen acht Uhr, wenn das passt.", "Ich komme morgen gegen acht Uhr, wenn das passt.", &c).unwrap(), "gegen acht Uhr, wenn das passt.");
        // Short overlaps are left alone: they may be the person's own words.
        let c = ctx("Ja ", "");
        assert_eq!(check("Ja, passt.", "Ja, passt.", &c).unwrap(), "Ja, passt.");
    }

    #[test]
    fn builtin_modes_parse() {
        for (id, src) in BUILTIN {
            let m = parse_mode(id, src).unwrap();
            assert!(!m.examples.is_empty(), "{id} has examples");
        }
        let _: BaseFile = toml::from_str(BASE).unwrap();
    }
}
