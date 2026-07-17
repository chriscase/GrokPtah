//! Multi-session PTY hub — forwards stdout/stderr to the UI via ring buffers + events.
//!
//! #135: retain `Child`, kill+wait on close (no zombies / leaked reader threads).
//! #136: UI may hide the pane without calling kill; only explicit kill terminates shells.
//! #138: capped backlog, sequenced chunks, UTF-8 boundary buffering, exit events + reap.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Soft cap for PTY output replay buffers (~256 KiB).
pub const PTY_BACKLOG_CAP: usize = 256 * 1024;

/// Append `chunk` to a backlog ring, dropping from the front when over cap.
pub fn append_backlog_capped(backlog: &mut Vec<u8>, chunk: &[u8], cap: usize) {
    if chunk.is_empty() {
        return;
    }
    if chunk.len() >= cap {
        backlog.clear();
        backlog.extend_from_slice(&chunk[chunk.len() - cap..]);
        return;
    }
    backlog.extend_from_slice(chunk);
    if backlog.len() > cap {
        let drop_n = backlog.len() - cap;
        backlog.drain(..drop_n);
    }
}

/// Decode complete UTF-8 from `pending + chunk`, leaving an incomplete trailing
/// sequence in `pending` so multi-byte characters split across reads are not corrupted (#138).
pub fn push_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    match std::str::from_utf8(pending) {
        Ok(s) => {
            let out = s.to_owned();
            pending.clear();
            out
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            let valid = std::str::from_utf8(&pending[..valid_up_to])
                .unwrap_or("")
                .to_owned();
            if e.error_len().is_none() {
                // Incomplete sequence at end — keep the tail for the next read.
                let rest = pending[valid_up_to..].to_vec();
                pending.clear();
                pending.extend_from_slice(&rest);
                valid
            } else {
                // Invalid byte sequence — emit replacement and skip the bad byte.
                let bad_len = e.error_len().unwrap_or(1).max(1);
                let after = valid_up_to + bad_len;
                let mut out = valid;
                out.push('\u{FFFD}');
                let rest = if after < pending.len() {
                    pending[after..].to_vec()
                } else {
                    Vec::new()
                };
                pending.clear();
                if !rest.is_empty() {
                    // Recurse for remaining bytes after the bad sequence.
                    out.push_str(&push_utf8_chunk(pending, &rest));
                }
                out
            }
        }
    }
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    /// Owned shell/command process — must be killed+waited (#135).
    child: Box<dyn Child + Send + Sync>,
    backlog: Vec<u8>,
    /// Last sequence number written into backlog / emitted to UI (#138).
    last_seq: u64,
    killed: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyOutputEvent {
    pub id: String,
    pub data: String,
    pub seq: u64,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyExitEvent {
    pub id: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyBacklog {
    pub data: String,
    /// Highest seq included in `data` (0 if empty). Live events with `seq <= up_to_seq` are duplicates.
    pub up_to_seq: u64,
}

pub struct PtyHub {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    app: Mutex<Option<AppHandle>>,
}

impl PtyHub {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            app: Mutex::new(None),
        }
    }

    pub fn set_app(&self, app: AppHandle) {
        *self.app.lock() = Some(app);
    }

    pub fn create(&self, cols: u16, rows: u16) -> Result<String> {
        self.spawn_inner(None, cols, rows)
    }

    /// Create a PTY that runs a one-shot command (agent tool terminal attach).
    pub fn create_command(&self, command: &str, cols: u16, rows: u16) -> Result<String> {
        self.spawn_inner(Some(command), cols, rows)
    }

    fn spawn_inner(&self, command: Option<&str>, cols: u16, rows: u16) -> Result<String> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(default_shell());
        cmd.env("TERM", "xterm-256color");
        if let Some(c) = command {
            cmd.arg("-lc");
            cmd.arg(c);
        }
        let child = pair.slave.spawn_command(cmd)?;
        // Drop slave so the child is the only holder; master close → EOF to reader.
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;
        let id = Uuid::new_v4().to_string();
        let id_for_thread = id.clone();
        let sessions = self.sessions.clone();
        let app = self.app.lock().clone();

        self.sessions.lock().insert(
            id.clone(),
            PtySession {
                master: pair.master,
                writer,
                child,
                backlog: Vec::new(),
                last_seq: 0,
                killed: false,
            },
        );

        // Reader thread exits on EOF (master closed / process exit) or killed flag.
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut utf8_pending = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let raw = &buf[..n];
                        let text = push_utf8_chunk(&mut utf8_pending, raw);
                        let seq = {
                            let mut g = sessions.lock();
                            if let Some(s) = g.get_mut(&id_for_thread) {
                                if s.killed {
                                    break;
                                }
                                // Backlog stores raw bytes (cap applies to bytes, not lossy text).
                                append_backlog_capped(&mut s.backlog, raw, PTY_BACKLOG_CAP);
                                s.last_seq = s.last_seq.saturating_add(1);
                                s.last_seq
                            } else {
                                break;
                            }
                        };
                        if text.is_empty() {
                            continue;
                        }
                        if let Some(app) = &app {
                            let _ = app.emit(
                                "pty://output",
                                PtyOutputEvent {
                                    id: id_for_thread.clone(),
                                    data: text,
                                    seq,
                                },
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
            // Flush any leftover incomplete UTF-8 as lossy.
            if !utf8_pending.is_empty() {
                let text = String::from_utf8_lossy(&utf8_pending).into_owned();
                utf8_pending.clear();
                if !text.is_empty() {
                    let seq = {
                        let mut g = sessions.lock();
                        if let Some(s) = g.get_mut(&id_for_thread) {
                            append_backlog_capped(&mut s.backlog, text.as_bytes(), PTY_BACKLOG_CAP);
                            s.last_seq = s.last_seq.saturating_add(1);
                            Some(s.last_seq)
                        } else {
                            None
                        }
                    };
                    if let (Some(app), Some(seq)) = (&app, seq) {
                        let _ = app.emit(
                            "pty://output",
                            PtyOutputEvent {
                                id: id_for_thread.clone(),
                                data: text,
                                seq,
                            },
                        );
                    }
                }
            }
            // Natural exit / EOF: reap child and drop FDs so we don't leak (#135/#138).
            {
                let mut g = sessions.lock();
                if let Some(mut s) = g.remove(&id_for_thread) {
                    s.killed = true;
                    let _ = s.child.wait();
                    drop(s.writer);
                    drop(s.master);
                    drop(s.child);
                }
            }
            if let Some(app) = &app {
                let _ = app.emit("pty://exit", PtyExitEvent { id: id_for_thread });
            }
        });

        Ok(id)
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        let mut g = self.sessions.lock();
        let s = g.get_mut(id).ok_or_else(|| anyhow!("unknown pty"))?;
        s.writer.write_all(data)?;
        s.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        let g = self.sessions.lock();
        let s = g.get(id).ok_or_else(|| anyhow!("unknown pty"))?;
        s.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Terminate the shell process, wait for reaping, drop FDs so the reader exits.
    pub fn kill(&self, id: &str) -> Result<()> {
        let mut g = self.sessions.lock();
        let mut s = g.remove(id).ok_or_else(|| anyhow!("unknown pty"))?;
        s.killed = true;
        // Kill process first, then close master/writer so reader gets EOF.
        let _ = s.child.kill();
        let _ = s.child.wait();
        drop(s.writer);
        drop(s.master);
        drop(s.child);
        Ok(())
    }

    pub fn list(&self) -> Vec<String> {
        self.sessions.lock().keys().cloned().collect()
    }

    /// True if a live session with this id still exists (for attach-without-create).
    pub fn exists(&self, id: &str) -> bool {
        self.sessions.lock().contains_key(id)
    }

    /// Backlog text + seq watermark for replaying when switching tabs / reopening the pane.
    pub fn backlog(&self, id: &str) -> Result<PtyBacklog> {
        let g = self.sessions.lock();
        let s = g.get(id).ok_or_else(|| anyhow!("unknown pty"))?;
        Ok(PtyBacklog {
            data: String::from_utf8_lossy(&s.backlog).into_owned(),
            up_to_seq: s.last_seq,
        })
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backlog_cap_drops_from_front() {
        let mut b = Vec::new();
        append_backlog_capped(&mut b, b"hello ", 10);
        append_backlog_capped(&mut b, b"world!!", 10);
        assert_eq!(b.len(), 10);
        assert_eq!(std::str::from_utf8(&b).unwrap(), "lo world!!");
    }

    #[test]
    fn backlog_oversized_chunk_keeps_tail() {
        let mut b = vec![b'x'; 5];
        append_backlog_capped(&mut b, &[b'y'; 20], 8);
        assert_eq!(b.len(), 8);
        assert!(b.iter().all(|&c| c == b'y'));
    }

    #[test]
    fn backlog_under_cap_preserves_all() {
        let mut b = Vec::new();
        append_backlog_capped(&mut b, b"abc", 100);
        append_backlog_capped(&mut b, b"def", 100);
        assert_eq!(b, b"abcdef");
    }

    #[test]
    fn kill_unknown_returns_error() {
        let hub = PtyHub::new();
        assert!(hub.kill("no-such-id").is_err());
    }

    #[test]
    fn utf8_split_across_chunks_preserves_multibyte() {
        // "你" is E4 BD A0 in UTF-8
        let yi = "你".as_bytes();
        assert_eq!(yi.len(), 3);
        let mut pending = Vec::new();
        let part1 = push_utf8_chunk(&mut pending, &yi[..1]);
        assert_eq!(part1, "");
        assert_eq!(pending.len(), 1);
        let part2 = push_utf8_chunk(&mut pending, &yi[1..]);
        assert_eq!(part2, "你");
        assert!(pending.is_empty());
    }

    #[test]
    fn utf8_ascii_passthrough() {
        let mut pending = Vec::new();
        let s = push_utf8_chunk(&mut pending, b"hello");
        assert_eq!(s, "hello");
        assert!(pending.is_empty());
    }
}
