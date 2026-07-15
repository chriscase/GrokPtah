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
                                s.backlog.extend_from_slice(&chunk);
                                // Cap backlog ~256 KiB
                                if s.backlog.len() > 256 * 1024 {
                                    let drop = s.backlog.len() - 256 * 1024;
                                    s.backlog.drain(..drop);
                                }
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
                                s.backlog.extend_from_slice(&chunk);
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
