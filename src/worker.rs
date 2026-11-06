//! The EC worker process.
//!
//! This process runs with `uid == 0` and owns all EC access. It polls the
//! shared state written by the UI process, writes manual/automatic fan duties
//! and publishes fresh sensor readings.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use nix::unistd::Pid;

use crate::ec::{self, Ec};
use crate::process;
use crate::shared::Shared;
use crate::signals::TerminationFlag;
use crate::AppError;

/// Poll interval, matching the original 200 ms.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

const EC_SYS_PATH: &str = "/sys/kernel/debug/ec/ec0/io";

/// Absolute locations of `modprobe`.
///
/// Never rely on `PATH` here: this code runs as root and the environment is
/// inherited from the unprivileged user who started the setuid binary.
const MODPROBE_CANDIDATES: &[&str] = &[
    "/sbin/modprobe",
    "/usr/sbin/modprobe",
    "/bin/modprobe",
    "/usr/bin/modprobe",
];

pub fn run(shared: Shared, parent_pid: Pid) -> Result<(), AppError> {
    // The worker was started via a setuid-root binary; make sure it still has
    // effective root before touching the EC. The real UID stays that of the
    // desktop user, so the worker never gains more than it needs.
    process::require_root()?;

    // Ask the kernel to terminate this process if the UI dies. The periodic
    // `kill(parent, 0)` check below is only a fallback: PDEATHSIG also covers
    // PID reuse. There is a small race between fork and this call, so verify
    // that the parent is still the expected process.
    process::arm_parent_death_signal()?;
    if process::parent_pid() != parent_pid {
        log::warn!("worker: parent process is gone");
        return Ok(());
    }

    // Open the EC only in this process. The UI process deliberately never
    // calls `ioperm(2)`, so after it drops privileges it has no way to touch
    // the EC ports even if it is compromised.
    let ec = Ec::open()?;

    load_ec_sys_module();
    let terminate = TerminationFlag::install()?;

    while !terminate.is_set() && shared.exit() == 0 {
        if !process::is_alive(parent_pid) {
            log::info!("worker: parent process is gone");
            break;
        }

        apply_manual_duty(&ec, &shared);
        read_ec_sys(&shared)?;
        apply_auto_duty(&ec, &shared);

        sleep(POLL_INTERVAL);
    }

    log::info!("worker quit");
    Ok(())
}

fn load_ec_sys_module() {
    let Some(modprobe) = MODPROBE_CANDIDATES
        .iter()
        .copied()
        .find(|path| Path::new(path).is_file())
    else {
        log::warn!("modprobe not found; load the ec_sys module manually");
        return;
    };

    // `env_clear` is security-critical: the worker is root and inherited the
    // caller's environment. Without it, `PATH`, `LD_PRELOAD` or
    // `MODPROBE_OPTIONS` could make a privileged `modprobe` execute
    // user-controlled code. `output()` also keeps stdout/stderr out of
    // user-controlled file descriptors.
    match Command::new(modprobe)
        .arg("ec_sys")
        .env_clear()
        .current_dir("/")
        .output()
    {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!(
                "modprobe ec_sys exited with {}: {}",
                output.status,
                stderr.trim()
            );
        }
        Ok(_) => {}
        Err(err) => log::warn!("failed to run {modprobe} ec_sys: {err}"),
    }
}

fn apply_manual_duty(ec: &Ec, shared: &Shared) {
    let new_fan_duty = shared.manual_next_fan_duty();
    if new_fan_duty == 0 || new_fan_duty == shared.manual_prev_fan_duty() {
        return;
    }

    // Fan 1 and fan 2 are independent controllers; try both even if the first
    // one fails.
    if let Err(err) = ec.write_fan_duty(new_fan_duty, 1) {
        log::warn!("failed to set fan 1 duty to {new_fan_duty}%: {err}");
    }
    if let Err(err) = ec.write_fan_duty(new_fan_duty, 2) {
        log::warn!("failed to set fan 2 duty to {new_fan_duty}%: {err}");
    }

    shared.set_manual_prev_fan_duty(new_fan_duty);
}

fn read_ec_sys(shared: &Shared) -> Result<(), AppError> {
    let file = File::open(EC_SYS_PATH)?;

    // The ec_sys debugfs file supports positional reads, and the kernel
    // translates every byte into one EC transaction. Reading the whole
    // 256-byte region would therefore issue 256 EC accesses every poll;
    // fetch only the five registers actually used.
    let reading = (|| -> io::Result<(u8, u8, u8, u8, u8)> {
        Ok((
            read_reg(&file, ec::EC_REG_CPU_TEMP)?,
            read_reg(&file, ec::EC_REG_GPU_TEMP)?,
            read_reg(&file, ec::EC_REG_FAN_DUTY)?,
            read_reg(&file, ec::EC_REG_FAN_RPMS_HI)?,
            read_reg(&file, ec::EC_REG_FAN_RPMS_LO)?,
        ))
    })();

    match reading {
        Ok((cpu_temp, gpu_temp, fan_duty, rpm_hi, rpm_lo)) => {
            shared.set_cpu_temp(cpu_temp as i32);
            shared.set_gpu_temp(gpu_temp as i32);
            shared.set_fan_duty(ec::calculate_fan_duty(fan_duty as i32));
            shared.set_fan_rpms(ec::calculate_fan_rpms(rpm_hi as i32, rpm_lo as i32));
        }
        Err(err) => log::warn!("unable to read EC from sysfs: {err}"),
    }

    Ok(())
}

/// Reads one EC register through the `ec_sys` debugfs interface.
fn read_reg(file: &File, offset: usize) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    file.read_exact_at(&mut byte, offset as u64)?;
    Ok(byte[0])
}

fn apply_auto_duty(ec: &Ec, shared: &Shared) {
    if shared.auto_duty() != 1 {
        return;
    }

    let next_duty = ec::auto_duty_adjust(shared.cpu_temp(), shared.gpu_temp(), shared.fan_duty());
    if next_duty == 0 || next_duty == shared.auto_duty_val() {
        return;
    }

    let fan1 = ec.write_fan_duty(next_duty, 1);
    let fan2 = ec.write_fan_duty(next_duty, 2);

    if let Err(err) = &fan1 {
        log::warn!("auto: failed to set fan 1 duty to {next_duty}%: {err}");
    }
    if let Err(err) = &fan2 {
        log::warn!("auto: failed to set fan 2 duty to {next_duty}%: {err}");
    }

    if fan1.is_ok() && fan2.is_ok() {
        log::info!(
            "CPU={}°C, auto fan duty to {}%",
            shared.cpu_temp(),
            next_duty
        );
    }

    // Record the target even if a write failed, otherwise a permanently
    // missing second fan would make us retry and log on every poll.
    shared.set_auto_duty_val(next_duty);
}

#[cfg(test)]
mod tests {
    use super::read_reg;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn reads_registers_by_offset() {
        let path = std::env::temp_dir().join(format!(
            "clevo-indicator-ec-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let mut data = [0u8; 256];
        data[0x07] = 55;
        data[0xCD] = 60;
        data[0xCE] = 77;
        data[0xD0] = 0x03;
        data[0xD1] = 0xE8;

        let mut file = File::create(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        let file = File::open(&path).unwrap();
        assert_eq!(read_reg(&file, 0x07).unwrap(), 55);
        assert_eq!(read_reg(&file, 0xCD).unwrap(), 60);
        assert_eq!(read_reg(&file, 0xCE).unwrap(), 77);
        assert_eq!(read_reg(&file, 0xD0).unwrap(), 0x03);
        assert_eq!(read_reg(&file, 0xD1).unwrap(), 0xE8);
        assert!(read_reg(&file, 256).is_err());

        drop(file);
        fs::remove_file(&path).unwrap();
    }
}
