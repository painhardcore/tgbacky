use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct ShutdownFlag {
    requested: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl ShutdownFlag {
    /// Returns the process-wide flag, installing the signal listeners on first use. A batch
    /// export calls this once per chat; fresh listeners each time printed the cancel message
    /// once per chat on a single Ctrl+C.
    pub fn spawn() -> Self {
        static PROCESS_FLAG: OnceLock<ShutdownFlag> = OnceLock::new();
        PROCESS_FLAG.get_or_init(Self::install).clone()
    }

    fn install() -> Self {
        let flag = Self::default();

        let ctrl_c_flag = flag.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            ctrl_c_flag.request("Ctrl+C");
        });

        #[cfg(unix)]
        {
            let sigterm_flag = flag.clone();
            tokio::spawn(async move {
                if let Ok(mut signal) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    signal.recv().await;
                    sigterm_flag.request("SIGTERM");
                }
            });
        }

        flag
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        if self.is_requested() {
            return;
        }
        self.notify.notified().await;
    }

    fn request(&self, source: &str) {
        if !self.requested.swap(true, Ordering::SeqCst) {
            eprintln!(
                "{source} received. Cancelling active downloads, keeping the previous checkpoint, and stopping..."
            );
            self.notify.notify_waiters();
        }
    }

    #[cfg(test)]
    pub fn request_for_test(&self) {
        self.request("test");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_reuses_one_process_flag() {
        let first = ShutdownFlag::spawn();
        let second = ShutdownFlag::spawn();
        // Compare identity only: requesting shutdown here would leak into parallel tests.
        assert!(Arc::ptr_eq(&first.requested, &second.requested));
    }
}
