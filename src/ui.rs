//! GTK3 + libappindicator user interface.
//!
//! This is a direct port of `main_ui_worker` from the C program: a tray
//! indicator whose label shows the CPU temperature and whose icon reflects
//! the fan RPM, plus a menu for switching between automatic and manual fan
//! control.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use appindicator3::prelude::*;
use appindicator3::{Indicator, IndicatorCategory, IndicatorStatus};
use gtk::glib;
use gtk::prelude::*;

use crate::ec;
use crate::shared;
use crate::{APP_ID, PROC_NAME};

/// How often the indicator label and icon are refreshed (the C code used
/// `g_timeout_add(500, ...)`).
const UPDATE_INTERVAL: Duration = Duration::from_millis(500);

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

/// Runs the indicator main loop. Never returns until the user quits or the
/// worker process dies.
pub fn run() {
    println!("Indicator...");

    // Drop effective root before creating any GUI objects, exactly like the
    // original: only the desktop user may own an indicator.
    // SAFETY: plain libc call; `getuid` returns the real (desktop) uid.
    unsafe { libc::setuid(libc::getuid()) };

    if gtk::init().is_err() {
        eprintln!("unable to initialize GTK");
        return;
    }

    let menu = gtk::Menu::new();
    let widgets: Rc<RefCell<Vec<gtk::MenuItem>>> = Rc::new(RefCell::new(Vec::new()));

    for entry in MENU {
        let item = if entry.label.is_empty() {
            gtk::SeparatorMenuItem::new().upcast::<gtk::MenuItem>()
        } else {
            gtk::MenuItem::with_label(entry.label)
        };

        if !entry.label.is_empty() {
            let widgets = Rc::clone(&widgets);
            let option = entry.option;
            let action = entry.action;
            item.connect_activate(move |_| match action {
                Action::SetFan => {
                    set_fan(option);
                    toggle_menuitems(&widgets.borrow(), option);
                }
                Action::Quit => {
                    println!("clicked on quit");
                    gtk::main_quit();
                }
            });
        }

        menu.append(&item);
        widgets.borrow_mut().push(item);
    }
    menu.show_all();

    let indicator = Indicator::new(APP_ID, "brasero", IndicatorCategory::Hardware);
    indicator.set_label("Init..", "XX");
    indicator.set_status(IndicatorStatus::Attention);
    // The C code passes -2 (gint); the binding exposes it as guint.
    indicator.set_ordering_index((-2i32) as u32);
    indicator.set_title(Some("Clevo"));
    indicator.set_menu(Some(&menu));

    let update_indicator = indicator.clone();
    glib::timeout_add_local(UPDATE_INTERVAL, move || {
        update(&update_indicator);
        glib::ControlFlow::Continue
    });

    toggle_menuitems(&widgets.borrow(), shared::get().fan_duty());

    gtk::main();
    println!("main on UI quit");
}

/// Shows the "multiple instances" error dialog, if a desktop is available.
pub fn show_multiple_instances_dialog() {
    // SAFETY: plain libc call; drop effective root before GTK starts.
    unsafe { libc::setuid(libc::getuid()) };

    if gtk::init().is_err() {
        return;
    }

    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::empty(),
        gtk::MessageType::Error,
        gtk::ButtonsType::Close,
        &format!("Multiple running instances of {PROC_NAME}!"),
    );
    dialog.run();
    dialog.close();
}

fn update(indicator: &Indicator) {
    let info = shared::get();

    let label = format!("{}℃", info.cpu_temp());
    indicator.set_label(&label, "XXXXXX");

    let load = info.fan_rpms() as f64 / ec::MAX_FAN_RPM * 100.0;
    let load_rounded = (load / 5.0).round() * 5.0;
    indicator.set_icon(&format!("brasero-disc-{:02}", load_rounded as i32));
}

fn set_fan(fan_duty: i32) {
    let info = shared::get();
    if fan_duty == 0 {
        println!("clicked on fan duty auto");
        info.set_auto_duty(1);
        info.set_auto_duty_val(0);
        info.set_manual_next_fan_duty(0);
    } else {
        println!("clicked on fan duty: {fan_duty}");
        info.set_auto_duty(0);
        info.set_auto_duty_val(0);
        info.set_manual_next_fan_duty(fan_duty);
    }
}

fn toggle_menuitems(items: &[gtk::MenuItem], fan_duty: i32) {
    for (item, entry) in items.iter().zip(MENU.iter()) {
        let sensitive = if fan_duty == 0 {
            entry.item_type != ItemType::Auto
        } else {
            entry.item_type != ItemType::Manual || entry.option != fan_duty
        };
        item.set_sensitive(sensitive);
    }
}
