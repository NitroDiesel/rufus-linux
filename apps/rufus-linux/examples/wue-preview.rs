//! Non-destructive visual fixture for the Windows User Experience dialog.
//! No device discovery or helper callbacks. `--silent` opens the silent
//! install confirmation; `--expert` adds the S Mode option.
#[allow(dead_code)]
#[path = "../src/wue.rs"]
mod wue;
mod generated_ui {
    #![allow(clippy::todo, clippy::unwrap_used)]
    slint::include_modules!();
}
use std::cell::RefCell;
use std::rc::Rc;

use generated_ui::{AppWindow, WueRow};
use rufus_image::windows::{WindowsEdition, WindowsImage};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use wue::WueOption;

fn rows(options: &[WueOption], selection: &[WueOption]) -> (Vec<WueRow>, i32) {
    let silent_allowed = wue::silent_allowed(selection);
    let mut next = 0;
    let mut take = |focusable: bool| {
        if focusable {
            next += 1;
            next - 1
        } else {
            -1
        }
    };
    let rows = options
        .iter()
        .map(|&option| {
            let (label, detail) = option.text();
            let enabled = option != WueOption::SilentInstall || silent_allowed;
            let kind = match option {
                WueOption::LocalAccount => "username",
                WueOption::SilentInstall => "edition",
                _ => "",
            };
            let slot = take(enabled);
            let control_slot = take(enabled && !kind.is_empty());
            WueRow {
                label: label.into(),
                detail: detail.into(),
                checked: selection.contains(&option),
                enabled,
                kind: kind.into(),
                slot,
                control_slot,
            }
        })
        .collect();
    (rows, next + 2)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scale: f64 = std::env::var("SLINT_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1.0);
    let backend = i_slint_backend_winit::Backend::builder()
        .with_window_attributes_hook(move |attributes| {
            attributes.with_resizable(false).with_inner_size(
                i_slint_backend_winit::winit::dpi::LogicalSize::new(560.0, 720.0)
                    .to_physical::<f64>(scale),
            )
        })
        .build()?;
    slint::platform::set_platform(Box::new(backend))?;
    let ui = AppWindow::new()?;
    ui.set_dark_mode(std::env::var("RUFUS_LINUX_THEME").as_deref() == Ok("dark"));
    let windows = WindowsImage {
        major: 11,
        build: 26300,
        has_bootmgr_efi: true,
        editions: ["Home", "Education", "Pro", "Pro for Workstations"]
            .iter()
            .enumerate()
            .map(|(i, name)| WindowsEdition {
                index: i as u32 + 1,
                name: format!("Windows 11 {name}"),
                display_name: format!("Windows 11 {name}"),
            })
            .collect(),
        ..WindowsImage::default()
    };
    let options = wue::offered(&windows, std::env::args().any(|arg| arg == "--expert"));
    let selection = Rc::new(RefCell::new(wue::default_selection()));
    let editions: Vec<SharedString> = windows
        .editions
        .iter()
        .map(|edition| edition.display_name.as_str().into())
        .collect();
    ui.set_wue_edition(editions[wue::default_edition(&windows)].clone());
    ui.set_wue_editions(ModelRc::new(VecModel::from(editions)));
    ui.set_wue_username("preview".into());
    let (initial, slots) = rows(&options, &selection.borrow());
    ui.set_wue_rows(ModelRc::new(VecModel::from(initial)));
    ui.set_wue_slot_count(slots);
    ui.set_show_wue(true);
    ui.set_show_silent_warning(std::env::args().any(|arg| arg == "--silent"));
    {
        let ui_weak = ui.as_weak();
        let selection = Rc::clone(&selection);
        ui.on_wue_toggled(move |index, on| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut selection = selection.borrow_mut();
            let option = options[index as usize];
            selection.retain(|selected| *selected != option);
            if on {
                selection.push(option);
            }
            if !wue::silent_allowed(&selection) {
                selection.retain(|selected| *selected != WueOption::SilentInstall);
            }
            let (rows, slots) = rows(&options, &selection);
            ui.set_wue_slot_count(slots);
            let current = ui.get_wue_rows();
            let model = current
                .as_any()
                .downcast_ref::<VecModel<WueRow>>()
                .expect("rows model");
            for (index, row) in rows.into_iter().enumerate() {
                model.set_row_data(index, row);
            }
        });
    }
    ui.on_refresh_devices(|| {
        eprintln!("background refresh escaped the modal");
        std::process::exit(2);
    });
    ui.on_wue_accepted(|| {
        println!("Preview accepted; no operation was run.");
        let _ = slint::quit_event_loop();
    });
    ui.on_wue_rejected(|| {
        println!("Preview cancelled.");
        let _ = slint::quit_event_loop();
    });
    {
        let ui_weak = ui.as_weak();
        ui.on_silent_rejected(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_silent_warning(false);
            }
        });
    }
    ui.on_silent_accepted(|| {
        println!("Silent install accepted; no operation was run.");
        let _ = slint::quit_event_loop();
    });
    ui.run()?;
    Ok(())
}
