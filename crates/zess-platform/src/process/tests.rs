use super::*;

#[cfg(unix)]
fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.args(["-c", script]);
    spec
}

#[cfg(unix)]
#[test]
fn process_input_and_both_output_streams_can_exceed_pipe_capacity() {
    let spec = shell("head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2; cat");
    let cancellation = JobCancellation {
        cancelled: CancellationToken::default(),
        deadline: Instant::now() + Duration::from_secs(5),
    };
    let input = vec![b'x'; 262144];
    let output = SystemProcessRunner
        .output(&spec, Some(&input), &cancellation)
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout[..262144], vec![0; 262144]);
    assert_eq!(output.stdout[262144..], input);
    assert_eq!(output.stderr, vec![0; 262144]);
}

#[cfg(unix)]
#[test]
fn command_output_completes_when_a_descendant_inherits_the_pipes() {
    let spec = shell("sleep 5 & printf result; exit 0");
    let cancellation = JobCancellation {
        cancelled: CancellationToken::default(),
        deadline: Instant::now() + Duration::from_secs(2),
    };
    let output = SystemProcessRunner
        .output(&spec, None, &cancellation)
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"result");
}

#[cfg(unix)]
#[test]
fn oversized_output_stops_process_without_waiting_for_exit() {
    let mut spec = shell("head -c 65536 /dev/zero; sleep 5");
    spec.output_limit = Some(1024);
    let cancellation = JobCancellation {
        cancelled: CancellationToken::default(),
        deadline: Instant::now() + Duration::from_secs(4),
    };
    let start = Instant::now();
    let error = SystemProcessRunner
        .output(&spec, None, &cancellation)
        .unwrap_err();
    assert!(error.to_string().contains("byte limit"));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn starting_a_process_job_reports_missing_executable_immediately() {
    let directory = tempfile::tempdir().unwrap();
    let spec = ProcessSpec::new(directory.path().join("missing-command"));
    let result = ProcessJob::start(
        Arc::new(SystemProcessRunner),
        spec,
        None,
        Duration::from_secs(3),
        1024,
    );
    let error = match result {
        Ok(_) => panic!("missing command started"),
        Err(error) => error,
    };
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::NotFound
    );
}

#[cfg(unix)]
#[test]
fn dropping_a_process_job_releases_its_running_process() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("survived");
    let mut spec = shell("sleep 1; touch \"$MARKER\"");
    spec.env("MARKER", &marker);
    let job = ProcessJob::start(
        Arc::new(SystemProcessRunner),
        spec,
        None,
        Duration::from_secs(5),
        1024,
    )
    .unwrap();
    let start = Instant::now();
    drop(job);
    assert!(start.elapsed() < Duration::from_millis(100));
    thread::sleep(Duration::from_millis(1100));
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn successful_owned_process_stops_descendants_that_redirect_output() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("descendant");
    let mut spec =
        shell("(sleep 0.2; printf survived > \"$MARKER\") </dev/null >/dev/null 2>&1 & exit 0");
    spec.env("MARKER", &marker);
    let cancellation = JobCancellation {
        cancelled: CancellationToken::default(),
        deadline: Instant::now() + Duration::from_secs(2),
    };
    let output = SystemProcessRunner
        .output(&spec, None, &cancellation)
        .unwrap();
    assert!(output.status.success());
    thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists());
}

#[cfg(unix)]
fn run_in_terminal(scenario: &str, input: &[u8]) {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
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
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "process::tests::terminal_fixture", "--nocapture"])
        .env("ZESS_TERMINAL_FIXTURE", scenario)
        .stdin(slave)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    master.write_all(input).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn interactive_output_can_read_the_terminal_and_returns_its_foreground() {
    run_in_terminal("output", b"typed-secret\n");
}

#[cfg(unix)]
#[test]
fn releasing_a_running_process_console_returns_terminal_input_to_the_caller() {
    run_in_terminal("release", b"typed-secret\ncaller-input\n");
}

#[cfg(unix)]
#[test]
fn interactive_process_failure_and_cancellation_restore_terminal_foreground() {
    run_in_terminal("failure", b"");
}

#[cfg(unix)]
#[test]
fn terminal_fixture() {
    use std::os::fd::AsRawFd;
    let Ok(scenario) = std::env::var("ZESS_TERMINAL_FIXTURE") else {
        return;
    };
    let tty = std::fs::File::open("/dev/tty").unwrap();
    let caller = unsafe { libc::getpgrp() };
    assert_eq!(unsafe { libc::tcgetpgrp(tty.as_raw_fd()) }, caller);
    let mut spec = shell("read secret < /dev/tty; printf '%s' \"$secret\"");
    spec.console_mode = ConsoleMode::Inherit;
    let cancellation = JobCancellation {
        cancelled: CancellationToken::default(),
        deadline: Instant::now() + Duration::from_secs(2),
    };
    match scenario.as_str() {
        "output" => {
            let output = SystemProcessRunner
                .output(&spec, None, &cancellation)
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"typed-secret");
        }
        "release" => {
            spec.args[1] = "read secret < /dev/tty; printf '%s' \"$secret\"; sleep 5".into();
            let mut child = SystemProcessRunner.spawn(&spec).unwrap();
            let mut ready = [0; 12];
            child.take_stdout().unwrap().read_exact(&mut ready).unwrap();
            assert_eq!(&ready, b"typed-secret");
            assert_ne!(unsafe { libc::tcgetpgrp(tty.as_raw_fd()) }, caller);
            child.release_console().unwrap();
            assert_eq!(unsafe { libc::tcgetpgrp(tty.as_raw_fd()) }, caller);
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(&tty), &mut line).unwrap();
            assert_eq!(line, "caller-input\n");
            drop(child);
        }
        "failure" => {
            let mut missing = ProcessSpec::new("/zess-missing-interactive-executable");
            missing.console_mode = ConsoleMode::Inherit;
            assert!(SystemProcessRunner.spawn(&missing).is_err());
            assert_eq!(unsafe { libc::tcgetpgrp(tty.as_raw_fd()) }, caller);
            let cancellation = JobCancellation {
                cancelled: CancellationToken::default(),
                deadline: Instant::now() + Duration::from_millis(100),
            };
            let error = SystemProcessRunner
                .output(&spec, None, &cancellation)
                .unwrap_err();
            assert!(error.to_string().contains("timed out"));
        }
        _ => panic!("unknown terminal fixture"),
    }
    assert_eq!(unsafe { libc::tcgetpgrp(tty.as_raw_fd()) }, caller);
    std::process::exit(0);
}

#[cfg(windows)]
#[test]
fn subprocess_fixture() {
    let Some(role) = std::env::var_os("ZESS_PROCESS_FIXTURE") else {
        return;
    };
    if role == "parent" {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::subprocess_fixture",
                "--nocapture",
            ])
            .env("ZESS_PROCESS_FIXTURE", "descendant")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        println!("parent output");
    } else {
        thread::sleep(Duration::from_secs(15));
    }
    std::process::exit(0);
}

#[cfg(windows)]
#[test]
fn command_output_completes_when_a_descendant_inherits_the_pipes() {
    let mut spec = ProcessSpec::new(std::env::current_exe().unwrap());
    spec.args([
        "--exact",
        "process::tests::subprocess_fixture",
        "--nocapture",
    ])
    .env("ZESS_PROCESS_FIXTURE", "parent");
    let cancellation = JobCancellation {
        cancelled: CancellationToken::default(),
        deadline: Instant::now() + Duration::from_secs(4),
    };
    let output = SystemProcessRunner
        .output(&spec, None, &cancellation)
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("parent output"));
}
