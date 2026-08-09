//! Unprivileged client for the short-lived, polkit-authorized helper.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc, Mutex};

use rufus_helper_protocol::{decode_line, encode_line, HelperEvent, HelperRequest};

const PKEXEC: &str = "/usr/bin/pkexec";
const INSTALLED_HELPER: &str = "/usr/libexec/rufus-linux-helper";

#[derive(Debug)]
pub enum WorkerMessage {
    Event(HelperEvent),
    Failed(String),
    Cancelled,
}

pub struct RunningHelper {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
}

impl RunningHelper {
    pub fn cancel(&self) -> Result<(), String> {
        self.cancelled.store(true, Ordering::Release);
        let guard = self
            .child
            .lock()
            .map_err(|_| "helper process lock was poisoned".to_owned())?;
        if let Some(child) = guard.as_ref() {
            request_termination(child)
                .map_err(|error| format!("could not request helper cancellation: {error}"))?;
        }
        Ok(())
    }
}

fn request_termination(child: &Child) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(child.id())
        .map_err(|_| std::io::Error::other("helper pid was outside the platform range"))?;
    if unsafe { libc::kill(-pid, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn isolate_process_group(command: &mut Command) {
    // SAFETY: setpgid is async-signal-safe and no allocations occur after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub fn helper_available() -> bool {
    Path::new(PKEXEC).is_file() && Path::new(INSTALLED_HELPER).is_file()
}

pub fn launch(
    request: &HelperRequest,
    sender: Sender<WorkerMessage>,
) -> Result<RunningHelper, String> {
    if !Path::new(PKEXEC).is_file() {
        return Err("pkexec is not installed; install polkit for authorized disk writes".into());
    }
    if !Path::new(INSTALLED_HELPER).is_file() {
        return Err(format!(
            "privileged helper is not installed at {INSTALLED_HELPER}; install the Rufus Linux package"
        ));
    }

    let mut command = Command::new(PKEXEC);
    command
        .arg(INSTALLED_HELPER)
        .arg("--execute-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not request authorization: {error}"))?;

    let request_line = encode_line(request)
        .map_err(|error| format!("could not encode helper request: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "helper stdin was not available".to_owned())?;
    stdin
        .write_all(&request_line)
        .map_err(|error| format!("could not send helper request: {error}"))?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "helper stdout was not available".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "helper stderr was not available".to_owned())?;
    let shared_child = Arc::new(Mutex::new(Some(child)));
    let thread_child = Arc::clone(&shared_child);
    let cancelled = Arc::new(AtomicBool::new(false));
    let thread_cancelled = Arc::clone(&cancelled);

    std::thread::spawn(move || {
        let stderr_thread = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });

        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(mut line) = line else {
                break;
            };
            if line.is_empty() {
                continue;
            }
            line.push(b'\n');
            match decode_line::<HelperEvent>(&line) {
                Ok(event) => {
                    if sender.send(WorkerMessage::Event(event)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(WorkerMessage::Failed(format!(
                        "invalid response from privileged helper: {error}"
                    )));
                    return;
                }
            }
        }

        let status = thread_child
            .lock()
            .ok()
            .and_then(|mut guard| guard.as_mut().and_then(|child| child.wait().ok()));
        if let Ok(mut guard) = thread_child.lock() {
            *guard = None;
        }
        let stderr_text = stderr_thread.join().unwrap_or_default();

        if thread_cancelled.load(Ordering::Acquire) {
            let _ = sender.send(WorkerMessage::Cancelled);
        } else if status.is_none_or(|status| !status.success()) {
            let detail = if stderr_text.is_empty() {
                "authorization was denied or the privileged helper failed".to_owned()
            } else {
                stderr_text
            };
            let _ = sender.send(WorkerMessage::Failed(detail));
        }
    });

    Ok(RunningHelper {
        child: shared_child,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_requests_sigterm_instead_of_forcing_sigkill() {
        use std::os::unix::process::ExitStatusExt;

        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        isolate_process_group(&mut command);
        let mut child = command.spawn().expect("spawn sleep fixture");
        request_termination(&child).expect("send SIGTERM");
        let status = child.wait().expect("reap sleep fixture");
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }
}
