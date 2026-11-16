use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Instant;

use super::*;

fn attributes(fd: i32) -> libc::termios {
    let mut attributes = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(fd, &mut attributes) }, 0);
    attributes
}

#[test]
fn terminal_lifetimes_preserve_the_callers_input_and_protocol_state() {
    const NAME: &str =
        "terminal::tests::pty::terminal_lifetimes_preserve_the_callers_input_and_protocol_state";
    if std::env::var_os("ZESS_TERMINAL_PTY_TEST").is_some() {
        let mut terminal = CrosstermTerminal::enter().unwrap();
        terminal.set_mouse_capture(true).unwrap();
        terminal.suspend().unwrap();
        assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
        terminal.resume().unwrap();
        assert!(terminal.mouse_capture());
        drop(terminal);
        assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());

        crossterm::terminal::enable_raw_mode().unwrap();
        drop(CrosstermTerminal::enter().unwrap());
        assert!(crossterm::terminal::is_raw_mode_enabled().unwrap());
        crossterm::terminal::disable_raw_mode().unwrap();

        let output = unsafe { OwnedFd::from_raw_fd(libc::dup(1)) };
        let full = File::options().write(true).open("/dev/full").unwrap();
        assert_eq!(unsafe { libc::dup2(full.as_raw_fd(), 1) }, 1);
        let result = CrosstermTerminal::enter();
        assert_eq!(unsafe { libc::dup2(output.as_raw_fd(), 1) }, 1);
        assert!(result.is_err());
        assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
        return;
    }

    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let original = attributes(slave.as_raw_fd());
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture"])
        .env("ZESS_TERMINAL_PTY_TEST", "1")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(command);
    let output = std::thread::spawn(move || {
        let mut bytes = vec![];
        let mut buffer = [0; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let result = child.wait_with_output().unwrap();
    let restored = attributes(slave.as_raw_fd());
    drop(slave);
    let output = String::from_utf8_lossy(&output.join().unwrap()).into_owned();
    assert!(
        result.status.success(),
        "{}\n{}",
        output,
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        (
            restored.c_iflag,
            restored.c_oflag,
            restored.c_cflag,
            restored.c_lflag,
            restored.c_cc
        ),
        (
            original.c_iflag,
            original.c_oflag,
            original.c_cflag,
            original.c_lflag,
            original.c_cc
        )
    );
    assert_eq!(output.matches("\x1b[?1002h").count(), 2);
    assert_eq!(output.matches("\x1b[?1002l").count(), 2);
    let entered = output.matches("\x1b[?1049h").count();
    assert!(entered >= 3);
    assert_eq!(entered, output.matches("\x1b[?1049l").count());
    assert!(!output.contains("\x1b[<u"));
}
