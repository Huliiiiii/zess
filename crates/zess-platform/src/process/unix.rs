use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::Command;

pub(super) struct ConsoleGuard {
    tty: Option<File>,
    previous_group: libc::pid_t,
}

impl ConsoleGuard {
    pub(super) fn prepare() -> io::Result<Option<Self>> {
        let tty = match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open("/dev/tty")
        {
            Ok(tty) => tty,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENXIO | libc::ENOENT | libc::ENODEV)
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let previous_group = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
        if previous_group == -1 {
            return Err(io::Error::last_os_error());
        }
        if previous_group != unsafe { libc::getpgrp() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "interactive process requires the caller's terminal foreground",
            ));
        }
        Ok(Some(Self {
            tty: Some(tty),
            previous_group,
        }))
    }

    pub(super) fn configure(&self, command: &mut Command) {
        let tty = self.tty.as_ref().unwrap().as_raw_fd();
        // Runs after Command sets the child's process group, before it can read /dev/tty.
        unsafe {
            command.pre_exec(move || set_foreground(tty, libc::getpid()));
        }
    }

    pub(super) fn release(&mut self) -> io::Result<()> {
        if let Some(tty) = &self.tty {
            set_foreground(tty.as_raw_fd(), self.previous_group)?;
            self.tty = None;
        }
        Ok(())
    }
}

impl Drop for ConsoleGuard {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn set_foreground(tty: i32, group: libc::pid_t) -> io::Result<()> {
    // The caller is in the background when it takes the terminal back. Block
    // SIGTTOU on this thread without changing the process-wide signal disposition.
    unsafe {
        let mut blocked = std::mem::zeroed();
        let mut previous = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let result = libc::tcsetpgrp(tty, group);
        let error = if result == -1 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        let mask_error = libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if let Some(error) = error {
            Err(error)
        } else if mask_error != 0 {
            Err(io::Error::from_raw_os_error(mask_error))
        } else {
            Ok(())
        }
    }
}
