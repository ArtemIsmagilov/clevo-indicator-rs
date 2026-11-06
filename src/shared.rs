//! Shared state between the UI process and the EC worker process.
//!
//! The C version uses a single `mmap(MAP_SHARED | MAP_ANONYMOUS)` block and
//! `volatile` fields. Here the fields are atomics in the same kind of
//! anonymously shared mapping, which is inherited across `fork(2)`.
//!
//! All `unsafe` code is confined to [`Shared::create`]. The resulting
//! [`Shared`] handle is `Copy`, safe to send between threads and valid until
//! the process exits.

use std::sync::atomic::{AtomicI32, Ordering};

const SHARED_SIZE: usize = 4096;

#[repr(C)]
struct SharedInfo {
    exit: AtomicI32,
    cpu_temp: AtomicI32,
    gpu_temp: AtomicI32,
    fan_duty: AtomicI32,
    fan_rpms: AtomicI32,
    auto_duty: AtomicI32,
    auto_duty_val: AtomicI32,
    manual_next_fan_duty: AtomicI32,
    manual_prev_fan_duty: AtomicI32,
}

/// A handle to the shared memory block.
///
/// Must be created in the parent process before `fork(2)`. The child inherits
/// both the mapping and this handle, so both processes observe the same
/// atomics.
#[derive(Clone, Copy)]
pub struct Shared(&'static SharedInfo);

impl Shared {
    /// Allocates and initializes the shared memory block.
    pub fn create() -> std::io::Result<Self> {
        // SAFETY: an anonymous shared mapping of page-aligned memory is
        // created. `SharedInfo` contains only atomics, which are valid for any
        // bit pattern, and the mapping is never unmapped.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SHARED_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_SHARED,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }

        let info_ptr = ptr as *mut SharedInfo;

        // SAFETY: `mmap` returned a suitably aligned, exclusively owned block
        // of at least `size_of::<SharedInfo>()` bytes. Writing a fully
        // initialized value into it is valid.
        unsafe {
            info_ptr.write(SharedInfo {
                exit: AtomicI32::new(0),
                cpu_temp: AtomicI32::new(0),
                gpu_temp: AtomicI32::new(0),
                fan_duty: AtomicI32::new(0),
                fan_rpms: AtomicI32::new(0),
                auto_duty: AtomicI32::new(1),
                auto_duty_val: AtomicI32::new(0),
                manual_next_fan_duty: AtomicI32::new(0),
                manual_prev_fan_duty: AtomicI32::new(0),
            });
        }

        // SAFETY: the pointer was just initialized and remains valid for the
        // rest of the process lifetime.
        Ok(Self(unsafe { &*info_ptr }))
    }

    pub fn exit(&self) -> i32 {
        self.0.exit.load(Ordering::SeqCst)
    }

    pub fn set_exit(&self, value: i32) {
        self.0.exit.store(value, Ordering::SeqCst);
    }

    pub fn cpu_temp(&self) -> i32 {
        self.0.cpu_temp.load(Ordering::SeqCst)
    }

    pub fn set_cpu_temp(&self, value: i32) {
        self.0.cpu_temp.store(value, Ordering::SeqCst);
    }

    pub fn gpu_temp(&self) -> i32 {
        self.0.gpu_temp.load(Ordering::SeqCst)
    }

    pub fn set_gpu_temp(&self, value: i32) {
        self.0.gpu_temp.store(value, Ordering::SeqCst);
    }

    pub fn fan_duty(&self) -> i32 {
        self.0.fan_duty.load(Ordering::SeqCst)
    }

    pub fn set_fan_duty(&self, value: i32) {
        self.0.fan_duty.store(value, Ordering::SeqCst);
    }

    pub fn fan_rpms(&self) -> i32 {
        self.0.fan_rpms.load(Ordering::SeqCst)
    }

    pub fn set_fan_rpms(&self, value: i32) {
        self.0.fan_rpms.store(value, Ordering::SeqCst);
    }

    pub fn auto_duty(&self) -> i32 {
        self.0.auto_duty.load(Ordering::SeqCst)
    }

    pub fn set_auto_duty(&self, value: i32) {
        self.0.auto_duty.store(value, Ordering::SeqCst);
    }

    pub fn auto_duty_val(&self) -> i32 {
        self.0.auto_duty_val.load(Ordering::SeqCst)
    }

    pub fn set_auto_duty_val(&self, value: i32) {
        self.0.auto_duty_val.store(value, Ordering::SeqCst);
    }

    pub fn manual_next_fan_duty(&self) -> i32 {
        self.0.manual_next_fan_duty.load(Ordering::SeqCst)
    }

    pub fn set_manual_next_fan_duty(&self, value: i32) {
        self.0.manual_next_fan_duty.store(value, Ordering::SeqCst);
    }

    pub fn manual_prev_fan_duty(&self) -> i32 {
        self.0.manual_prev_fan_duty.load(Ordering::SeqCst)
    }

    pub fn set_manual_prev_fan_duty(&self, value: i32) {
        self.0.manual_prev_fan_duty.store(value, Ordering::SeqCst);
    }
}
