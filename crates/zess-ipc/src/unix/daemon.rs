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
    use std::{
        fs, io,
        mem::{MaybeUninit, size_of},
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        time::Duration,
    };

    use anyhow::{Context, Result, bail};

    use crate::{
        LocalIpc, SocketPath, Stream,
        path::SocketIdentity,
        platform::{ProcessStop, ProcessWait},
    };

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

    pub fn watch(stream: &Stream) -> Result<Box<dyn ProcessWait>> {
        let credentials = peer_credentials(stream)?;
        Ok(Box::new(PeerProcess(peer_pidfd(credentials.pid)?)))
    }

    fn connected_peer_pidfd(stream: &Stream, pid: libc::pid_t) -> Result<OwnedFd> {
        let mut fd = -1;
        let mut length = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: stream is live and fd and length point to writable integers.
        if unsafe {
            libc::getsockopt(
                stream.0.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERPIDFD,
                (&raw mut fd).cast(),
                &mut length,
            )
        } == 0
        {
            // SAFETY: SO_PEERPIDFD transferred ownership of a fresh descriptor.
            return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOPROTOOPT) {
            return Err(error).context("open connected socket peer pidfd");
        }

        // Older kernels identify the peer by PID. A live connection after
        // pidfd_open ensures the original server was still present at acquisition.
        let process = peer_pidfd(pid)?;
        let mut poll = libc::pollfd {
            fd: stream.0.as_raw_fd(),
            events: libc::POLLRDHUP,
            revents: 0,
        };
        // SAFETY: poll points to one initialized pollfd with a live descriptor.
        if unsafe { libc::poll(&mut poll, 1, 0) } == -1 {
            return Err(io::Error::last_os_error()).context("check socket peer connection");
        }
        if poll.revents != 0 {
            bail!("socket peer disconnected before process identification");
        }
        Ok(process)
    }

    struct StopPeer {
        process: PeerProcess,
        credentials: libc::ucred,
        socket: SocketPath,
        identity: SocketIdentity,
    }

    impl ProcessWait for StopPeer {
        fn wait(&self, timeout: Duration) -> io::Result<bool> {
            self.process.wait(timeout)
        }
    }

    impl ProcessStop for StopPeer {
        fn is_peer(&self, stream: &Stream) -> Result<bool> {
            Ok(peer_credentials(stream)?.pid == self.credentials.pid
                && !self.wait(Duration::ZERO)?)
        }

        fn terminate(&self) -> Result<()> {
            if self.wait(Duration::ZERO)? {
                return Ok(());
            }
            // SAFETY: geteuid takes no arguments and has no memory safety preconditions.
            if self.credentials.uid != unsafe { libc::geteuid() } {
                bail!("refusing to stop a server owned by another user");
            }

            let pid = self.credentials.pid;
            let executable = fs::read_link(format!("/proc/{pid}/exe"))?;
            let name = executable.file_name().unwrap_or_default().to_string_lossy();
            let arguments = fs::read(format!("/proc/{pid}/cmdline"))?;
            if name.trim_end_matches(" (deleted)") != "zess"
                || arguments.split(|byte| *byte == 0).nth(1) != Some(b"server")
            {
                bail!("refusing to signal socket peer {pid}: it is not a Zess server");
            }

            // SAFETY: the pidfd remains live. SIGTERM, null siginfo, and zero flags
            // are valid arguments to pidfd_send_signal, which retains no pointers.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.process.0.as_raw_fd(),
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0u32,
                )
            };
            if result == -1 {
                return Err(io::Error::last_os_error()).context("terminate server process");
            }
            if !self
                .wait(Duration::from_secs(2))
                .context("wait for server exit")?
            {
                bail!("server {pid} did not exit after SIGTERM");
            }

            self.socket
                .remove_if_same(&self.identity)
                .context("remove stopped server socket")
        }
    }

    pub fn connect_for_stop(
        ipc: &dyn LocalIpc,
        socket: &SocketPath,
    ) -> Result<(Stream, Box<dyn ProcessStop>)> {
        let identity = socket.identity().context("identify server socket")?;
        let stream = ipc.connect(socket).context("connect session")?;
        let credentials = peer_credentials(&stream)?;
        let process = PeerProcess(connected_peer_pidfd(&stream, credentials.pid)?);
        Ok((
            stream,
            Box::new(StopPeer {
                process,
                credentials,
                socket: socket.clone(),
                identity,
            }),
        ))
    }
}

#[cfg(target_os = "linux")]
pub use linux::{connect_for_stop, watch};

#[cfg(not(target_os = "linux"))]
pub fn watch(_stream: &crate::Stream) -> anyhow::Result<Box<dyn crate::platform::ProcessWait>> {
    anyhow::bail!("waiting for the server process is unavailable on this platform")
}

#[cfg(not(target_os = "linux"))]
pub fn connect_for_stop(
    _ipc: &dyn crate::LocalIpc,
    _socket: &crate::SocketPath,
) -> anyhow::Result<(crate::Stream, Box<dyn crate::platform::ProcessStop>)> {
    anyhow::bail!("waiting for the server process is unavailable on this platform")
}
