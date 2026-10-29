use super::*;

#[test]
fn renderer_input_and_output_can_both_exceed_pipe_capacity() {
    let input = (0..1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let output = Cancellation::default()
        .output_with_input(
            Command::new("sh").args(["-c", "cat; printf diagnostic >&2"]),
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
            Command::new("sh")
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
