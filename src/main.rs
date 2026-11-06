//! Clevo fan control indicator.
//!
//! Rust port of `clevo-indicator.c` by AqD <iiiaqd@gmail.com>, based on the
//! fan control code by Jonas Diemer.
//!
//! The program keeps the original two-process design:
//!
//! * the parent process runs as the desktop user and owns the GTK3 /
//!   libappindicator UI;
//! * the forked child keeps effective root and talks to the Clevo EC;
//! * both processes exchange state through a shared anonymous mapping.
//!
//! The executable must be installed setuid-root and started by the desktop
//! user, just like the C version.
//!
//! `unsafe` is confined to three small modules:
//!
//! * [`ec`] — `ioperm(2)` and the `in`/`out` instructions;
//! * [`shared`] — the shared `mmap` and the cast to atomics;
//! * [`process`] — `fork(2)`, which is inherently unsafe in Rust.
//!
//! Everything else (menu logic, error handling, signal plumbing, process
//! supervision, CLI) is safe Rust.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod ec;
mod process;
mod shared;
mod signals;
mod ui;
mod worker;

use std::env;
use std::fs;
use std::io;
use std::process::{id, ExitCode};

use nix::unistd::Pid;
use thiserror::Error;

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

/// Top-level error type for the binary.
#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Ec(#[from] ec::EcError),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Nix(#[from] nix::Error),

    #[error("GTK initialization failed")]
    Gtk,
}

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
    init_logging();
    println!("Simple fan control utility for Clevo laptops");

    match run() {
        Ok(code) => code,
        Err(err) => {
            log::error!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}

fn run() -> Result<ExitCode, AppError> {
    // `check_proc_instances` skips our own PID, so any positive count means
    // another indicator (C or Rust) is already running. Both builds share the
    // truncated 15-byte `comm` name, hence they exclude each other.
    if check_proc_instances(PROC_NAME) > 0 {
        println!("Multiple running instances!");
        if display_available() {
            if let Err(err) = ui::show_multiple_instances_dialog() {
                log::warn!("unable to show the multiple-instances dialog: {err}");
            }
        }
        return Ok(ExitCode::FAILURE);
    }

    let args: Vec<String> = env::args().collect();

    if args.len() <= 1 {
        if display_available() {
            run_gui()
        } else {
            Ok(dump_fan(&ec::Ec::open()?)?)
        }
    } else if args[1].starts_with('-') {
        print!("\n{HELP}");
        Ok(dump_fan(&ec::Ec::open()?)?)
    } else {
        match args[1].parse::<i32>() {
            Ok(value) if (0..=100).contains(&value) => Ok(test_fan(&ec::Ec::open()?, value)?),
            Ok(value) => {
                println!("invalid fan duty {value}!");
                Ok(ExitCode::FAILURE)
            }
            Err(_) => {
                println!("invalid fan duty {}!", args[1]);
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

/// Starts the UI process and the EC worker, then waits for the worker.
fn run_gui() -> Result<ExitCode, AppError> {
    let parent_pid = Pid::this();
    let shared = shared::Shared::create()?;

    match process::fork_worker()? {
        process::Forked::Child => {
            // Child: pure EC worker. It opens the EC itself, after the fork,
            // so the UI process never holds I/O-port permissions. Termination
            // signals only set a flag, so an in-flight EC command is never
            // cut in half.
            worker::run(shared, parent_pid)?;
            Ok(ExitCode::SUCCESS)
        }
        process::Forked::Parent(worker_pid) => {
            ui::run(shared, worker_pid)?;
            shared.set_exit(1);

            match process::wait(worker_pid) {
                Ok(status) => log::debug!("EC worker status: {status:?}"),
                Err(nix::errno::Errno::ECHILD) => {
                    // Already reaped by the UI worker-watch timer.
                }
                Err(err) => return Err(err.into()),
            }

            Ok(ExitCode::SUCCESS)
        }
    }
}

fn dump_fan(ec: &ec::Ec) -> Result<ExitCode, ec::EcError> {
    println!("Dump fan information");
    println!("  FAN Duty: {}%", ec.fan_duty(1)?);
    println!("  FA2 Duty: {}%", ec.fan_duty(2)?);
    println!("  FAN RPMs: {} RPM", ec.fan_rpms()?);
    println!("  FA2 RPMs: {} RPM", ec.fa2_rpms()?);
    println!("  CPU Temp: {}°C", ec.cpu_temp()?);
    println!("  GPU Temp: {}°C", ec.gpu_temp()?);
    Ok(ExitCode::SUCCESS)
}

fn test_fan(ec: &ec::Ec, duty_percentage: i32) -> Result<ExitCode, AppError> {
    println!("Change fans duty to {duty_percentage}%");
    ec.write_fan_duty(duty_percentage, 1)?;
    ec.write_fan_duty(duty_percentage, 2)?;
    println!();
    Ok(dump_fan(ec)?)
}

fn display_available() -> bool {
    env::var("DISPLAY")
        .map(|display| !display.is_empty())
        .unwrap_or(false)
}

/// Counts other running processes with the same `comm` name.
///
/// Returns -1 when `/proc` cannot be read, matching the C implementation.
fn check_proc_instances(proc_name: &str) -> i32 {
    let this_pid = id();
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

        if comm_matches(&comm, proc_name) {
            eprintln!("Process: {pid}");
            instance_count += 1;
        }
    }

    instance_count
}

/// Checks whether a `/proc/<pid>/comm` line names exactly `proc_name`.
///
/// The kernel truncates `comm` to 15 bytes, so the Rust binary
/// (`clevo-indicator-rs`) appears as `clevo-indicator`, exactly like the C
/// version. A longer name that merely starts with `proc_name` must not match.
fn comm_matches(comm: &str, proc_name: &str) -> bool {
    comm.len() >= proc_name.len()
        && comm.starts_with(proc_name)
        && comm
            .as_bytes()
            .get(proc_name.len())
            .is_none_or(|&byte| byte == b'\n' || byte == 0)
}

#[cfg(test)]
mod tests {
    use super::comm_matches;

    const NAME: &str = "clevo-indicator";

    #[test]
    fn matches_kernel_comm_line() {
        assert!(comm_matches("clevo-indicator\n", NAME));
        assert!(comm_matches("clevo-indicator\0", NAME));
        assert!(comm_matches("clevo-indicator", NAME));
    }

    #[test]
    fn rejects_longer_names() {
        assert!(!comm_matches("clevo-indicator-rs\n", NAME));
        assert!(!comm_matches("clevo-indicator2\n", NAME));
        assert!(!comm_matches("clevo\n", NAME));
        assert!(!comm_matches("", NAME));
    }
}
