//! Unprivileged client for the short-lived, polkit-authorized helper.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc, Mutex};

use rufus_helper_protocol::{decode_line, encode_line, HelperEvent, HelperRequest};

const PKEXEC: &str = "/usr/bin/pkexec";
const PKACTION: &str = "/usr/bin/pkaction";
const INSTALLED_HELPER: &str = "/usr/libexec/rufus-linux-helper";
const INSTALLED_POLICY: &str =
    "/usr/share/polkit-1/actions/io.github.nitrodiesel.rufus-linux.policy";
const ACTION_ID: &str = "io.github.nitrodiesel.rufus-linux.helper";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const APPIMAGE_ENVIRONMENT: [&str; 12] = [
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "GCONV_PATH",
    "GI_TYPELIB_PATH",
    "GTK_PATH",
    "LD_AUDIT",
    "LD_DEBUG",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "OWD",
    "PYTHONPATH",
];

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
    helper_readiness().is_ok()
}

fn trusted_system_file(path: &str, executable: bool) -> Result<fs::Metadata, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("{path} is unavailable: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "{path} must be owned by root and not writable by group or other users"
        ));
    }
    if executable && metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{path} is not executable"));
    }
    Ok(metadata)
}

fn helper_readiness() -> Result<(), String> {
    if !Path::new(PKEXEC).is_file() || !Path::new(PKACTION).is_file() {
        return Err(
            "polkit is not installed; install pkexec and pkaction for authorized disk writes"
                .into(),
        );
    }
    trusted_system_file(INSTALLED_HELPER, true)?;
    let policy_metadata = trusted_system_file(INSTALLED_POLICY, false)?;
    if policy_metadata.len() > MAX_POLICY_BYTES {
        return Err(format!("{INSTALLED_POLICY} is unexpectedly large"));
    }
    let policy = fs::read_to_string(INSTALLED_POLICY)
        .map_err(|error| format!("could not read {INSTALLED_POLICY}: {error}"))?;
    if !policy_authorizes_helper(&policy) {
        return Err("the installed polkit policy does not authorize the packaged helper".into());
    }
    Ok(())
}

fn policy_authorizes_helper(policy: &str) -> bool {
    policy.contains(&format!("<action id=\"{ACTION_ID}\">"))
        && policy.contains(&format!(
            "<annotate key=\"org.freedesktop.policykit.exec.path\">{INSTALLED_HELPER}</annotate>"
        ))
}

fn sanitize_appimage_environment(command: &mut Command) {
    for variable in APPIMAGE_ENVIRONMENT {
        command.env_remove(variable);
    }
}

fn installed_helper_is_compatible() -> Result<(), String> {
    let mut command = Command::new(INSTALLED_HELPER);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    sanitize_appimage_environment(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("could not query the privileged helper version: {error}"))?;
    let expected = format!("rufus-linux-helper {}", env!("CARGO_PKG_VERSION"));
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != expected {
        return Err(format!(
            "the installed privileged helper is incompatible; reinstall Rufus Linux {}",
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

fn registered_action_authorizes_helper() -> Result<(), String> {
    let mut command = Command::new(PKACTION);
    command
        .args(["--verbose", "--action-id", ACTION_ID])
        .env("LC_ALL", "C");
    sanitize_appimage_environment(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("could not inspect the installed polkit action: {error}"))?;
    if !output.status.success() {
        return Err("the Rufus Linux polkit action is not registered".into());
    }
    let details = String::from_utf8_lossy(&output.stdout);
    let expected = format!("org.freedesktop.policykit.exec.path -> {INSTALLED_HELPER}");
    if !details.contains(&expected) {
        return Err("the registered polkit action does not target the packaged helper".into());
    }
    Ok(())
}

pub fn launch(
    request: &HelperRequest,
    sender: Sender<WorkerMessage>,
) -> Result<RunningHelper, String> {
    helper_readiness()?;
    installed_helper_is_compatible()?;
    registered_action_authorizes_helper()?;

    let mut command = Command::new(PKEXEC);
    command
        .arg(INSTALLED_HELPER)
        .arg("--execute-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize_appimage_environment(&mut command);
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

    #[test]
    fn policy_must_name_the_action_and_exact_helper_path() {
        let valid = format!(
            "<action id=\"{ACTION_ID}\"><annotate key=\"org.freedesktop.policykit.exec.path\">{INSTALLED_HELPER}</annotate></action>"
        );
        assert!(policy_authorizes_helper(&valid));
        assert!(!policy_authorizes_helper(
            &valid.replace(INSTALLED_HELPER, "/tmp/helper")
        ));
        assert!(!policy_authorizes_helper(
            &valid.replace(ACTION_ID, "org.example.other")
        ));
    }
}
