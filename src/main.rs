//! Clevo fan control indicator.
//!
//! Rust port of `clevo-indicator.c` by AqD <iiiaqd@gmail.com>, based on the
//! fan control code by Jonas Diemer.
//!
//! The program keeps the original two-process design:
//!
//! * the parent process runs as the desktop user and owns the GTK3 /
//!   libappindicator UI;
//! * the forked child re-acquires root and talks to the Clevo EC;
//! * both processes exchange state through a shared anonymous mapping.
//!
//! The executable must be installed setuid-root and started by the desktop
//! user, just like the C version.

mod ec;
mod shared;
mod ui;
mod worker;

use std::env;
use std::fs;
use std::io;
use std::process::{self, ExitCode};
use std::sync::atomic::{AtomicI32, Ordering};

/// AppIndicator / D-Bus ID for the Rust build. Kept separate from
/// [`PROC_NAME`] so the indicator identity differs from the C version.
pub const APP_ID: &str = "clevo-indicator-rs";

/// Process name used by the single-instance guard.
///
/// Linux truncates `/proc/<pid>/comm` to 15 bytes and the C version uses
/// `clevo-indicator` as its name, so this must stay 15 characters or less and
/// must match the C program. That way the C and Rust builds refuse to run at
/// the same time and cannot poke the EC concurrently.
pub const PROC_NAME: &str = "clevo-indicator";

/// PID of the UI parent, observed by the EC worker.
static PARENT_PID: AtomicI32 = AtomicI32::new(0);

const HELP: &str = "\
Usage: clevo-indicator-rs [fan-duty-percentage]

Dump/Control fan duty on Clevo laptops. Display indicator by default.

Arguments:
  [fan-duty-percentage]\t\tTarget fan duty in percentage, from 20 to 100
  -?\t\t\t\tDisplay this help and exit

Without arguments this program should attempt to display an indicator in
the Ubuntu tray area for fan information display and control. The indicator
requires this program to have setuid=root flag but run from the desktop user
, because a root user is not allowed to display a desktop indicator while a
non-root user is not allowed to control Clevo EC (Embedded Controller that's
responsible of the fan). Fix permissions of this executable if it fails to
run:
    sudo chown root clevo-indicator-rs
    sudo chmod u+s  clevo-indicator-rs

Note any fan duty change should take 1-2 seconds to come into effect - you
can verify by the fan speed displayed on indicator icon and also louder fan
noise.

In the indicator mode, this program would always attempt to load kernel
module 'ec_sys', in order to query EC information from
'/sys/kernel/debug/ec/ec0/io' instead of polling EC ports for readings,
which may be more risky if interrupted or concurrently operated during the
process.

DO NOT MANIPULATE OR QUERY EC I/O PORTS WHILE THIS PROGRAM IS RUNNING.
";

fn main() -> ExitCode {
    println!("Simple fan control utility for Clevo laptops");

    if check_proc_instances(PROC_NAME) > 1 {
        println!("Multiple running instances!");
        if display_available() {
            ui::show_multiple_instances_dialog();
        }
        return ExitCode::FAILURE;
    }

    if let Err(err) = ec::init() {
        println!("unable to control EC: {err}");
        return ExitCode::FAILURE;
    }

    let args: Vec<String> = env::args().collect();

    if args.len() <= 1 {
        if display_available() {
            run_gui()
        } else {
            dump_fan()
        }
    } else if args[1].starts_with('-') {
        print!("\n{HELP}");
        dump_fan()
    } else {
        match args[1].parse::<i32>() {
            Ok(value) if (0..=100).contains(&value) => test_fan(value),
            Ok(value) => {
                println!("invalid fan duty {value}!");
                ExitCode::FAILURE
            }
            Err(_) => {
                println!("invalid fan duty {}!", args[1]);
                ExitCode::FAILURE
            }
        }
    }
}

/// Starts the UI process and the EC worker, then waits for the worker.
fn run_gui() -> ExitCode {
    // SAFETY: plain libc call.
    let pid = unsafe { libc::getpid() };
    PARENT_PID.store(pid, Ordering::SeqCst);

    shared::init();

    install_signal(
        libc::SIGCHLD,
        main_on_sigchld as *const () as libc::sighandler_t,
    );
    signal_term(main_on_sigterm as *const () as libc::sighandler_t);

    // SAFETY: no threads are running at this point, so `fork` is safe.
    let worker_pid = unsafe { libc::fork() };

    if worker_pid == 0 {
        // Child: pure EC worker. It must not inherit the parent's SIGCHLD
        // handler, and a termination signal only asks it to stop at the next
        // loop iteration (so no EC command is cut in half).
        install_signal(libc::SIGCHLD, libc::SIG_DFL);
        signal_term(ec_on_sigterm as *const () as libc::sighandler_t);
        worker::run();
        return ExitCode::SUCCESS;
    }

    if worker_pid > 0 {
        ui::run();
        shared::get().set_exit(1);
        // SAFETY: `worker_pid` is a live child of this process.
        unsafe {
            libc::waitpid(worker_pid, std::ptr::null_mut(), 0);
        }
        return ExitCode::SUCCESS;
    }

    println!("unable to create worker: {}", io::Error::last_os_error());
    ExitCode::FAILURE
}

fn dump_fan() -> ExitCode {
    println!("Dump fan information");
    println!("  FAN Duty: {}%", ec::query_fan_duty(1));
    println!("  FA2 Duty: {}%", ec::query_fan_duty(2));
    println!("  FAN RPMs: {} RPM", ec::query_fan_rpms());
    println!("  FA2 RPMs: {} RPM", ec::query_fa2_rpms());
    println!("  CPU Temp: {}°C", ec::query_cpu_temp());
    println!("  GPU Temp: {}°C", ec::query_gpu_temp());
    ExitCode::SUCCESS
}

fn test_fan(duty_percentage: i32) -> ExitCode {
    println!("Change fans duty to {duty_percentage}%");
    ec::write_fan_duty(duty_percentage, 1);
    ec::write_fan_duty(duty_percentage, 2);
    println!();
    dump_fan()
}

fn display_available() -> bool {
    env::var("DISPLAY")
        .map(|display| !display.is_empty())
        .unwrap_or(false)
}

pub fn parent_pid() -> libc::pid_t {
    PARENT_PID.load(Ordering::SeqCst)
}

fn install_signal(signal: libc::c_int, handler: libc::sighandler_t) {
    // SAFETY: `libc::signal` installs a process-wide handler.
    unsafe {
        libc::signal(signal, handler);
    }
}

fn signal_term(handler: libc::sighandler_t) {
    install_signal(libc::SIGHUP, handler);
    install_signal(libc::SIGINT, handler);
    install_signal(libc::SIGQUIT, handler);
    install_signal(libc::SIGPIPE, handler);
    install_signal(libc::SIGALRM, handler);
    install_signal(libc::SIGTERM, handler);
    install_signal(libc::SIGUSR1, handler);
    install_signal(libc::SIGUSR2, handler);
}

/// Terminates the UI process when the worker exits.
extern "C" fn main_on_sigchld(_signal: libc::c_int) {
    // SAFETY: `_exit` is async-signal-safe.
    unsafe { libc::_exit(0) };
}

/// UI termination: tell the worker to stop, then exit immediately.
extern "C" fn main_on_sigterm(_signal: libc::c_int) {
    if let Some(info) = shared::try_get() {
        info.set_exit(1);
    }
    // SAFETY: `_exit` is async-signal-safe.
    unsafe { libc::_exit(0) };
}

/// Worker termination: only set the flag so an in-flight EC command can
/// finish. The main loop notices it within one poll interval.
extern "C" fn ec_on_sigterm(_signal: libc::c_int) {
    if let Some(info) = shared::try_get() {
        info.set_exit(1);
    }
}

/// Counts other running processes with the same `comm` name.
///
/// Returns -1 when `/proc` cannot be read, matching the C implementation.
fn check_proc_instances(proc_name: &str) -> i32 {
    let this_pid = process::id();
    let dir = match fs::read_dir("/proc") {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("can't open /proc: {err}");
            return -1;
        }
    };

    let mut instance_count = 0;

    for entry in dir.flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == this_pid {
            continue;
        }

        let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };

        let is_same_name = comm.len() >= proc_name.len()
            && comm.starts_with(proc_name)
            && comm
                .as_bytes()
                .get(proc_name.len())
                .is_none_or(|&byte| byte == b'\n' || byte == 0);

        if is_same_name {
            eprintln!("Process: {pid}");
            instance_count += 1;
        }
    }

    instance_count
}
