use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use windows_sys::Win32::System::Console::{
    AllocConsole, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use super::*;
use crate::{CancellationToken, ConsoleMode, ProcessRunner, ProcessSpec, SystemProcessRunner};

fn modes() -> (u32, u32) {
    (
        SavedMode::open("CONIN$").unwrap().mode.unwrap(),
        SavedMode::open("CONOUT$").unwrap().mode.unwrap(),
    )
}

fn change_modes() {
    let input = SavedMode::open("CONIN$").unwrap();
    let output = SavedMode::open("CONOUT$").unwrap();
    let input_mode = (input.mode.unwrap() & !(ENABLE_ECHO_INPUT | ENABLE_QUICK_EDIT_MODE))
        | ENABLE_EXTENDED_FLAGS;
    let output_mode = output.mode.unwrap() ^ ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    assert_ne!(
        unsafe { SetConsoleMode(input.handle.as_raw_handle(), input_mode) },
        0
    );
    assert_ne!(
        unsafe { SetConsoleMode(output.handle.as_raw_handle(), output_mode) },
        0
    );
}

#[test]
fn inherited_children_cannot_leave_changed_console_modes_after_exit_or_drop() {
    const NAME: &str =
        "console::tests::inherited_children_cannot_leave_changed_console_modes_after_exit_or_drop";
    const ROLE: &str = "ZESS_CONSOLE_MODE_TEST_ROLE";
    match std::env::var(ROLE).as_deref() {
        Ok("exit" | "wait") => {
            change_modes();
            if std::env::var(ROLE).unwrap() == "wait" {
                println!("console mutated");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(30));
            }
            std::process::exit(0);
        }
        Ok("console") => {}
        _ => {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env(ROLE, "console")
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
    }
    // The isolated helper owns this real console; no CI runner console is changed.
    assert_ne!(unsafe { AllocConsole() }, 0);
    let original = modes();
    let mut spec = ProcessSpec::new(std::env::current_exe().unwrap());
    spec.args(["--exact", NAME, "--nocapture"]);
    spec.console_mode = ConsoleMode::Inherit;
    spec.env(ROLE, "exit");
    assert!(
        SystemProcessRunner
            .output(&spec, None, &CancellationToken::new())
            .unwrap()
            .status
            .success()
    );
    assert_eq!(modes(), original);

    spec.env = vec![(ROLE.into(), "wait".into())];
    let mut child = SystemProcessRunner.spawn(&spec).unwrap();
    let mut reader = BufReader::new(child.take_stdout().unwrap());
    let mut line = String::new();
    while !line.contains("console mutated") {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
    }
    assert_ne!(modes(), original);
    drop(child);
    assert_eq!(modes(), original);
}
