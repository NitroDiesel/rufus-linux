//! Unprivileged client for the short-lived, polkit-authorized helper.

use std::io::{BufRead, BufReader, Write};
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
        let mut guard = self
            .child
            .lock()
            .map_err(|_| "helper process lock was poisoned".to_owned())?;
        if let Some(child) = guard.as_mut() {
            child
                .kill()
                .map_err(|error| format!("could not stop helper: {error}"))?;
        }
        Ok(())
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

    let mut child = Command::new(PKEXEC)
        .arg(INSTALLED_HELPER)
        .arg("--execute-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
