//! Clevo EC (Embedded Controller) low-level access.
//!
//! The port I/O code mirrors the original C implementation: the EC is
//! accessed through command/status port `0x66` and data port `0x62`.
//! Access must be granted with `ioperm(2)` first, which requires root.

use std::io;

pub const EC_SC: u16 = 0x66;
pub const EC_DATA: u16 = 0x62;

const IBF: u32 = 1;
const OBF: u32 = 0;
const EC_SC_READ_CMD: u8 = 0x80;

pub const EC_REG_SIZE: usize = 0x100;
pub const EC_REG_CPU_TEMP: usize = 0x07;
pub const EC_REG_GPU_TEMP: usize = 0xCD;
pub const EC_REG_FAN_DUTY: usize = 0xCE;
pub const EC_REG_FA2_DUTY: usize = 0xCF;
pub const EC_REG_FAN_RPMS_HI: usize = 0xD0;
pub const EC_REG_FAN_RPMS_LO: usize = 0xD1;
pub const EC_REG_FA2_RPMS_HI: usize = 0xD2;
pub const EC_REG_FA2_RPMS_LO: usize = 0xD3;

pub const MAX_FAN_RPM: f64 = 4400.0;

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
compile_error!("Clevo EC access requires an x86 or x86_64 target");

/// Reads a byte from an I/O port (`inb` from `sys/io.h`).
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    std::arch::asm!(
        "in al, dx",
        in("dx") port,
        out("al") value,
        options(nomem, nostack, preserves_flags),
    );
    value
}

/// Writes a byte to an I/O port (`outb` from `sys/io.h`).
#[inline]
unsafe fn outb(value: u8, port: u16) {
    std::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags),
    );
}

/// Grants access to the EC command and data ports.
pub fn init() -> io::Result<()> {
    // SAFETY: `ioperm` only changes the I/O permission bitmap of the
    // current process. It is safe to call; failure is reported as an error.
    unsafe {
        if libc::ioperm(EC_DATA as libc::c_ulong, 1, 1) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::ioperm(EC_SC as libc::c_ulong, 1, 1) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn io_wait(port: u16, flag: u32, value: u8) -> bool {
    // SAFETY: the ports are expected to have been granted by `init`.
    let mut data = unsafe { inb(port) };
    let mut i = 0;
    while ((data >> flag) & 0x1) != value && i < 100 {
        // SAFETY: plain libc call.
        unsafe { libc::usleep(1000) };
        // SAFETY: see above.
        data = unsafe { inb(port) };
        i += 1;
    }
    if i >= 100 {
        println!(
            "wait_ec error on port 0x{port:x}, data=0x{data:x}, flag=0x{flag:x}, value=0x{value:x}"
        );
        return false;
    }
    true
}

fn io_read(port: u16) -> u8 {
    io_wait(EC_SC, IBF, 0);
    // SAFETY: the ports are expected to have been granted by `init`.
    unsafe { outb(EC_SC_READ_CMD, EC_SC) };

    io_wait(EC_SC, IBF, 0);
    // SAFETY: see above.
    unsafe { outb(port as u8, EC_DATA) };

    io_wait(EC_SC, OBF, 1);
    // SAFETY: see above.
    unsafe { inb(EC_DATA) }
}

fn io_do(cmd: u8, port: u16, value: u8) -> bool {
    io_wait(EC_SC, IBF, 0);
    // SAFETY: the ports are expected to have been granted by `init`.
    unsafe { outb(cmd, EC_SC) };

    io_wait(EC_SC, IBF, 0);
    // SAFETY: see above.
    unsafe { outb(port as u8, EC_DATA) };

    io_wait(EC_SC, IBF, 0);
    // SAFETY: see above.
    unsafe { outb(value, EC_DATA) };

    io_wait(EC_SC, IBF, 0)
}

pub fn query_cpu_temp() -> i32 {
    io_read(EC_REG_CPU_TEMP as u16) as i32
}

pub fn query_gpu_temp() -> i32 {
    io_read(EC_REG_GPU_TEMP as u16) as i32
}

pub fn query_fan_duty(fan_number: i32) -> i32 {
    let reg = if fan_number == 2 {
        EC_REG_FA2_DUTY
    } else {
        EC_REG_FAN_DUTY
    };
    calculate_fan_duty(io_read(reg as u16) as i32)
}

pub fn query_fan_rpms() -> i32 {
    let hi = io_read(EC_REG_FAN_RPMS_HI as u16) as i32;
    let lo = io_read(EC_REG_FAN_RPMS_LO as u16) as i32;
    calculate_fan_rpms(hi, lo)
}

pub fn query_fa2_rpms() -> i32 {
    let hi = io_read(EC_REG_FA2_RPMS_HI as u16) as i32;
    let lo = io_read(EC_REG_FA2_RPMS_LO as u16) as i32;
    calculate_fan_rpms(hi, lo)
}

/// Writes the target duty (0..=100 %) for fan 1 or 2.
///
/// Returns 0 on success and 1 on failure, like the C version's `EXIT_*`
/// values.
pub fn write_fan_duty(duty_percentage: i32, fan_number: i32) -> i32 {
    if !(0..=100).contains(&duty_percentage) {
        println!("Wrong fan duty to write: {duty_percentage}");
        return 1;
    }
    let value = (duty_percentage as f64 / 100.0 * 255.0) as u8;
    io_do(0x99, fan_number as u16, value) as i32
}

pub fn calculate_fan_duty(raw_duty: i32) -> i32 {
    (raw_duty as f64 / 255.0 * 100.0) as i32
}

pub fn calculate_fan_rpms(raw_rpm_high: i32, raw_rpm_low: i32) -> i32 {
    let raw_rpm = (raw_rpm_high << 8) + raw_rpm_low;
    if raw_rpm > 0 {
        2_156_220 / raw_rpm
    } else {
        0
    }
}

/// The aggressive auto fan curve from the original program.
///
/// It keeps temperatures below roughly 60 °C even with Intel Turbo Boost
/// enabled, and is intentionally more eager than the built-in EC algorithm.
pub fn auto_duty_adjust(cpu_temp: i32, gpu_temp: i32, duty: i32) -> i32 {
    let temp = cpu_temp.max(gpu_temp);

    if temp >= 80 && duty < 50 {
        return 75;
    }
    if temp >= 70 && duty < 50 {
        return 50;
    }
    if temp >= 60 && duty < 30 {
        return 30;
    }
    if temp >= 55 && duty < 25 {
        return 25;
    }
    if temp >= 40 && duty < 20 {
        return 20;
    }
    if temp >= 30 && duty < 10 {
        return 10;
    }

    if temp <= 15 && duty > 0 {
        return 0;
    }
    if temp <= 25 && duty > 10 {
        return 10;
    }
    if temp <= 35 && duty > 15 {
        return 15;
    }
    if temp <= 45 && duty > 20 {
        return 20;
    }
    if temp <= 55 && duty > 25 {
        return 25;
    }
    if temp <= 65 && duty > 30 {
        return 30;
    }
    if temp <= 75 && duty > 50 {
        return 50;
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_duty_conversion() {
        assert_eq!(calculate_fan_duty(0), 0);
        assert_eq!(calculate_fan_duty(255), 100);
        assert_eq!(calculate_fan_duty(128), 50);
    }

    #[test]
    fn fan_rpm_conversion() {
        assert_eq!(calculate_fan_rpms(0, 0), 0);
        assert_eq!(calculate_fan_rpms(0x03, 0xE8), 2_156_220 / 1000);
    }

    #[test]
    fn auto_curve_steps_up() {
        assert_eq!(auto_duty_adjust(85, 50, 0), 75);
        assert_eq!(auto_duty_adjust(70, 50, 30), 50);
        assert_eq!(auto_duty_adjust(60, 50, 20), 30);
        assert_eq!(auto_duty_adjust(30, 20, 0), 10);
    }

    #[test]
    fn auto_curve_steps_down() {
        assert_eq!(auto_duty_adjust(30, 30, 30), 15);
        assert_eq!(auto_duty_adjust(20, 20, 30), 10);
        assert_eq!(auto_duty_adjust(10, 10, 30), 0);
        assert_eq!(auto_duty_adjust(85, 50, 75), 0);
    }
}
