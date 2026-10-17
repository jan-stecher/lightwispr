//! Context for a dictation: the focused field's text around the cursor (where the app
//! reports it), the app and window, and what was dictated into that window shortly before
//! (the fallback for terminals, Warp and XWayland apps, which report nothing).
//! Everything stays in memory; nothing here is written to disk.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::deliver::Window;
use crate::ime::Surrounding;

/// How much field text goes into the prompt.
const MAX_BEFORE: usize = 1500;
const MAX_AFTER: usize = 500;
const MAX_TITLE: usize = 120;
/// Earlier dictations into the same window count as context for this long.
const RECENT_FOR: Duration = Duration::from_secs(15 * 60);
const RECENT_MAX: usize = 3;

/// The focused field around the cursor; the dictation replaces whatever is selected.
#[derive(Debug, Default)]
pub struct Field {
    pub before: String,
    pub after: String,
}

impl Field {
    pub fn from(s: &Surrounding) -> Self {
        let (a, b) = (s.cursor.min(s.anchor), s.cursor.max(s.anchor));
        Self { before: s.text[..a].to_string(), after: s.text[b..].to_string() }
    }

    /// The parts that go into the prompt: the end of `before`, the start of `after`,
    /// cut at word boundaries.
    pub fn trimmed(&self) -> (String, String) {
        (tail(&self.before, MAX_BEFORE), head(&self.after, MAX_AFTER))
    }
}

/// Fits the text between what's left and right of the cursor: a space where words would
/// touch, and no final period when the sentence goes on after the cursor.
pub fn join(text: &str, field: &Field) -> String {
    let mut out = text.to_string();
    if let (Some(prev), Some(first)) = (field.before.chars().last(), out.chars().next()) {
        if !prev.is_whitespace() && !"([{„“\"'«‹/-–—".contains(prev) && (first.is_alphanumeric() || "„“\"(".contains(first)) {
            out.insert(0, ' ');
        }
    }
    // The sentence goes on after the cursor: no period before its next word.
    if field.after.trim_start().chars().next().is_some_and(char::is_lowercase) && out.ends_with('.') && !out.ends_with("..") {
        out.pop();
    }
    if let (Some(next), Some(last)) = (field.after.chars().next(), out.chars().last()) {
        if next.is_alphanumeric() && !last.is_whitespace() {
            out.push(' ');
        }
    }
    out
}

/// One line about where the text goes, e.g. `dev.warp.Warp (terminal), window "✳ lightwispr"`.
pub fn app_line(win: &Window) -> String {
    let kind = if win.is_terminal() { " (terminal)" } else { "" };
    let title = head(&win.title, MAX_TITLE);
    if title.is_empty() { format!("{}{kind}", win.class) } else { format!("{}{kind}, window \"{title}\"", win.class) }
}

/// Recent dictations per window (Hyprland address).
#[derive(Default)]
pub struct Recent(HashMap<String, Vec<(Instant, String)>>);

impl Recent {
    pub fn add(&mut self, window: &str, text: &str) {
        if window.is_empty() {
            return;
        }
        let list = self.0.entry(window.to_string()).or_default();
        list.push((Instant::now(), text.to_string()));
        if list.len() > RECENT_MAX {
            list.remove(0);
        }
        self.0.retain(|_, l| l.last().is_some_and(|(t, _)| t.elapsed() < RECENT_FOR));
    }

    pub fn get(&self, window: &str) -> Vec<String> {
        self.0
            .get(window)
            .map(|l| l.iter().filter(|(t, _)| t.elapsed() < RECENT_FOR).map(|(_, s)| tail(s, 600)).collect())
            .unwrap_or_default()
    }
}

/// The last `max` bytes of `s`, starting at a word boundary.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = s.len() - max;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    let cut = &s[i..];
    match cut.find(char::is_whitespace) {
        Some(w) => format!("…{}", cut[w..].trim_start()),
        None => cut.to_string(),
    }
}

/// The first `max` bytes of `s`, ending at a word boundary.
fn head(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    let cut = &s[..i];
    match cut.rfind(char::is_whitespace) {
        Some(w) => format!("{}…", cut[..w].trim_end()),
        None => cut.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(before: &str, after: &str) -> Field {
        Field { before: before.into(), after: after.into() }
    }

    #[test]
    fn join_adds_spaces_and_drops_period_mid_sentence() {
        assert_eq!(join("Falls nicht.", &field("verbessert werden.", "")), " Falls nicht.");
        assert_eq!(join("Montag.", &field("am ", " im Büro")), "Montag");
        assert_eq!(join("Montag.", &field("am ", "im Büro")), "Montag ");
        assert_eq!(join("Hallo.", &field("", "")), "Hallo.");
        assert_eq!(join("Hallo.", &field("Text\n", "")), "Hallo.");
        assert_eq!(join("x", &field("(", ")")), "x");
    }

    #[test]
    fn trimming_respects_words_and_utf8() {
        let long = "äöü ".repeat(500);
        let (b, a) = field(&long, &long).trimmed();
        assert!(b.starts_with('…') && b.len() <= MAX_BEFORE + 4);
        assert!(a.ends_with('…') && a.len() <= MAX_AFTER + 4);
    }
}
