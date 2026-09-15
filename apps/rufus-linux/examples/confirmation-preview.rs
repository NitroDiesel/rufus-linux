//! Non-destructive visual fixture. No device discovery or helper callbacks.
#[path = "../src/confirmation.rs"]
mod confirmation;
mod generated_ui {
    #![allow(clippy::todo, clippy::unwrap_used)]
    slint::include_modules!();
}
use slint::ComponentHandle;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = i_slint_backend_winit::Backend::builder()
        .with_window_attributes_hook(|attributes| {
            attributes.with_inner_size(i_slint_backend_winit::winit::dpi::LogicalSize::new(
                560.0, 720.0,
            ))
        })
        .build()?;
    slint::platform::set_platform(Box::new(backend))?;
    let ui = generated_ui::AppWindow::new()?;
    let action = if std::env::args().any(|arg| arg == "--format") {
        "Format device"
    } else {
        "Write image"
    };
    let mut body = rufus_core::safety::confirmation_message(
        action,
        "Preview USB device (synthetic)",
        "/dev/preview-only",
        "32.0 GB",
        Some("PREVIEW-NOT-A-DEVICE"),
    );
    if action == "Write image" {
        body.push_str("\n\n");
        body.push_str(&confirmation::source_details(
            &rufus_core::plan::ImageSource {
                path: if std::env::args().any(|arg| arg == "--long") {
                    format!(
                        "/images/{}recovery-disk.vhdx",
                        "nested-directory-for-keyboard-scroll-check/".repeat(12)
                    )
                    .into()
                } else {
                    "/images/recovery-disk-with-a-long-descriptive-filename.vhdx".into()
                },
                kind: rufus_core::plan::ImageSourceKind::Vhdx,
                size_bytes: 8 * 1024 * 1024,
                decompressed_size_bytes: Some(16 * 1024 * 1024),
                sha256: None,
            },
        ));
        body.push_str("\n\nBad-block testing will overwrite all data and take a long time.\n\nEnd of preview details.");
    }
    ui.set_dark_mode(std::env::var("RUFUS_LINUX_THEME").as_deref() == Ok("dark"));
    ui.set_confirm_title(action.into());
    ui.set_confirm_body(body.into());
    ui.set_show_confirm(true);
    ui.on_refresh_devices(|| {
        eprintln!("background refresh escaped the modal");
        std::process::exit(2);
    });
    ui.on_confirm_accepted(|| {
        println!("Preview accepted; no operation was run.");
        let _ = slint::quit_event_loop();
    });
    ui.on_confirm_rejected(|| {
        println!("Preview cancelled.");
        let _ = slint::quit_event_loop();
    });
    ui.run()?;
    Ok(())
}
