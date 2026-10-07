//! Line-based IPC over a Unix socket: the CLI sends one command, gets one JSON line back.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("lightwispr")
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("sock")
}

pub fn request(cmd: &str) -> Result<String> {
    let mut stream = UnixStream::connect(socket_path()).context("lightwispr daemon is not running")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{cmd}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line.trim_end().to_string())
}
