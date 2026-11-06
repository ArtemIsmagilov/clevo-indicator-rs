//! Thin, safe wrappers around the few process/OS operations the program
//! needs. Everything `unsafe` (`fork`) is encapsulated here.

use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, setuid, ForkResult, Pid};

/// Result of forking the EC worker.
pub enum Forked {
    Parent(Pid),
    Child,
}

/// Forks the EC worker process.
///
/// # Panics / safety
///
/// Must be called before GTK, logging threads or any other threads are
/// started. This is guaranteed by the current call order: the program forks
/// immediately after parsing arguments, before `gtk::init`.
pub fn fork_worker() -> nix::Result<Forked> {
    // SAFETY: the caller guarantees that no other threads exist yet, which is
    // exactly the condition `fork(2)` requires in Rust.
    match unsafe { fork() }? {
        ForkResult::Parent { child } => Ok(Forked::Parent(child)),
        ForkResult::Child => Ok(Forked::Child),
    }
}

/// Switches to the real user (the desktop user that started the setuid
/// binary).
pub fn drop_privileges() -> nix::Result<()> {
    setuid(nix::unistd::getuid())
}

/// Verifies that the worker still has effective root (inherited from the
/// setuid binary).
///
/// Unlike `setuid(0)`, this does not make the real UID root as well: the
/// worker keeps the invoking user as its real UID and only uses effective
/// root for EC access.
pub fn require_root() -> nix::Result<()> {
    if nix::unistd::geteuid().is_root() {
        Ok(())
    } else {
        Err(nix::errno::Errno::EPERM)
    }
}

/// Asks the kernel to send `SIGTERM` to the worker when its parent dies.
///
/// This cannot be fooled by PID reuse, unlike a periodic `kill(pid, 0)`
/// check. Call it in the forked child.
pub fn arm_parent_death_signal() -> nix::Result<()> {
    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM)
}

/// Returns the current parent PID.
pub fn parent_pid() -> Pid {
    nix::unistd::getppid()
}

/// Checks whether a process still exists.
pub fn is_alive(pid: Pid) -> bool {
    kill(pid, None::<Signal>).is_ok()
}

/// Non-blocking `waitpid`: returns `None` while the child is still running.
pub fn reap(pid: Pid) -> nix::Result<Option<WaitStatus>> {
    match waitpid(pid, Some(WaitPidFlag::WNOHANG))? {
        WaitStatus::StillAlive => Ok(None),
        status => Ok(Some(status)),
    }
}

/// Blocking `waitpid`.
pub fn wait(pid: Pid) -> nix::Result<WaitStatus> {
    waitpid(pid, None)
}
