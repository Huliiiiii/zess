use std::{
    io::Write,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use super::*;

fn package(directory: &std::path::Path) -> Vec<u8> {
    let source = directory.join("package");
    std::fs::create_dir_all(source.join("lib")).unwrap();
    std::fs::write(source.join("zess"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(source.join("patchelf"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        source.join("patchelf"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(source.join("lib/ld-linux-x86-64.so.2"), b"loader").unwrap();
    let archive = directory.join("server.tar.gz");
    assert!(
        Command::new("tar")
            .args([
                "-czf",
                archive.to_str().unwrap(),
                "-C",
                source.to_str().unwrap(),
                "."
            ])
            .status()
            .unwrap()
            .success()
    );
    std::fs::read(archive).unwrap()
}

fn install_command(home: &std::path::Path, bytes: &[u8]) -> std::process::Child {
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Command::new("sh")
        .args([
            "-c",
            &RemotePlatform::new(ServerTarget::LinuxX86_64).install(&hash, bytes.len()),
        ])
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap()
}

fn wait_for_exit(child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("installation did not finish");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn installs(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".cache/zess/servers").join(zess_build::version())
}

#[test]
fn installation_finishes_when_all_bytes_arrive_without_stdin_eof() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = package(directory.path());
    let mut child = install_command(directory.path(), &bytes);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&bytes).unwrap();
    stdin.flush().unwrap();
    wait_for_exit(&mut child);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(installs(directory.path()).join("current/zess").exists());
    drop(stdin);
}

#[test]
fn truncated_upload_removes_its_staging_directory() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = package(directory.path());
    let mut child = install_command(directory.path(), &bytes);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&bytes[..bytes.len() / 2])
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("server upload truncated"));
    assert_eq!(
        std::fs::read_dir(installs(directory.path()))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn cancelled_upload_removes_only_its_staging_directory() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = package(directory.path());
    let base = installs(directory.path());
    let existing = base.join("existing");
    std::fs::create_dir_all(&existing).unwrap();
    std::fs::write(existing.join("keep"), b"existing session").unwrap();
    let mut child = install_command(directory.path(), &bytes);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&bytes[..bytes.len() / 2]).unwrap();
    stdin.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(&base).unwrap().count() < 2 {
        assert!(Instant::now() < deadline, "installation did not start");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Command::new("sh")
            .args(["-c", &format!("kill -TERM -- -{}", child.id())])
            .status()
            .unwrap()
            .success()
    );
    wait_for_exit(&mut child);
    assert!(!child.wait().unwrap().success());
    assert_eq!(
        std::fs::read(existing.join("keep")).unwrap(),
        b"existing session"
    );
    assert_eq!(std::fs::read_dir(base).unwrap().count(), 1);
    drop(stdin);
}
