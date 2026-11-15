use std::{os::unix::process::CommandExt, process::Command};

pub fn detach(command: &mut Command) -> anyhow::Result<()> {
    // The server must survive SIGHUP when the client's terminal closes.
    // SAFETY: setsid is async-signal-safe. The callback captures no state, and its
    // error path reads errno without allocating. It does not access locks or
    // duplicated Rust resources between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::{SocketPath, Stream};
    use std::{
        fs, io,
        mem::{MaybeUninit, size_of},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::fs::MetadataExt,
        },
        time::Duration,
    };

    use anyhow::{Context, Result, bail};

    use crate::platform::ProcessWait;

    struct PeerProcess(OwnedFd);

    impl ProcessWait for PeerProcess {
        fn wait(&self, timeout: Duration) -> io::Result<bool> {
            let mut poll = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = timeout.as_millis() + u128::from(timeout.subsec_nanos() % 1_000_000 != 0);
            let timeout = millis.min(i32::MAX as u128) as i32;
            // SAFETY: poll points to one initialized pollfd with a live descriptor.
            let result = unsafe { libc::poll(&mut poll, 1, timeout) };
            if result == -1 {
                Err(io::Error::last_os_error())
            } else if poll.revents & libc::POLLNVAL != 0 {
                Err(io::Error::from_raw_os_error(libc::EBADF))
            } else {
                Ok(result > 0)
            }
        }
    }

    pub fn watch(stream: &Stream) -> Result<Box<dyn ProcessWait>> {
        let credentials = peer_credentials(stream)?;
        Ok(Box::new(PeerProcess(peer_pidfd(credentials.pid)?)))
    }

    fn peer_pidfd(pid: libc::pid_t) -> Result<OwnedFd> {
        // SAFETY: pid comes from SO_PEERCRED and zero is a valid flags value.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
        if descriptor == -1 {
            return Err(io::Error::last_os_error()).context("open socket peer pidfd");
        }
        // SAFETY: successful pidfd_open transferred ownership of a fresh descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor as libc::c_int) })
    }

    fn peer_credentials(stream: &Stream) -> Result<libc::ucred> {
        let mut credentials = MaybeUninit::<libc::ucred>::uninit();
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;

        // SAFETY: stream is live, length is initialized, and credentials is an aligned,
        // writable ucred buffer whose size is passed to getsockopt.
        let result = unsafe {
            libc::getsockopt(
                stream.0.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                credentials.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error()).context("read socket peer credentials");
        }
        if length as usize != size_of::<libc::ucred>() {
            bail!("invalid socket peer credentials");
        }

        // SAFETY: SO_PEERCRED succeeded and returned exactly the size of ucred,
        // initializing all of its integer fields.
        let credentials = unsafe { credentials.assume_init() };
        if credentials.pid <= 1 {
            bail!("server PID is not visible; run this command in the server's PID namespace");
        }
        Ok(credentials)
    }

    pub fn stop(socket: &SocketPath) -> Result<()> {
        let metadata = fs::symlink_metadata(socket.as_path())?;
        let stream =
            Stream::connect(socket).context("connect to server for process identification")?;
        let credentials = peer_credentials(&stream)?;
        // SAFETY: geteuid takes no arguments and has no memory safety preconditions.
        if credentials.uid != unsafe { libc::geteuid() } {
            bail!("refusing to stop a server owned by another user");
        }

        let process = peer_pidfd(credentials.pid)?;
        let executable = fs::read_link(format!("/proc/{}/exe", credentials.pid))?;
        let name = executable.file_name().unwrap_or_default().to_string_lossy();
        let arguments = fs::read(format!("/proc/{}/cmdline", credentials.pid))?;
        if name.trim_end_matches(" (deleted)") != "zess"
            || arguments.split(|byte| *byte == 0).nth(1) != Some(b"server")
        {
            bail!(
                "refusing to signal socket peer {}: it is not a Zess server",
                credentials.pid
            );
        }

        // SAFETY: process owns a live pidfd. SIGTERM, null siginfo, and zero flags
        // are valid arguments to pidfd_send_signal, which retains no pointers.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                process.as_raw_fd(),
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error()).context("terminate server process");
        }

        let mut poll = libc::pollfd {
            fd: process.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one initialized, writable pollfd, its descriptor
        // remains live, and nfds is exactly one.
        let result = unsafe { libc::poll(&mut poll, 1, 2000) };
        if result == -1 {
            return Err(io::Error::last_os_error()).context("wait for server exit");
        }
        if result == 0 {
            bail!("server {} did not exit after SIGTERM", credentials.pid);
        }

        match fs::symlink_metadata(socket.as_path()) {
            Ok(current) if current.dev() == metadata.dev() && current.ino() == metadata.ino() => {
                fs::remove_file(socket.as_path()).context("remove stopped server socket")?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("check stopped server socket"),
        }

        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub use linux::{stop, watch};

#[cfg(not(target_os = "linux"))]
pub fn watch(_stream: &crate::Stream) -> anyhow::Result<Box<dyn crate::platform::ProcessWait>> {
    anyhow::bail!("waiting for the server process is unavailable on this platform")
}

#[cfg(not(target_os = "linux"))]
pub fn stop(_socket: &crate::SocketPath) -> anyhow::Result<()> {
    anyhow::bail!(
        "forced process termination is unavailable on this platform; use the server stop protocol"
    )
}
