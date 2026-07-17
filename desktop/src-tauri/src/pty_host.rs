//! Multi-session PTY hub — forwards stdout/stderr to the UI via ring buffers + events.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Soft cap for PTY output replay buffers (~256 KiB).
pub const PTY_BACKLOG_CAP: usize = 256 * 1024;

/// Append `chunk` to a backlog ring, dropping from the front when over cap.
///
/// Pure helper so unit tests can exercise drain boundaries without a live PTY (#142).
pub fn append_backlog_capped(backlog: &mut Vec<u8>, chunk: &[u8], cap: usize) {
    if chunk.is_empty() {
        return;
    }
    if chunk.len() >= cap {
        // Keep only the tail of an oversized chunk.
        backlog.clear();
        backlog.extend_from_slice(&chunk[chunk.len() - cap..]);
        return;
    }
    backlog.extend_from_slice(chunk);
    if backlog.len() > cap {
        let drop = backlog.len() - cap;
        backlog.drain(..drop);
    }
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    /// Recent output for tab switch replay.
    backlog: Vec<u8>,
    killed: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyOutputEvent {
    pub id: String,
    pub data: String,
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
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(default_shell());
        cmd.env("TERM", "xterm-256color");
        let _child = pair.slave.spawn_command(cmd)?;
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
                backlog: Vec::new(),
                killed: false,
            },
        );

        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = buf[..n].to_vec();
                        {
                            let mut g = sessions.lock();
                            if let Some(s) = g.get_mut(&id_for_thread) {
                                if s.killed {
                                    break;
                                }
                                append_backlog_capped(
                                    &mut s.backlog,
                                    &chunk,
                                    PTY_BACKLOG_CAP,
                                );
                            } else {
                                break;
                            }
                        }
                        if let Some(app) = &app {
                            let text = String::from_utf8_lossy(&chunk).into_owned();
                            let _ = app.emit(
                                "pty://output",
                                PtyOutputEvent {
                                    id: id_for_thread.clone(),
                                    data: text,
                                },
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(id)
    }

    /// Create a PTY that runs a one-shot command (agent tool terminal attach).
    pub fn create_command(&self, command: &str, cols: u16, rows: u16) -> Result<String> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(default_shell());
        cmd.env("TERM", "xterm-256color");
        cmd.arg("-lc");
        cmd.arg(command);
        let _child = pair.slave.spawn_command(cmd)?;
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
                backlog: Vec::new(),
                killed: false,
            },
        );

        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = buf[..n].to_vec();
                        {
                            let mut g = sessions.lock();
                            if let Some(s) = g.get_mut(&id_for_thread) {
                                if s.killed {
                                    break;
                                }
                                append_backlog_capped(
                                    &mut s.backlog,
                                    &chunk,
                                    PTY_BACKLOG_CAP,
                                );
                            } else {
                                break;
                            }
                        }
                        if let Some(app) = &app {
                            let text = String::from_utf8_lossy(&chunk).into_owned();
                            let _ = app.emit(
                                "pty://output",
                                PtyOutputEvent {
                                    id: id_for_thread.clone(),
                                    data: text,
                                },
                            );
                        }
                    }
                    Err(_) => break,
                }
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

    pub fn kill(&self, id: &str) -> Result<()> {
        let mut g = self.sessions.lock();
        if let Some(mut s) = g.remove(id) {
            s.killed = true;
        } else {
            return Err(anyhow!("unknown pty"));
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<String> {
        self.sessions.lock().keys().cloned().collect()
    }

    /// Backlog text for replaying when switching tabs.
    pub fn backlog(&self, id: &str) -> Result<String> {
        let g = self.sessions.lock();
        let s = g.get(id).ok_or_else(|| anyhow!("unknown pty"))?;
        Ok(String::from_utf8_lossy(&s.backlog).into_owned())
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
        // "hello world!!" is 13 bytes → keep last 10
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
}
