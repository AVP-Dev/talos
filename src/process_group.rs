use std::{io, process::ExitStatus};

use anyhow::{Context, Result};
use tokio::process::Child;

#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::time;

#[cfg(unix)]
const TERMINATION_GRACE: Duration = Duration::from_millis(750);

/// Owns the process group created with `Command::process_group(0)`.
///
/// Keep the leader's `Child` unreaped while this guard is armed. The child PID is also the new
/// group ID, so retaining the unreaped child prevents that numeric group ID from being reused.
/// Disarm immediately after `Child::wait` returns. Cleanup after reaping is intentionally not
/// attempted because TALOS cannot prove that a still-existing numeric PGID is the old group.
pub struct ProcessGroupGuard {
    #[cfg(unix)]
    group_id: i32,
    armed: bool,
}

impl ProcessGroupGuard {
    pub fn new(process_id: u32) -> Result<Self> {
        #[cfg(unix)]
        let group_id = i32::try_from(process_id)
            .context("Child process ID is out of range for its Unix process group")?;
        #[cfg(not(unix))]
        let _ = process_id;
        Ok(Self {
            #[cfg(unix)]
            group_id,
            armed: true,
        })
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Terminate the known process group while the child PID is still reserved, then reap it.
    pub async fn terminate_and_wait(&mut self, child: &mut Child) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        {
            let _ = signal_process_group(self.group_id, libc::SIGTERM);
            time::sleep(TERMINATION_GRACE).await;
            let kill_result = signal_process_group(self.group_id, libc::SIGKILL);
            if kill_result.is_err() {
                let _ = child.start_kill();
            }
            let status = child.wait().await?;
            self.disarm();
            kill_result?;
            Ok(status)
        }
        #[cfg(not(unix))]
        {
            child.kill().await?;
            let status = child.wait().await?;
            self.disarm();
            Ok(status)
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            #[cfg(unix)]
            let _ = signal_process_group(self.group_id, libc::SIGKILL);
        }
    }
}

#[cfg(unix)]
fn signal_process_group(group_id: i32, signal: i32) -> io::Result<()> {
    let result = unsafe { libc::kill(-group_id, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    tracing::warn!(%error, group_id, signal, "Could not signal child process group");
    Err(error)
}
