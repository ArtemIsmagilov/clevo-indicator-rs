//! GTK3 + libappindicator user interface.
//!
//! This is a direct port of `main_ui_worker` from the C program: a tray
//! indicator whose label shows the CPU temperature and whose icon reflects
//! the fan RPM, plus a menu for switching between automatic and manual fan
//! control.
//!
//! No `unsafe` is used here: signal handling goes through a self-pipe watched
//! by GLib and child reaping goes through safe `nix` wrappers.

use std::os::fd::AsRawFd;
use std::time::Duration;

use appindicator3::prelude::*;
use appindicator3::{Indicator, IndicatorCategory, IndicatorStatus};
use gtk::glib;
use gtk::prelude::*;
use nix::unistd::Pid;

use crate::ec;
use crate::process;
use crate::shared::Shared;
use crate::signals;
use crate::{AppError, APP_ID, PROC_NAME};

/// How often the indicator label and icon are refreshed (the C code used
/// `g_timeout_add(500, ...)`).
const UPDATE_INTERVAL: Duration = Duration::from_millis(500);

/// How often the UI checks whether the worker process is still alive.
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemType {
    Na,
    Auto,
    Manual,
}

#[derive(Clone, Copy)]
enum Action {
    SetFan,
    Quit,
}

#[derive(Clone, Copy)]
struct MenuEntry {
    label: &'static str,
    option: i32,
    item_type: ItemType,
    action: Action,
}

const MENU: &[MenuEntry] = &[
    MenuEntry {
        label: "Set FAN to AUTO",
        option: 0,
        item_type: ItemType::Auto,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "",
        option: 0,
        item_type: ItemType::Na,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  0%",
        option: 0,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  20%",
        option: 20,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  30%",
        option: 30,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  40%",
        option: 40,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  60%",
        option: 60,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  80%",
        option: 80,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to  90%",
        option: 90,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Set FAN to 100%",
        option: 100,
        item_type: ItemType::Manual,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "",
        option: 0,
        item_type: ItemType::Na,
        action: Action::SetFan,
    },
    MenuEntry {
        label: "Quit",
        option: 0,
        item_type: ItemType::Na,
        action: Action::Quit,
    },
];

/// Runs the indicator main loop until the user quits or the worker dies.
pub fn run(shared: Shared, worker_pid: Pid) -> Result<(), AppError> {
    log::info!("starting indicator");

    // Drop effective root before creating any GUI objects, exactly like the
    // original: only the desktop user may own an indicator.
    process::drop_privileges()?;

    gtk::init().map_err(|_| AppError::Gtk)?;

    // Install the signal pipe and worker watch before doing any UI setup so a
    // signal arriving during startup is handled instead of the default action.
    watch_termination(shared)?;
    watch_worker(shared, worker_pid);

    let menu = gtk::Menu::new();
    // Weak reference used by the menu callbacks. A strong reference would
    // create a cycle: menu -> item -> closure -> menu. It would be bounded by
    // the process lifetime, but a weak reference lets the widgets be freed
    // when the indicator is destroyed.
    let menu_weak = menu.downgrade();

    for entry in MENU {
        let item = if entry.label.is_empty() {
            gtk::SeparatorMenuItem::new().upcast::<gtk::MenuItem>()
        } else {
            gtk::MenuItem::with_label(entry.label)
        };

        if !entry.label.is_empty() {
            let menu_weak = menu_weak.clone();
            let option = entry.option;
            let action = entry.action;
            item.connect_activate(move |_| match action {
                Action::SetFan => {
                    set_fan(shared, option);
                    if let Some(menu) = menu_weak.upgrade() {
                        toggle_menuitems(&menu, option);
                    }
                }
                Action::Quit => {
                    log::info!("clicked on quit");
                    gtk::main_quit();
                }
            });
        }

        menu.append(&item);
    }
    menu.show_all();

    let indicator = Indicator::new(APP_ID, "brasero", IndicatorCategory::Hardware);
    indicator.set_label("Init..", "XX");
    indicator.set_status(IndicatorStatus::Attention);
    // The C code passes -2 (gint32); the binding exposes it as guint32.
    indicator.set_ordering_index((-2i32) as u32);
    indicator.set_title(Some("Clevo"));
    indicator.set_menu(Some(&menu));

    let update_indicator = indicator.clone();
    let mut last_temp = i32::MIN;
    let mut last_icon = i32::MIN;
    glib::timeout_add_local(UPDATE_INTERVAL, move || {
        update(&update_indicator, shared, &mut last_temp, &mut last_icon);
        glib::ControlFlow::Continue
    });

    toggle_menuitems(&menu, shared.fan_duty());

    gtk::main();

    log::info!("UI quit");
    Ok(())
}

/// Shows the "multiple instances" error dialog, if a desktop is available.
pub fn show_multiple_instances_dialog() -> Result<(), AppError> {
    // SAFETY is encapsulated in `process`: this only calls `setuid`.
    process::drop_privileges()?;
    gtk::init().map_err(|_| AppError::Gtk)?;

    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::empty(),
        gtk::MessageType::Error,
        gtk::ButtonsType::Close,
        &format!("Multiple running instances of {PROC_NAME}!"),
    );
    dialog.run();
    dialog.close();
    Ok(())
}

/// Quits the indicator when a termination signal arrives.
///
/// `signal-hook` writes one byte into a self-pipe from each signal handler;
/// GLib watches the read end. This is fully event-driven: no periodic polling
/// and no signal-handler code touching GTK or shared state.
fn watch_termination(shared: Shared) -> Result<(), AppError> {
    let read_fd = signals::install_signal_pipe()?;

    glib::unix_fd_add_local(read_fd.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
        let mut buf = [0u8; 16];
        let _ = nix::unistd::read(&read_fd, &mut buf);

        log::info!("termination signal received, quitting");
        shared.set_exit(1);
        gtk::main_quit();
        glib::ControlFlow::Break
    });

    Ok(())
}

/// Quits the indicator if the EC worker exits or gets reaped unexpectedly.
fn watch_worker(shared: Shared, worker_pid: Pid) {
    glib::timeout_add_local(WORKER_POLL_INTERVAL, move || {
        match process::reap(worker_pid) {
            Ok(Some(status)) => {
                log::info!("EC worker exited: {status:?}");
                shared.set_exit(1);
                gtk::main_quit();
                glib::ControlFlow::Break
            }
            Ok(None) => glib::ControlFlow::Continue,
            Err(err) => {
                log::error!("waitpid failed: {err}");
                shared.set_exit(1);
                gtk::main_quit();
                glib::ControlFlow::Break
            }
        }
    });
}

/// Refreshes the indicator, but only touches GTK/D-Bus when a value changed.
///
/// Updating the label and icon unconditionally every 500 ms generates D-Bus
/// property traffic even when nothing changed; caching the last values keeps
/// the idle process quiet.
fn update(indicator: &Indicator, shared: Shared, last_temp: &mut i32, last_icon: &mut i32) {
    let cpu_temp = shared.cpu_temp();
    if cpu_temp != *last_temp {
        indicator.set_label(&format!("{cpu_temp}℃"), "XXXXXX");
        *last_temp = cpu_temp;
    }

    let load = shared.fan_rpms() as f64 / ec::MAX_FAN_RPM * 100.0;
    // Clamp so a spurious EC reading cannot produce a nonexistent icon name.
    let load_rounded = ((load / 5.0).round() * 5.0).clamp(0.0, 100.0) as i32;
    if load_rounded != *last_icon {
        indicator.set_icon(&format!("brasero-disc-{load_rounded:02}"));
        *last_icon = load_rounded;
    }
}

fn set_fan(shared: Shared, fan_duty: i32) {
    if fan_duty == 0 {
        log::info!("clicked on fan duty auto");
        shared.set_auto_duty(1);
        shared.set_auto_duty_val(0);
        shared.set_manual_next_fan_duty(0);
    } else {
        log::info!("clicked on fan duty: {fan_duty}");
        shared.set_auto_duty(0);
        shared.set_auto_duty_val(0);
        shared.set_manual_next_fan_duty(fan_duty);
    }
}

fn toggle_menuitems(menu: &gtk::Menu, fan_duty: i32) {
    for (child, entry) in menu.children().iter().zip(MENU.iter()) {
        let Some(item) = child.downcast_ref::<gtk::MenuItem>() else {
            continue;
        };
        let sensitive = if fan_duty == 0 {
            entry.item_type != ItemType::Auto
        } else {
            entry.item_type != ItemType::Manual || entry.option != fan_duty
        };
        item.set_sensitive(sensitive);
    }
}
