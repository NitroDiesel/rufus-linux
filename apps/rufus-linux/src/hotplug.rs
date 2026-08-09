//! Lightweight block-device change detection for the desktop application.
//!
//! Linux updates `/sys/class/block` as disks and their partitions appear or
//! disappear. Polling that small directory avoids a daemon dependency and a
//! one-slot channel naturally coalesces bursts of kernel events.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SYS_BLOCK: &str = "/sys/class/block";
const SCAN_INTERVAL: Duration = Duration::from_millis(500);

/// Stops and joins its worker when dropped.
pub struct BlockDeviceWatcher {
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl BlockDeviceWatcher {
    pub fn spawn(changes: SyncSender<()>) -> io::Result<Self> {
        Self::spawn_at(PathBuf::from(SYS_BLOCK), SCAN_INTERVAL, changes)
    }

    fn spawn_at(
        sys_block: PathBuf,
        scan_interval: Duration,
        changes: SyncSender<()>,
    ) -> io::Result<Self> {
        // Establish the baseline before returning so a device connected just
        // after startup cannot race with worker initialization.
        let mut previous = block_names(&sys_block)?;
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("rufus-device-watch".into())
            .spawn(move || loop {
                let (lock, wake) = &*worker_stop;
                let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let (guard, _) = wake
                    .wait_timeout_while(guard, scan_interval, |should_stop| !*should_stop)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *guard {
                    break;
                }
                drop(guard);

                let Ok(current) = block_names(&sys_block) else {
                    // A transient sysfs read failure is not a device removal.
                    continue;
                };
                if current == previous {
                    continue;
                }
                previous = current;
                match changes.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) => {}
                    Err(TrySendError::Disconnected(())) => break,
                }
            })?;

        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for BlockDeviceWatcher {
    fn drop(&mut self) {
        let (lock, wake) = &*self.stop;
        let mut should_stop = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *should_stop = true;
        wake.notify_one();
        drop(should_stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn block_names(sys_block: &Path) -> io::Result<Vec<OsString>> {
    let mut names = fs::read_dir(sys_block)?
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect::<Vec<_>>();
    names.sort_unstable();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("rufus-hotplug-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn snapshot_is_stable_regardless_of_directory_order() {
        let root = test_directory();
        fs::create_dir_all(root.join("sdc")).expect("create sdc fixture");
        fs::create_dir_all(root.join("sda")).expect("create sda fixture");

        let names = block_names(&root).expect("scan fixture");
        assert_eq!(names, vec![OsString::from("sda"), OsString::from("sdc")]);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn watcher_reports_a_device_change_and_stops_cleanly() {
        let root = test_directory();
        fs::create_dir_all(root.join("sda")).expect("create initial fixture");
        let (sender, receiver) = mpsc::sync_channel(1);
        let watcher = BlockDeviceWatcher::spawn_at(root.clone(), Duration::from_millis(10), sender)
            .expect("start watcher");

        fs::create_dir_all(root.join("sdb")).expect("add device fixture");
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("receive device change");

        drop(watcher);
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
