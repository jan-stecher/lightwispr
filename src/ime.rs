//! Wayland input method (zwp_input_method_v2): knows when a text field has focus and
//! commits text into it directly, without simulated keystrokes.
//!
//! A background thread keeps dispatching events, so the focus state is always current
//! and the connection never backs up.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{ContentHint, ContentPurpose};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{self, ZwpInputMethodV2};

#[derive(Default)]
struct Focus {
    /// Applied on `done`.
    active: bool,
    /// Password/PIN field or marked sensitive: never type into it.
    secret: bool,
    /// Number of `done` events received; `commit` must echo it.
    serial: u32,
    /// The compositor dropped us (another input method, or the connection died).
    gone: bool,
}

struct State {
    shared: Arc<Mutex<Focus>>,
    // Double-buffered until `done`.
    pending_active: bool,
    pending_secret: bool,
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
        let mut state = State { shared: shared.clone(), pending_active: false, pending_secret: false, verbose };
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
        let serial = self.shared.lock().unwrap().serial;
        self.im.commit_string(text.to_string());
        self.im.commit(serial);
        self.conn.flush()?;
        Ok(())
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
