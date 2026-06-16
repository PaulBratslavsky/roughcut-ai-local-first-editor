//! In-app terminal sessions: a real login shell behind a pseudo-terminal,
//! streamed to an xterm.js panel in the webview. Local-first stays the
//! default — this is just an optional surface for driving the MCP server
//! (or anything else) from a shell without leaving the app.
//!
//! Design notes, learned from the reference Tauri terminals (which all get
//! these wrong for an *embedded* terminal):
//!   * Output is PUSHED from a blocking reader thread as `terminal-output`
//!     events — not polled from JS every animation frame.
//!   * Output is shipped as raw bytes (base64), never `from_utf8`'d here: a
//!     multibyte char can land on a read boundary and would otherwise be
//!     dropped. xterm writes the bytes directly.
//!   * A shell that exits emits `terminal-exit` and the app keeps running.
//!     (The common `process::exit(code)` pattern would close the editor.)
//!
//! Sessions are keyed by a frontend-supplied id, so multiple terminals are a
//! free extension even though the UI ships one to begin with.

use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, SharedSink};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Mutex, OnceLock};

/// Live sessions. The master + writer live here for resize/write; the reader
/// and child are owned by the per-session reader thread.
struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static S: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pty_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(format!("pty: {e}"))
}

/// Spawn a login shell in a new PTY. `mcp` (url, token), when present, is
/// injected into the shell env so a one-click "connect" can wire Claude Code
/// to the running MCP server without the user copy-pasting secrets.
pub fn spawn(
    sink: SharedSink,
    id: String,
    cols: u16,
    rows: u16,
    mcp: Option<(String, String)>,
) -> Result<()> {
    // Replacing an existing id: tear the old one down first so we don't leak a
    // shell or double-bind the id.
    kill(&id).ok();

    let pair = native_pty_system()
        .openpty(PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 })
        .map_err(pty_err)?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    // Login shell so the user's normal rc files (prompt, aliases, PATH) load.
    cmd.arg("-l");
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
    // `clear`, cursor addressing, colours.
    cmd.env("TERM", "xterm-256color");
    if let Some((url, token)) = mcp {
        cmd.env("ROUGHCUT_MCP_URL", url);
        cmd.env("ROUGHCUT_MCP_TOKEN", token);
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(pty_err)?;
    // The parent must drop its handle to the slave or the reader never sees EOF.
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(pty_err)?;
    let writer = pair.master.take_writer().map_err(pty_err)?;

    sessions()
        .lock()
        .unwrap()
        .insert(id.clone(), Session { master: pair.master, writer, killer });

    // Blocking reader. portable-pty's reader is synchronous, so this is a
    // dedicated OS thread rather than a tokio task.
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break, // EOF or the master was torn down
                Ok(n) => send(&sink, CoreEvent::terminal_output(id.clone(), &buf[..n])),
            }
        }
        // Reap for the exit code; the session may already be gone (explicit
        // kill), which is fine.
        let code = child.wait().ok().map(|s| s.exit_code() as i64);
        sessions().lock().unwrap().remove(&id);
        send(&sink, CoreEvent::terminal_exit(id, code));
    });

    Ok(())
}

/// Forward keystrokes (raw UTF-8 bytes from xterm's `onData`) to the shell.
pub fn write(id: &str, bytes: &[u8]) -> Result<()> {
    let mut map = sessions().lock().unwrap();
    let session = map.get_mut(id).ok_or_else(|| pty_err("no such session"))?;
    session.writer.write_all(bytes).map_err(pty_err)?;
    session.writer.flush().map_err(pty_err)
}

/// Resize the PTY to match the xterm grid (cols/rows, not pixels).
pub fn resize(id: &str, cols: u16, rows: u16) -> Result<()> {
    let map = sessions().lock().unwrap();
    let session = map.get(id).ok_or_else(|| pty_err("no such session"))?;
    session
        .master
        .resize(PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 })
        .map_err(pty_err)
}

/// Kill a session's shell. The reader thread then hits EOF and emits
/// `terminal-exit`; this just removes our handles and signals the child.
pub fn kill(id: &str) -> Result<()> {
    if let Some(mut session) = sessions().lock().unwrap().remove(id) {
        let _ = session.killer.kill();
    }
    Ok(())
}

/// Kill every session — wired into the app's exit hook so no shells outlive
/// the window.
pub fn kill_all() {
    let mut map = sessions().lock().unwrap();
    for (_, mut session) in map.drain() {
        let _ = session.killer.kill();
    }
}
