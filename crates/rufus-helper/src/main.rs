//! `rufus-linux-helper` — root-only raw-device operations.
//!
//! Invoked via polkit. Never run the desktop GUI as root.

use std::env;
use std::io;
use std::process::ExitCode;

use rufus_core::progress::CancellationToken;
use rufus_helper::{
    execute, sample_dry_request, validate_request, write_event_line, ExecutionOptions,
    HELPER_VERSION,
};
use rufus_helper_protocol::{decode_line, ClientMessage, HelperEvent, HelperRequest, HelperResult};

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
            let mut input = String::new();
            if let Some(path) = args.next() {
                input = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("read error: {e}");
                        return ExitCode::from(2);
                    }
                };
            } else if io::stdin().read_line(&mut input).is_err() {
                eprintln!("expected JSON request on stdin or path argument");
                return ExitCode::from(2);
            }
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
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_err() {
                eprintln!("expected JSON request on stdin");
                return ExitCode::from(2);
            }
            match decode_line::<HelperRequest>(input.as_bytes()) {
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
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_err() || input.trim().is_empty() {
                print_help();
                return ExitCode::from(2);
            }
            match decode_line::<HelperRequest>(input.as_bytes()) {
                Ok(req) => run_request(req, false),
                Err(e) => {
                    eprintln!("decode error: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn run_request(request: HelperRequest, dry_run: bool) -> ExitCode {
    let job_id = request.job_id;
    let cancel = CancellationToken::new();
    let cancel_watcher = cancel.clone();
    // Best-effort cancel via a second stdin line is not available once we consume stdin;
    // polkit path uses signals. For dry-run this is fine.
    let _ = cancel_watcher;
    let _ = ClientMessage::Cancel {
        job_id: request.job_id,
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
