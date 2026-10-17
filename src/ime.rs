//! Wayland input method (zwp_input_method_v2): knows when a text field has focus and
//! commits text into it directly, without simulated keystrokes.
//!
//! A background thread keeps dispatching events, so the focus state is always current
//! and the connection never backs up.
//!
//! Apps that support it also report the text around the cursor ("surrounding text", max.
//! 4000 bytes): GTK, Qt, Firefox (paragraph), Chromium/Electron (±100 characters).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{ContentHint, ContentPurpose};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{self, ZwpInputMethodV2};

/// Text around the cursor as the app reports it; offsets are bytes into `text`.
#[derive(Clone, Debug)]
pub struct Surrounding {
    pub text: String,
    pub cursor: usize,
    pub anchor: usize,
}

/// The protocol caps a committed string at 4000 bytes; longer text goes in several commits.
const MAX_COMMIT: usize = 3800;

#[derive(Default)]
struct Focus {
    /// Applied on `done`.
    active: bool,
    /// Password/PIN field or marked sensitive: never type into it.
    secret: bool,
    /// Number of `done` events received; `commit` must echo it.
    serial: u32,
    /// None when the app doesn't report it.
    surrounding: Option<Surrounding>,
    /// The compositor dropped us (another input method, or the connection died).
    gone: bool,
}

struct State {
    shared: Arc<Mutex<Focus>>,
    // Double-buffered until `done`.
    pending_active: bool,
    pending_secret: bool,
    pending_surrounding: Option<Surrounding>,
    /// A new field got focus: forget the old field's text on the next done.
    pending_reset: bool,
    verbose: bool,
}

pub struct Ime {
    conn: Connection,
    im: ZwpInputMethodV2,
    shared: Arc<Mutex<Focus>>,
}

impl Ime {
    /// Registers as the input method and starts the event thread.
    pub fn spawn(verbose: bool) -> Result<Self> {
        let conn = Connection::connect_to_env().context("no Wayland connection")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).context("no wl_seat")?;
        let manager: ZwpInputMethodManagerV2 = globals
            .bind(&qh, 1..=1, ())
            .context("compositor has no zwp_input_method_manager_v2")?;
        let im = manager.get_input_method(&seat, &qh, ());
        let shared = Arc::new(Mutex::new(Focus::default()));
        let mut state = State { shared: shared.clone(), pending_active: false, pending_secret: false, pending_surrounding: None, pending_reset: false, verbose };
        queue.roundtrip(&mut state)?;
        if shared.lock().unwrap().gone {
            bail!("another input method is already running (fcitx5/ibus?)");
        }
        std::thread::Builder::new().name("ime".into()).spawn(move || {
            while queue.blocking_dispatch(&mut state).is_ok() {}
            state.shared.lock().unwrap().gone = true;
        })?;
        Ok(Self { conn, im, shared })
    }

    pub fn is_active(&self) -> bool {
        self.shared.lock().unwrap().active
    }

    pub fn is_gone(&self) -> bool {
        self.shared.lock().unwrap().gone
    }

    /// Waits up to `timeout` for a normal (non-secret) text field to have focus,
    /// e.g. right after a bar panel closed.
    pub fn wait_typable(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            {
                let f = self.shared.lock().unwrap();
                if f.gone {
                    return false;
                }
                if f.active && !f.secret {
                    return true;
                }
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Inserts `text` at the cursor of the focused text field.
    pub fn commit(&self, text: &str) -> Result<()> {
        let mut rest = text;
        while !rest.is_empty() {
            let mut cut = rest.len().min(MAX_COMMIT);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (chunk, tail) = rest.split_at(cut);
            let serial = self.shared.lock().unwrap().serial;
            self.im.commit_string(chunk.to_string());
            self.im.commit(serial);
            self.conn.flush()?;
            rest = tail;
            if !rest.is_empty() {
                // A second string before the app applied the first would replace it.
                self.wait_done(serial, Duration::from_millis(300));
            }
        }
        Ok(())
    }

    /// The focused field's text around the cursor, if it reports any (never for secret
    /// fields). Asks for a fresh copy first: an empty commit makes Hyprland acknowledge the
    /// app's own commits, which Chromium waits for before it reports changes again
    /// (otherwise its text goes stale after the first insert).
    pub fn surrounding(&self, wait: Duration) -> Option<Surrounding> {
        let serial = {
            let f = self.shared.lock().unwrap();
            if !f.active || f.secret {
                return None;
            }
            f.serial
        };
        self.im.commit(serial);
        let _ = self.conn.flush();
        self.wait_done(serial, wait);
        let f = self.shared.lock().unwrap();
        if f.active && !f.secret { f.surrounding.clone() } else { None }
    }

    /// Waits until a `done` newer than `serial` arrived, or `timeout` passed.
    fn wait_done(&self, serial: u32, timeout: Duration) {
        let end = Instant::now() + timeout;
        while self.shared.lock().unwrap().serial == serial && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

impl Drop for Ime {
    fn drop(&mut self) {
        self.im.destroy();
        let _ = self.conn.flush();
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_input_method_v2::Event;
        match event {
            Event::Activate => {
                state.pending_active = true;
                state.pending_secret = false;
                state.pending_surrounding = None;
                state.pending_reset = true;
            }
            Event::SurroundingText { text, cursor, anchor } => {
                let clamp = |i: u32| {
                    let mut i = (i as usize).min(text.len());
                    while !text.is_char_boundary(i) {
                        i -= 1;
                    }
                    i
                };
                let (cursor, anchor) = (clamp(cursor), clamp(anchor));
                if state.verbose {
                    let (a, b) = (cursor.min(anchor), cursor.max(anchor));
                    eprintln!("  surrounding: {:?}▏{:?}▕{:?}", &text[..a], &text[a..b], &text[b..]);
                }
                state.pending_surrounding = Some(Surrounding { text, cursor, anchor });
            }
            Event::Deactivate => state.pending_active = false,
            Event::ContentType { hint, purpose } => {
                let secret_purpose = matches!(purpose, WEnum::Value(ContentPurpose::Password | ContentPurpose::Pin));
                let secret_hint = matches!(hint, WEnum::Value(h) if h.contains(ContentHint::SensitiveData));
                state.pending_secret = secret_purpose || secret_hint;
                if state.verbose {
                    eprintln!("  content type: purpose={purpose:?} hint={hint:?}");
                }
            }
            Event::Done => {
                let mut f = state.shared.lock().unwrap();
                f.serial += 1;
                if state.verbose && f.active != state.pending_active {
                    eprintln!("text field {}", if state.pending_active { "focused" } else { "unfocused" });
                }
                f.active = state.pending_active;
                f.secret = state.pending_secret;
                // Keep the last report within one focus; a new field starts empty.
                if std::mem::take(&mut state.pending_reset) || !f.active {
                    f.surrounding = None;
                }
                if let Some(s) = state.pending_surrounding.take() {
                    f.surrounding = Some(s);
                }
            }
            Event::Unavailable => state.shared.lock().unwrap().gone = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpInputMethodManagerV2, _: <ZwpInputMethodManagerV2 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
