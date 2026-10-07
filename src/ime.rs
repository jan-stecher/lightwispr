//! Wayland input method (zwp_input_method_v2): knows when a text field has focus and
//! commits text into it directly, without simulated keystrokes.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{self, ZwpInputMethodV2};

#[derive(Default)]
struct State {
    /// Double-buffered: activate/deactivate set `pending`, `done` applies it.
    pending_active: bool,
    active: bool,
    /// Number of `done` events received; `commit` must echo it.
    serial: u32,
    unavailable: bool,
    /// Log state changes to stderr (for `lightwispr ime-watch`).
    verbose: bool,
}

pub struct Ime {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    im: ZwpInputMethodV2,
}

impl Ime {
    pub fn connect(verbose: bool) -> Result<Self> {
        let conn = Connection::connect_to_env().context("no Wayland connection")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).context("no wl_seat")?;
        let manager: ZwpInputMethodManagerV2 = globals
            .bind(&qh, 1..=1, ())
            .context("compositor has no zwp_input_method_manager_v2")?;
        let im = manager.get_input_method(&seat, &qh, ());
        let mut state = State { verbose, ..Default::default() };
        queue.roundtrip(&mut state)?;
        if state.unavailable {
            bail!("another input method is already running (fcitx5/ibus?)");
        }
        Ok(Self { conn, queue, state, im })
    }

    /// Processes pending events without blocking.
    pub fn poll(&mut self) -> Result<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        self.conn.flush()?;
        if let Some(guard) = self.conn.prepare_read() {
            // Non-blocking: only read if data is there.
            let fd = guard.connection_fd();
            let mut pfd = [libc_pollfd(fd)];
            if poll_fd(&mut pfd, 0) {
                guard.read()?;
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }

    /// Blocks until the next batch of events arrives.
    pub fn wait(&mut self) -> Result<()> {
        self.queue.blocking_dispatch(&mut self.state)?;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.state.active
    }

    /// Waits up to `timeout` for a text field to become focused (e.g. after a panel closes).
    pub fn wait_active(&mut self, timeout: Duration) -> Result<bool> {
        let end = Instant::now() + timeout;
        self.queue.roundtrip(&mut self.state)?;
        while !self.state.active && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(15));
            self.poll()?;
        }
        Ok(self.state.active)
    }

    /// Inserts `text` at the cursor of the focused text field.
    pub fn commit(&mut self, text: &str) -> Result<()> {
        self.im.commit_string(text.to_string());
        self.im.commit(self.state.serial);
        self.queue.roundtrip(&mut self.state)?;
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
            Event::Activate => state.pending_active = true,
            Event::Deactivate => state.pending_active = false,
            Event::Done => {
                state.serial += 1;
                if state.verbose && state.active != state.pending_active {
                    eprintln!("text field {}", if state.pending_active { "focused" } else { "unfocused" });
                }
                state.active = state.pending_active;
            }
            Event::ContentType { hint, purpose } if state.verbose => {
                eprintln!("  content type: purpose={purpose:?} hint={hint:?}")
            }
            Event::Unavailable => state.unavailable = true,
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

// Minimal poll(2) wrapper so we don't pull in a whole crate for one syscall.
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

fn libc_pollfd(fd: std::os::fd::BorrowedFd<'_>) -> PollFd {
    use std::os::fd::AsRawFd;
    PollFd { fd: fd.as_raw_fd(), events: 1 /* POLLIN */, revents: 0 }
}

fn poll_fd(fds: &mut [PollFd], timeout_ms: i32) -> bool {
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
    }
    unsafe { poll(fds.as_mut_ptr(), fds.len() as u64, timeout_ms) > 0 }
}
