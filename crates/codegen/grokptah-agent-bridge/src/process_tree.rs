//! Process-group setup and bounded tree termination for agent shell commands.

use tokio::process::{Child, Command};

pub(crate) fn configure(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
}

pub(crate) fn terminate_now(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // Shells are spawned as process-group leaders, so a negative PID
        // targets the complete descendant group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }

    let _ = child.start_kill();
}

pub(crate) async fn terminate(child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut taskkill = Command::new("taskkill");
        taskkill.args(["/PID", &pid.to_string(), "/T", "/F"]);
        crate::spawn_env::scrub_tokio_command(&mut taskkill);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), taskkill.status()).await;
    }

    terminate_now(child);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await;
}
