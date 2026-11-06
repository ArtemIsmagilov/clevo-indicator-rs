//! Clevo EC (Embedded Controller) low-level access.
//!
//! All port I/O and the `ioperm(2)` call are confined to this module. The
//! public API is safe: creating an [`Ec`] proves that the process has been
//! granted access to the command/status and data ports, after which the safe
//! methods can be used freely.

use std::io;
use std::thread::sleep;
use std::time::Duration;

use thiserror::Error;

pub const EC_SC: u16 = 0x66;
pub const EC_DATA: u16 = 0x62;

const IBF: u32 = 1;
const OBF: u32 = 0;
const EC_SC_READ_CMD: u8 = 0x80;
const WAIT_RETRIES: u32 = 100;
const WAIT_STEP: Duration = Duration::from_millis(1);

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

/// Errors returned while talking to the EC.
#[derive(Debug, Error)]
pub enum EcError {
    /// `ioperm(2)` failed or the EC did not answer in time.
    #[error("unable to control EC: {0}")]
    Io(#[from] io::Error),

    /// The EC status register did not reach the expected state.
    #[error("EC timeout on port {port:#x} (data={data:#x}, flag={flag:#x}, value={value:#x})")]
    Timeout {
        port: u16,
        data: u8,
        flag: u32,
        value: u8,
    },

    /// A duty outside the allowed range was requested.
    #[error("invalid fan duty {0}, expected 0..=100")]
    InvalidDuty(i32),
}

/// A handle to the Clevo Embedded Controller.
///
/// The constructor calls `ioperm(2)`; every method is safe because the struct
/// can only exist after that call succeeded.
#[derive(Debug)]
pub struct Ec {
    _private: (),
}

impl Ec {
    /// Grants access to the EC command and data ports.
    pub fn open() -> Result<Self, EcError> {
        // SAFETY: `ioperm` only changes the I/O permission bitmap of the
        // current process. It fails cleanly when the process is not
        // privileged or the architecture does not support it.
        unsafe {
            if libc::ioperm(EC_DATA as libc::c_ulong, 1, 1) != 0 {
                return Err(io::Error::last_os_error().into());
            }
            if libc::ioperm(EC_SC as libc::c_ulong, 1, 1) != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(Self { _private: () })
    }

    pub fn cpu_temp(&self) -> Result<i32, EcError> {
        Ok(self.read_reg(EC_REG_CPU_TEMP)? as i32)
    }

    pub fn gpu_temp(&self) -> Result<i32, EcError> {
        Ok(self.read_reg(EC_REG_GPU_TEMP)? as i32)
    }

    pub fn fan_duty(&self, fan_number: i32) -> Result<i32, EcError> {
        let reg = if fan_number == 2 {
            EC_REG_FA2_DUTY
        } else {
            EC_REG_FAN_DUTY
        };
        Ok(calculate_fan_duty(self.read_reg(reg)? as i32))
    }

    pub fn fan_rpms(&self) -> Result<i32, EcError> {
        let hi = self.read_reg(EC_REG_FAN_RPMS_HI)? as i32;
        let lo = self.read_reg(EC_REG_FAN_RPMS_LO)? as i32;
        Ok(calculate_fan_rpms(hi, lo))
    }

    pub fn fa2_rpms(&self) -> Result<i32, EcError> {
        let hi = self.read_reg(EC_REG_FA2_RPMS_HI)? as i32;
        let lo = self.read_reg(EC_REG_FA2_RPMS_LO)? as i32;
        Ok(calculate_fan_rpms(hi, lo))
    }

    /// Writes the target duty (0..=100 %) for fan 1 or 2.
    pub fn write_fan_duty(&self, duty_percentage: i32, fan_number: i32) -> Result<(), EcError> {
        if !(0..=100).contains(&duty_percentage) {
            return Err(EcError::InvalidDuty(duty_percentage));
        }
        let value = (duty_percentage as f64 / 100.0 * 255.0) as u8;
        self.io_do(0x99, fan_number as u16, value)
    }

    fn read_reg(&self, reg: usize) -> Result<u8, EcError> {
        self.io_read(reg as u16)
    }

    fn io_wait(&self, flag: u32, value: u8) -> Result<(), EcError> {
        let mut attempt = 0;

        // SAFETY: `Ec` can only be constructed after a successful `ioperm`,
        // so reading the status port is permitted.
        unsafe {
            let mut data = inb(EC_SC);

            while ((data >> flag) & 0x1) != value {
                if attempt >= WAIT_RETRIES {
                    return Err(EcError::Timeout {
                        port: EC_SC,
                        data,
                        flag,
                        value,
                    });
                }
                sleep(WAIT_STEP);
                data = inb(EC_SC);
                attempt += 1;
            }
        }

        Ok(())
    }

    fn io_read(&self, port: u16) -> Result<u8, EcError> {
        self.io_wait(IBF, 0)?;

        // SAFETY: `Ec` guarantees that both ports were granted with
        // `ioperm(2)`; the command sequence follows the EC protocol.
        unsafe {
            outb(EC_SC_READ_CMD, EC_SC);

            self.io_wait(IBF, 0)?;
            outb(port as u8, EC_DATA);

            self.io_wait(OBF, 1)?;
            Ok(inb(EC_DATA))
        }
    }

    fn io_do(&self, cmd: u8, port: u16, value: u8) -> Result<(), EcError> {
        self.io_wait(IBF, 0)?;

        // SAFETY: `Ec` guarantees that both ports were granted with
        // `ioperm(2)`; the command sequence follows the EC protocol.
        unsafe {
            outb(cmd, EC_SC);

            self.io_wait(IBF, 0)?;
            outb(port as u8, EC_DATA);

            self.io_wait(IBF, 0)?;
            outb(value, EC_DATA);
        }

        self.io_wait(IBF, 0)
    }
}

/// Reads a byte from an I/O port (`inb` from `sys/io.h`).
///
/// # Safety
///
/// The port must have been granted with `ioperm(2)` in this process.
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller guarantees that `port` was granted with `ioperm(2)`.
    unsafe {
        std::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

/// Writes a byte to an I/O port (`outb` from `sys/io.h`).
///
/// # Safety
///
/// The port must have been granted with `ioperm(2)` in this process.
#[inline]
unsafe fn outb(value: u8, port: u16) {
    // SAFETY: the caller guarantees that `port` was granted with `ioperm(2)`.
    unsafe {
        std::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
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
/// Returns the next duty in percent, or `0` when no change is needed. The
/// `0` sentinel is part of the original protocol between the curve and the
/// worker loop.
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
