use super::*;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::Duration;

#[cfg(unix)]
#[test]
fn renderer_input_and_output_can_both_exceed_pipe_capacity() {
    let input = (0..1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let output = Cancellation::default()
        .output_with_input(
            ProcessSpec::new("sh").args(["-c", "cat; printf diagnostic >&2"]),
            &input,
        )
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, input);
    assert_eq!(output.stderr, b"diagnostic");
}

#[cfg(unix)]
#[test]
fn cancelling_a_renderer_also_stops_children_that_hold_its_output_pipes() {
    let path =
        std::env::temp_dir().join(format!("zess-renderer-cancellation-{}", std::process::id()));
    let cancellation = Cancellation::default();
    let worker_cancellation = cancellation.clone();
    let (sender, result) = mpsc::channel();
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        let result = worker_cancellation.output(
            ProcessSpec::new("sh")
                .args([
                    "-c",
                    "sleep 30 & printf ready > \"$1\"; wait",
                    "zess-renderer",
                ])
                .arg(worker_path),
        );
        sender.send(result).unwrap();
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "renderer did not start"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    cancellation.cancel();
    let error = result
        .recv_timeout(Duration::from_secs(5))
        .expect("renderer children kept the output pipes open")
        .unwrap_err();
    assert_eq!(error.to_string(), "background job was cancelled");
    worker.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn cancellation_stops_pipe_holders_after_the_direct_child_has_been_reaped() {
    struct Renderer {
        root: std::path::PathBuf,
        process_group: Option<i32>,
    }

    impl Drop for Renderer {
        fn drop(&mut self) {
            if let Some(group) = self.process_group {
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    let root = std::env::temp_dir().join(format!(
        "zess-renderer-draining-cancellation-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut renderer = Renderer {
        root,
        process_group: None,
    };
    let gate = renderer.root.join("gate");
    assert!(
        Command::new("mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success()
    );
    let parent = renderer.root.join("parent");
    let holder = renderer.root.join("holder");
    let cancellation = Cancellation::default();
    let worker_cancellation = cancellation.clone();
    let (sender, result) = mpsc::channel();
    let worker_parent = parent.clone();
    let worker_holder = holder.clone();
    let worker = std::thread::spawn(move || {
        let output = worker_cancellation.output(
            ProcessSpec::new("sh")
                .args([
                    "-c",
                    "sh -c 'printf %s \"$$\" > \"$1\"; read line < \"$2\"' pipe-holder \"$2\" \"$3\" & printf %s \"$$\" > \"$1\"; exit 0",
                    "zess-renderer",
                ])
                .arg(worker_parent)
                .arg(worker_holder)
                .arg(gate),
        );
        sender.send(output).unwrap();
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let (parent_pid, holder_pid) = loop {
        let parent_pid = std::fs::read_to_string(&parent)
            .ok()
            .and_then(|pid| pid.parse::<i32>().ok());
        let holder_pid = std::fs::read_to_string(&holder)
            .ok()
            .and_then(|pid| pid.parse::<i32>().ok());
        if let Some(parent_pid) = parent_pid {
            renderer.process_group = Some(parent_pid);
        }
        if let (Some(parent_pid), Some(holder_pid)) = (parent_pid, holder_pid)
            && !std::path::Path::new(&format!("/proc/{parent_pid}")).exists()
        {
            break (parent_pid, holder_pid);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "direct child was not reaped"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(renderer.process_group, Some(parent_pid));
    assert!(std::path::Path::new(&format!("/proc/{holder_pid}")).exists());
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));

    cancellation.cancel();
    let error = result
        .recv_timeout(Duration::from_secs(5))
        .expect("pipe readers did not finish after cancellation")
        .unwrap_err();
    assert_eq!(error.to_string(), "background job was cancelled");
    worker.join().unwrap();
    // An orphan can remain as a zombie until its new parent reaps it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while let Ok(status) = std::fs::read_to_string(format!("/proc/{holder_pid}/status")) {
        if status.lines().any(|line| line.starts_with("State:\tZ")) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pipe holder survived cancellation: {status}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
