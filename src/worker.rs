//! The EC worker process.
//!
//! This process runs with `uid == 0` and owns all EC access. It polls the
//! shared state written by the UI process, writes manual/automatic fan duties
//! and publishes fresh sensor readings.

use std::fs::File;
use std::io::Read;
use std::process::Command;

use crate::ec;
use crate::shared;

/// Poll interval, matching the original 200 ms.
const POLL_INTERVAL_US: libc::c_uint = 200 * 1000;

const EC_SYS_PATH: &str = "/sys/kernel/debug/ec/ec0/io";

pub fn run() {
    // The worker was started via a setuid-root binary; make sure it really
    // has root privileges before touching the EC.
    // SAFETY: plain libc call.
    unsafe { libc::setuid(0) };

    // Loading `ec_sys` is best-effort, exactly like the C version.
    let _ = Command::new("modprobe").arg("ec_sys").status();

    while shared::get().exit() == 0 {
        let parent_pid = crate::parent_pid();
        if parent_pid != 0 {
            // SAFETY: signal 0 only performs an existence/permission check.
            if unsafe { libc::kill(parent_pid, 0) } == -1 {
                println!("worker on parent death");
                break;
            }
        }

        let info = shared::get();

        // Apply a manual duty change, if one was requested.
        let new_fan_duty = info.manual_next_fan_duty();
        if new_fan_duty != 0 && new_fan_duty != info.manual_prev_fan_duty() {
            ec::write_fan_duty(new_fan_duty, 1);
            ec::write_fan_duty(new_fan_duty, 2);
            info.set_manual_prev_fan_duty(new_fan_duty);
        }

        // Read the whole EC register space from debugfs.
        match File::open(EC_SYS_PATH) {
            Ok(mut file) => {
                let mut buf = [0u8; ec::EC_REG_SIZE];
                match file.read(&mut buf) {
                    Ok(ec::EC_REG_SIZE) => {
                        info.set_cpu_temp(buf[ec::EC_REG_CPU_TEMP] as i32);
                        info.set_gpu_temp(buf[ec::EC_REG_GPU_TEMP] as i32);
                        info.set_fan_duty(ec::calculate_fan_duty(buf[ec::EC_REG_FAN_DUTY] as i32));
                        info.set_fan_rpms(ec::calculate_fan_rpms(
                            buf[ec::EC_REG_FAN_RPMS_HI] as i32,
                            buf[ec::EC_REG_FAN_RPMS_LO] as i32,
                        ));
                    }
                    Ok(len) => println!("wrong EC size from sysfs: {len}"),
                    Err(err) => println!("unable to read EC from sysfs: {err}"),
                }
            }
            Err(err) => {
                println!("unable to read EC from sysfs: {err}");
                std::process::exit(1);
            }
        }

        // Automatic fan curve.
        if info.auto_duty() == 1 {
            let next_duty = ec::auto_duty_adjust(info.cpu_temp(), info.gpu_temp(), info.fan_duty());
            if next_duty != 0 && next_duty != info.auto_duty_val() {
                println!(
                    "{} CPU={}°C, auto fan duty to {}%",
                    time_string(),
                    info.cpu_temp(),
                    next_duty
                );
                ec::write_fan_duty(next_duty, 1);
                ec::write_fan_duty(next_duty, 2);
                info.set_auto_duty_val(next_duty);
            }
        }

        // SAFETY: plain libc call.
        unsafe { libc::usleep(POLL_INTERVAL_US) };
    }

    println!("worker quit");
}

/// Formats the current local time as `%m/%d %H:%M:%S` without extra crates.
fn time_string() -> String {
    // SAFETY: `time`, `localtime_r` and the returned `tm` fields are plain C
    // data; all pointers are valid for the duration of the call.
    unsafe {
        let mut now: libc::time_t = 0;
        libc::time(&mut now);

        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);

        format!(
            "{:02}/{:02} {:02}:{:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}
