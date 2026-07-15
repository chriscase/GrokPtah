//! Simple multi-session PTY hub for the integrated terminal pane.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use uuid::Uuid;

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
}

pub struct PtyHub {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
}

impl PtyHub {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
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
        // Drain output on a background thread so the PTY doesn't block.
        // UI reads via optional polling later; for v1 we keep process alive.
        let mut reader = pair.master.try_clone_reader()?;
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        let id = Uuid::new_v4().to_string();
        self.sessions.lock().insert(
            id.clone(),
            PtySession {
                master: pair.master,
                writer,
            },
        );
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
        g.remove(id).ok_or_else(|| anyhow!("unknown pty"))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<String> {
        self.sessions.lock().keys().cloned().collect()
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
}
