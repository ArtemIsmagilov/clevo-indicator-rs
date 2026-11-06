//! Shared state between the UI process and the EC worker process.
//!
//! The C version uses a single `mmap(MAP_SHARED | MAP_ANONYMOUS)` block and
//! `volatile` fields. Here the fields are atomics placed in the same kind of
//! anonymously shared mapping, which is inherited across `fork(2)`.

use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

const SHARED_SIZE: usize = 4096;

#[repr(C)]
pub struct SharedInfo {
    pub exit: AtomicI32,
    pub cpu_temp: AtomicI32,
    pub gpu_temp: AtomicI32,
    pub fan_duty: AtomicI32,
    pub fan_rpms: AtomicI32,
    pub auto_duty: AtomicI32,
    pub auto_duty_val: AtomicI32,
    pub manual_next_fan_duty: AtomicI32,
    pub manual_prev_fan_duty: AtomicI32,
}

static SHARE: AtomicPtr<SharedInfo> = AtomicPtr::new(std::ptr::null_mut());

/// Allocates and initializes the shared memory block.
///
/// Must be called once, in the parent process, before `fork(2)`.
pub fn init() {
    // SAFETY: an anonymous shared mapping of a plain data structure is used.
    unsafe {
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            SHARED_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANONYMOUS | libc::MAP_SHARED,
            -1,
            0,
        );
        if ptr == libc::MAP_FAILED {
            panic!(
                "unable to allocate shared memory: {}",
                std::io::Error::last_os_error()
            );
        }

        let info = &*(ptr as *const SharedInfo);
        SHARE.store(ptr as *mut SharedInfo, Ordering::SeqCst);

        info.exit.store(0, Ordering::SeqCst);
        info.cpu_temp.store(0, Ordering::SeqCst);
        info.gpu_temp.store(0, Ordering::SeqCst);
        info.fan_duty.store(0, Ordering::SeqCst);
        info.fan_rpms.store(0, Ordering::SeqCst);
        info.auto_duty.store(1, Ordering::SeqCst);
        info.auto_duty_val.store(0, Ordering::SeqCst);
        info.manual_next_fan_duty.store(0, Ordering::SeqCst);
        info.manual_prev_fan_duty.store(0, Ordering::SeqCst);
    }
}

/// Returns the shared state, panicking if [`init`] has not run.
///
/// The returned reference is valid for the rest of the process lifetime
/// because the mapping is never unmapped.
pub fn get() -> &'static SharedInfo {
    try_get().expect("shared memory is not initialized")
}

/// Returns the shared state if [`init`] has run.
///
/// This is used from signal handlers, where panicking is not acceptable.
pub fn try_get() -> Option<&'static SharedInfo> {
    let ptr = SHARE.load(Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: the pointer is set by `init` and points to a live mapping.
        Some(unsafe { &*ptr })
    }
}

impl SharedInfo {
    pub fn exit(&self) -> i32 {
        self.exit.load(Ordering::SeqCst)
    }

    pub fn set_exit(&self, value: i32) {
        self.exit.store(value, Ordering::SeqCst);
    }

    pub fn cpu_temp(&self) -> i32 {
        self.cpu_temp.load(Ordering::SeqCst)
    }

    pub fn set_cpu_temp(&self, value: i32) {
        self.cpu_temp.store(value, Ordering::SeqCst);
    }

    pub fn gpu_temp(&self) -> i32 {
        self.gpu_temp.load(Ordering::SeqCst)
    }

    pub fn set_gpu_temp(&self, value: i32) {
        self.gpu_temp.store(value, Ordering::SeqCst);
    }

    pub fn fan_duty(&self) -> i32 {
        self.fan_duty.load(Ordering::SeqCst)
    }

    pub fn set_fan_duty(&self, value: i32) {
        self.fan_duty.store(value, Ordering::SeqCst);
    }

    pub fn fan_rpms(&self) -> i32 {
        self.fan_rpms.load(Ordering::SeqCst)
    }

    pub fn set_fan_rpms(&self, value: i32) {
        self.fan_rpms.store(value, Ordering::SeqCst);
    }

    pub fn auto_duty(&self) -> i32 {
        self.auto_duty.load(Ordering::SeqCst)
    }

    pub fn set_auto_duty(&self, value: i32) {
        self.auto_duty.store(value, Ordering::SeqCst);
    }

    pub fn auto_duty_val(&self) -> i32 {
        self.auto_duty_val.load(Ordering::SeqCst)
    }

    pub fn set_auto_duty_val(&self, value: i32) {
        self.auto_duty_val.store(value, Ordering::SeqCst);
    }

    pub fn manual_next_fan_duty(&self) -> i32 {
        self.manual_next_fan_duty.load(Ordering::SeqCst)
    }

    pub fn set_manual_next_fan_duty(&self, value: i32) {
        self.manual_next_fan_duty.store(value, Ordering::SeqCst);
    }

    pub fn manual_prev_fan_duty(&self) -> i32 {
        self.manual_prev_fan_duty.load(Ordering::SeqCst)
    }

    pub fn set_manual_prev_fan_duty(&self, value: i32) {
        self.manual_prev_fan_duty.store(value, Ordering::SeqCst);
    }
}
