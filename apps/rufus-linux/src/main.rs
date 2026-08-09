//! Rufus Linux desktop — presentation only; destructive I/O stays in the helper.

mod helper_client;
mod hotplug;
mod state;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use ashpd::desktop::ResponseError;
use helper_client::{RunningHelper, WorkerMessage};
use hotplug::BlockDeviceWatcher;
use rufus_helper_protocol::{HelperEvent, HelperResult};
use slint::{
    CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};

use state::{AppState, BootSelection, DeviceListLabel};

mod generated_ui {
    #![allow(clippy::todo, clippy::unwrap_used)]
    slint::include_modules!();
}
use generated_ui::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let state = Rc::new(RefCell::new(AppState::new()));
    let running_helper = Rc::new(RefCell::new(None::<RunningHelper>));
    let (helper_sender, helper_receiver) = mpsc::channel::<WorkerMessage>();
    let (checksum_sender, checksum_receiver) = mpsc::channel::<Result<String, String>>();
    let (device_change_sender, device_change_receiver) = mpsc::sync_channel::<()>(1);
    let _device_watcher = match BlockDeviceWatcher::spawn(device_change_sender) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            state
                .borrow_mut()
                .push_log(format!("Automatic device detection unavailable: {error}"));
            None
        }
    };

    // Initial static lists
    ui.set_boot_choices(string_model(&[
        "Disk or ISO image",
        "Non bootable",
        "FreeDOS",
        "Windows To Go",
    ]));
    ui.set_partition_choices(string_model(&["MBR", "GPT", "Super floppy (disk image)"]));
    ui.set_target_choices(string_model(&[
        "BIOS or UEFI",
        "UEFI (non CSM)",
        "BIOS (CSM)",
    ]));
    ui.set_cluster_choices(string_model(&[
        "Default",
        "512 bytes",
        "1024 bytes",
        "2048 bytes",
        "4096 bytes",
        "8192 bytes",
        "16 KB",
        "32 KB",
        "64 KB",
    ]));
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let desktop_theme = std::env::var("GTK_THEME").unwrap_or_default();
    ui.set_dark_mode(
        std::env::var("RUFUS_LINUX_THEME")
            .map(|value| !value.eq_ignore_ascii_case("light"))
            .unwrap_or_else(|_| desktop_theme.to_ascii_lowercase().contains("dark")),
    );

    refresh_filesystems(&ui, &state.borrow());
    apply_state_to_ui(&ui, &state.borrow());
    refresh_devices_ui(&ui, &mut state.borrow_mut());

    let helper_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state = Rc::clone(&state);
        let running_helper = Rc::clone(&running_helper);
        helper_timer.start(TimerMode::Repeated, Duration::from_millis(40), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            while let Ok(message) = helper_receiver.try_recv() {
                let mut st = state.borrow_mut();
                match message {
                    WorkerMessage::Event(event) => {
                        let terminal = match &event {
                            HelperEvent::Finished { result, .. } => Some(result.clone()),
                            _ => None,
                        };
                        st.handle_helper_event(&event);
                        if let Some(result) = terminal {
                            match result {
                                HelperResult::Success => st.finish_ok(),
                                HelperResult::Cancelled { .. } => st.cancel_operation(),
                                HelperResult::Failed { message, .. } => st.fail_operation(message),
                            }
                            running_helper.borrow_mut().take();
                        }
                    }
                    WorkerMessage::Failed(error) => {
                        if st.is_busy {
                            st.fail_operation(error);
                        }
                        running_helper.borrow_mut().take();
                    }
                    WorkerMessage::Cancelled => {
                        if st.is_busy {
                            st.cancel_operation();
                        }
                        running_helper.borrow_mut().take();
                    }
                }
                apply_state_to_ui(&ui, &st);
            }
        });
    }

    let checksum_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state = Rc::clone(&state);
        checksum_timer.start(TimerMode::Repeated, Duration::from_millis(60), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            while let Ok(result) = checksum_receiver.try_recv() {
                let mut st = state.borrow_mut();
                match result {
                    Ok(message) => {
                        st.push_log(message.clone());
                        st.status_phase = "HASH".into();
                        st.status_operation = "Checksums ready".into();
                        st.status_line = message;
                        st.status_tone = "neutral".into();
                    }
                    Err(error) => {
                        st.push_log(format!("Checksum error: {error}"));
                        st.status_phase = "ERROR".into();
                        st.status_operation = "Checksum failed".into();
                        st.status_line = error;
                        st.status_tone = "error".into();
                    }
                }
                apply_state_to_ui(&ui, &st);
            }
        });
    }

    let device_change_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state = Rc::clone(&state);
        let mut refresh_pending = false;
        device_change_timer.start(TimerMode::Repeated, Duration::from_millis(150), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            while device_change_receiver.try_recv().is_ok() {
                refresh_pending = true;
            }
            let mut st = state.borrow_mut();
            // Keep the exact device shown in a destructive confirmation stable;
            // the queued refresh runs as soon as the dialog or operation ends.
            if refresh_pending && !st.is_busy && !ui.get_show_confirm() {
                refresh_devices_ui(&ui, &mut st);
                refresh_pending = false;
            }
        });
    }

    // —— Callbacks ——
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_refresh_devices(move || {
            if let Some(ui) = ui_weak.upgrade() {
                refresh_devices_ui(&ui, &mut state.borrow_mut());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_device_selected(move |label| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                if let Some(idx) = st
                    .devices
                    .iter()
                    .position(|d| d.list_label() == label.as_str())
                {
                    let had_placeholder = st.selected_device.is_none();
                    st.select_device(idx);
                    ui.set_selected_device(
                        i32::try_from(idx).unwrap_or(i32::MAX) + i32::from(had_placeholder),
                    );
                }
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_boot_selected(move |value| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.boot_selection = BootSelection::from_label(&value);
                st.recompute();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_select_image(move || {
            if let Some(ui) = ui_weak.upgrade() {
                match pick_image_file() {
                    Ok(Some(path)) => {
                        let mut st = state.borrow_mut();
                        st.set_image(path);
                        apply_state_to_ui(&ui, &st);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let mut st = state.borrow_mut();
                        st.status_phase = "ERROR".into();
                        st.status_operation = "File chooser failed".into();
                        st.status_line = error.clone();
                        st.status_tone = "error".into();
                        st.push_log(error);
                        apply_state_to_ui(&ui, &st);
                    }
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_partition_selected(move |v| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.partition_scheme_label = v.to_string();
                st.recompute();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_target_selected(move |v| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.target_system_label = v.to_string();
                st.recompute();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_filesystem_selected(move |v| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.filesystem_label = v.to_string();
                st.recompute();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_cluster_selected(move |v| {
            if let Some(ui) = ui_weak.upgrade() {
                state.borrow_mut().cluster_label = v.to_string();
                apply_state_to_ui(&ui, &state.borrow());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_option_changed(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.quick_format = ui.get_quick_format();
                st.check_bad_blocks = ui.get_check_bad_blocks();
                st.verify_write = ui.get_verify_write();
                st.list_usb_hdd = ui.get_list_usb_hdd();
                st.list_fixed_disks = ui.get_list_fixed_disks();
                st.volume_label = ui.get_volume_label().to_string();
                st.recompute();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_persistence_changed(move |v| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.persistence_gb = v as f64;
                st.recompute();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_start_clicked(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                st.volume_label = ui.get_volume_label().to_string();
                st.quick_format = ui.get_quick_format();
                st.check_bad_blocks = ui.get_check_bad_blocks();
                st.verify_write = ui.get_verify_write();
                match st.build_confirm() {
                    Ok(body) => {
                        ui.set_confirm_title(st.action_name().into());
                        ui.set_confirm_body(body.into());
                        ui.set_show_confirm(true);
                    }
                    Err(msg) => {
                        st.push_log(format!("Cannot start: {msg}"));
                        ui.set_status_line(msg.into());
                        ui.set_status_tone("error".into());
                        apply_state_to_ui(&ui, &st);
                    }
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_confirm_rejected(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_confirm(false);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let helper_sender = helper_sender.clone();
        let running_helper = Rc::clone(&running_helper);
        ui.on_confirm_accepted(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_confirm(false);
                let mut st = state.borrow_mut();
                let request = match st.build_helper_request() {
                    Ok(r) => r,
                    Err(e) => {
                        st.fail_operation(e);
                        apply_state_to_ui(&ui, &st);
                        return;
                    }
                };
                match helper_client::launch(&request, helper_sender.clone()) {
                    Ok(process) => {
                        st.begin_operation();
                        running_helper.borrow_mut().replace(process);
                    }
                    Err(error) => st.fail_operation(error),
                }
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let running_helper = Rc::clone(&running_helper);
        ui.on_cancel_clicked(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                if let Some(process) = running_helper.borrow().as_ref() {
                    if let Err(error) = process.cancel() {
                        st.push_log(error);
                    }
                }
                st.push_log("Cancel requested.".into());
                st.status_line = "Stopping at the current I/O boundary…".into();
                apply_state_to_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_log(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_log(!ui.get_show_log());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_about(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_about(!ui.get_show_about());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_advanced(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_advanced(!ui.get_show_advanced());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let checksum_sender = checksum_sender.clone();
        ui.on_compute_checksums(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state.borrow_mut();
                let Some(path) = st.image_path.clone() else {
                    st.status_line = "Select an image before computing checksums.".into();
                    apply_state_to_ui(&ui, &st);
                    return;
                };
                st.status_phase = "HASH".into();
                st.status_operation = "Computing checksums…".into();
                st.status_line = format!("Reading {}…", path.display());
                apply_state_to_ui(&ui, &st);
                let sender = checksum_sender.clone();
                std::thread::spawn(move || {
                    let result = rufus_image::compute_checksums(&path, true, true, true, true)
                        .map(|sums| {
                            format!(
                                "MD5 {}\nSHA-1 {}\nSHA-256 {}\nSHA-512 {}",
                                sums.md5.unwrap_or_default(),
                                sums.sha1.unwrap_or_default(),
                                sums.sha256.unwrap_or_default(),
                                sums.sha512.unwrap_or_default()
                            )
                        })
                        .map_err(|error| error.to_string());
                    if sender.send(result).is_err() {
                        // The UI has already closed.
                    }
                });
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_theme(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_dark_mode(!ui.get_dark_mode());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_close_clicked(move || {
            if let Some(ui) = ui_weak.upgrade() {
                if !ui.get_is_busy() {
                    let _ = ui.hide();
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = Rc::clone(&state);
        ui.window().on_close_requested(move || {
            let mut st = state.borrow_mut();
            if st.is_busy {
                st.status_line =
                    "A device operation is active. Cancel it safely before closing Rufus Linux."
                        .into();
                if let Some(ui) = ui_weak.upgrade() {
                    apply_state_to_ui(&ui, &st);
                }
                CloseRequestResponse::KeepWindowShown
            } else {
                CloseRequestResponse::HideWindow
            }
        });
    }

    ui.run()?;
    Ok(())
}

fn string_model(items: &[&str]) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = items.iter().map(|s| SharedString::from(*s)).collect();
    ModelRc::new(VecModel::from(v))
}

fn pick_image_file() -> Result<Option<std::path::PathBuf>, String> {
    let images = FileFilter::new("Disk images")
        .glob("*.iso")
        .glob("*.img")
        .glob("*.raw")
        .glob("*.dmg")
        .glob("*.vhd")
        .glob("*.vhdx")
        .glob("*.gz")
        .glob("*.xz")
        .glob("*.lzma")
        .glob("*.bz2")
        .glob("*.zst")
        .glob("*.zstd")
        .glob("*.zip")
        .glob("*.wim")
        .glob("*.esd")
        .glob("*.ffu");

    pollster::block_on(async {
        let request = SelectedFiles::open_file()
            .title("Select a disk image")
            .accept_label("Select")
            .modal(true)
            .filter(images)
            .filter(FileFilter::new("All files").glob("*"))
            .send()
            .await
            .map_err(|error| format!("Could not open the system file chooser: {error}"))?;

        let selected = match request.response() {
            Ok(selected) => selected,
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Ok(None),
            Err(error) => return Err(format!("System file chooser failed: {error}")),
        };
        selected
            .uris()
            .first()
            .map(|uri| {
                uri.to_file_path()
                    .map_err(|_| "The selected image is not a local file.".to_owned())
            })
            .transpose()
    })
}

fn refresh_filesystems(ui: &AppWindow, state: &AppState) {
    let labels: Vec<SharedString> = state
        .available_filesystems()
        .into_iter()
        .map(SharedString::from)
        .collect();
    ui.set_filesystem_choices(ModelRc::new(VecModel::from(labels)));
}

fn refresh_devices_ui(ui: &AppWindow, state: &mut AppState) {
    state.list_usb_hdd = ui.get_list_usb_hdd();
    state.list_fixed_disks = ui.get_list_fixed_disks();
    state.refresh_devices();
    let mut labels: Vec<SharedString> = state
        .devices
        .iter()
        .map(|d| SharedString::from(d.list_label()))
        .collect();
    if labels.is_empty() {
        ui.set_device_labels(ModelRc::new(VecModel::from(vec![SharedString::from(
            "No removable devices",
        )])));
        ui.set_selected_device(0);
    } else {
        if state.selected_device.is_none() {
            labels.insert(0, SharedString::from("Select a device"));
        }
        ui.set_device_labels(ModelRc::new(VecModel::from(labels)));
        ui.set_selected_device(
            state
                .selected_device
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(0),
        );
    }
    apply_state_to_ui(ui, state);
}

fn apply_state_to_ui(ui: &AppWindow, state: &AppState) {
    refresh_filesystems(ui, state);
    if let Some(dev) = state.selected() {
        ui.set_device_name(dev.display_name.clone().into());
        ui.set_device_path(dev.node.display().to_string().into());
        ui.set_device_capacity(rufus_image::format_size(dev.fingerprint.size_bytes).into());
        ui.set_device_risky(
            !dev.risks.is_empty()
                || matches!(
                    dev.class,
                    rufus_core::device::DeviceClass::Internal
                        | rufus_core::device::DeviceClass::ExternalFixed
                ),
        );
    } else {
        ui.set_device_name("No device selected".into());
        ui.set_device_path("—".into());
        ui.set_device_capacity("—".into());
        ui.set_device_risky(false);
    }

    ui.set_boot_selection(state.boot_selection.label().into());
    ui.set_image_path(
        state
            .image_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_image_summary(state.image_summary.clone().into());
    ui.set_image_notes(state.image_notes.clone().into());
    ui.set_partition_scheme(state.partition_scheme_label.clone().into());
    ui.set_target_system(state.target_system_label.clone().into());
    ui.set_filesystem(state.filesystem_label.clone().into());
    ui.set_cluster_size(state.cluster_label.clone().into());
    ui.set_volume_label(state.volume_label.clone().into());
    ui.set_persistence_enabled(state.persistence_enabled);
    ui.set_persistence_max_gb(state.persistence_max_gb as f32);
    ui.set_persistence_gb(state.persistence_gb as f32);
    ui.set_can_start(state.can_start);
    ui.set_is_busy(state.is_busy);
    ui.set_status_phase(state.status_phase.clone().into());
    ui.set_status_operation(state.status_operation.clone().into());
    ui.set_status_progress(state.status_progress as f32);
    ui.set_status_telemetry(state.status_telemetry.clone().into());
    ui.set_status_tone(state.status_tone.clone().into());
    ui.set_status_active(state.status_active);
    ui.set_status_line(state.status_line.clone().into());
    ui.set_log_text(state.log.join("\n").into());
    ui.set_capability_hint(state.capability_hint.clone().into());
}
