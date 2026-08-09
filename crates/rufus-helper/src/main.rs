//! `rufus-linux-helper` — root-only raw-device operations.
//!
//! Invoked via polkit. Never run the desktop GUI as root.

use std::env;
use std::io::{self, BufRead, Read};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rufus_core::progress::CancellationToken;
use rufus_helper::{
    execute, sample_dry_request, validate_request, write_event_line, ExecutionOptions,
    HELPER_VERSION,
};
use rufus_helper_protocol::{
    decode_line, HelperEvent, HelperRequest, HelperResult, MAX_MESSAGE_BYTES,
};

static TERMINATION_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_termination_signal(_signal: libc::c_int) {
    TERMINATION_REQUESTED.store(true, Ordering::Release);
}

struct SignalWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch_termination_signals(cancel: CancellationToken) -> io::Result<SignalWatcher> {
    TERMINATION_REQUESTED.store(false, Ordering::Release);
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = handle_termination_signal as usize;
    action.sa_flags = 0;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for signal in [libc::SIGTERM, libc::SIGINT] {
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let watcher = thread::spawn(move || {
        while !thread_stop.load(Ordering::Acquire) {
            if TERMINATION_REQUESTED.load(Ordering::Acquire) {
                cancel.request();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    Ok(SignalWatcher {
        stop,
        thread: Some(watcher),
    })
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("-V") => {
            println!("rufus-linux-helper {HELPER_VERSION}");
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("--dry-run") => {
            // Safe smoke path: validate + emit stages without raw I/O.
            let target = args.next().unwrap_or_else(|| "/dev/null".into());
            let request = sample_dry_request(target.into());
            run_request(request, true)
        }
        Some("--validate-json") => {
            let input = if let Some(path) = args.next() {
                match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("read error: {e}");
                        return ExitCode::from(2);
                    }
                }
            } else {
                match read_stdin_request() {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(error) => {
                        eprintln!("request read error: {error}");
                        return ExitCode::from(2);
                    }
                }
            };
            match decode_line::<HelperRequest>(input.as_bytes()) {
                Ok(req) => match validate_request(&req) {
                    Ok(()) => {
                        println!("{{\"ok\":true,\"job_id\":\"{}\"}}", req.job_id);
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{{\"ok\":false,\"error\":\"{e}\"}}");
                        ExitCode::from(1)
                    }
                },
                Err(e) => {
                    eprintln!("decode error: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Some("--execute-json") => {
            let input = match read_stdin_request() {
                Ok(input) => input,
                Err(error) => {
                    eprintln!("request read error: {error}");
                    return ExitCode::from(2);
                }
            };
            match decode_line::<HelperRequest>(&input) {
                Ok(req) => run_request(req, false),
                Err(e) => {
                    eprintln!("decode error: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Some(other) => {
            eprintln!("unknown argument: {other}");
            print_help();
            ExitCode::from(2)
        }
        None => {
            // Default: read one NDJSON request from stdin (polkit spawn path).
            let input = match read_stdin_request() {
                Ok(input) => input,
                Err(error) => {
                    eprintln!("request read error: {error}");
                    return ExitCode::from(2);
                }
            };
            if input.iter().all(u8::is_ascii_whitespace) {
                print_help();
                return ExitCode::from(2);
            }
            match decode_line::<HelperRequest>(&input) {
                Ok(req) => run_request(req, false),
                Err(e) => {
                    eprintln!("decode error: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn read_stdin_request() -> io::Result<Vec<u8>> {
    let stdin = io::stdin();
    let mut input = Vec::new();
    let mut limited = stdin.lock().take((MAX_MESSAGE_BYTES + 1) as u64);
    limited.read_until(b'\n', &mut input)?;
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request exceeds protocol size limit",
        ));
    }
    Ok(input)
}

fn run_request(request: HelperRequest, dry_run: bool) -> ExitCode {
    let job_id = request.job_id;
    let cancel = CancellationToken::new();
    let _signal_watcher = match watch_termination_signals(cancel.clone()) {
        Ok(watcher) => watcher,
        Err(error) => {
            let result = HelperResult::Failed {
                code: "signal-handler".into(),
                message: format!("could not install cancellation handler: {error}"),
            };
            let mut stdout = io::stdout().lock();
            let _ = write_event_line(&mut stdout, &HelperEvent::Finished { job_id, result });
            return ExitCode::from(1);
        }
    };

    let mut stdout = io::stdout().lock();
    let result = execute(
        request,
        ExecutionOptions { dry_run, cancel },
        Box::new(move |event| {
            let mut out = io::stdout().lock();
            let _ = write_event_line(&mut out, &event);
        }),
    );

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            let result = match e {
                rufus_helper::HelperError::Cancelled => HelperResult::Cancelled {
                    message: "Operation cancelled; target media may be incomplete.".into(),
                },
                other => HelperResult::Failed {
                    code: "helper-operation".into(),
                    message: other.to_string(),
                },
            };
            let _ = write_event_line(&mut stdout, &HelperEvent::Finished { job_id, result });
            ExitCode::from(1)
        }
    }
}

fn print_help() {
    eprintln!(
        "\
rufus-linux-helper {HELPER_VERSION}
Privileged raw-device helper for Rufus Linux (do not run the GUI as root).

Usage:
  rufus-linux-helper --version
  rufus-linux-helper --dry-run [TARGET_NODE]
  rufus-linux-helper --validate-json [FILE]
  rufus-linux-helper --execute-json   # NDJSON HelperRequest on stdin
  rufus-linux-helper                  # same as --execute-json (polkit path)
"
    );
}
