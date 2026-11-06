//! Termination-signal handling.
//!
//! The EC worker installs a handler that only sets an atomic flag; the worker
//! notices it between EC commands, so an in-flight command is never cut in
//! half. The UI uses a self-pipe instead: the handler writes one byte, and
//! GLib wakes up the main loop, which is the only place allowed to touch GTK.

use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use signal_hook::consts::signal::{
    SIGALRM, SIGHUP, SIGINT, SIGPIPE, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2,
};
use signal_hook::flag;

pub const TERMINATION_SIGNALS: [i32; 8] = [
    SIGHUP, SIGINT, SIGQUIT, SIGPIPE, SIGALRM, SIGTERM, SIGUSR1, SIGUSR2,
];

/// An atomic flag set by the termination-signal handler.
pub struct TerminationFlag(Arc<AtomicBool>);

impl TerminationFlag {
    pub fn install() -> io::Result<Self> {
        let flag = Arc::new(AtomicBool::new(false));
        for signal in TERMINATION_SIGNALS {
            flag::register(signal, Arc::clone(&flag))?;
        }
        Ok(Self(flag))
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Installs handlers for all [`TERMINATION_SIGNALS`] and returns the read end
/// of a self-pipe that becomes readable when one of them arrives.
///
/// Only the write end is touched inside the signal handler; the caller polls
/// the read end from a normal event loop.
pub fn install_signal_pipe() -> io::Result<OwnedFd> {
    let (read_fd, write_fd) = nix::unistd::pipe()?;

    for signal in TERMINATION_SIGNALS {
        // `register` takes ownership of the descriptor, so give each signal
        // its own duplicate. All duplicates share the same pipe.
        let write_fd = write_fd.try_clone()?;
        signal_hook::low_level::pipe::register(signal, write_fd)?;
    }

    Ok(read_fd)
}

#[cfg(test)]
mod tests {
    use super::install_signal_pipe;
    use signal_hook::low_level::raise;

    #[test]
    fn signal_pipe_becomes_readable() {
        let read_fd = install_signal_pipe().unwrap();
        raise(signal_hook::consts::SIGTERM).unwrap();

        let mut buf = [0u8; 16];
        let read = nix::unistd::read(&read_fd, &mut buf).unwrap();
        assert!(read >= 1);
    }
}
