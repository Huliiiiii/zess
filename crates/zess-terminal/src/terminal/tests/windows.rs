use std::fs::OpenOptions;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{
    AllocConsole, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE, GetConsoleMode, GetStdHandle,
    STD_OUTPUT_HANDLE, SetConsoleMode, SetStdHandle,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use super::*;

fn with_console(test_name: &str, test: impl FnOnce()) {
    if std::env::var("ZESS_TERMINAL_CONSOLE_TEST").as_deref() == Ok(test_name) {
        assert_ne!(unsafe { AllocConsole() }, 0);
        test();
    } else {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("ZESS_TERMINAL_CONSOLE_TEST", test_name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn mode(device: &str) -> u32 {
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .unwrap();
    let mut mode = 0;
    assert_ne!(
        unsafe { GetConsoleMode(handle.as_raw_handle(), &mut mode) },
        0
    );
    mode
}

fn modes() -> (u32, u32) {
    (mode("CONIN$"), mode("CONOUT$"))
}

fn enable_selection() {
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .unwrap();
    assert_ne!(
        unsafe {
            SetConsoleMode(
                handle.as_raw_handle(),
                mode("CONIN$") | ENABLE_EXTENDED_FLAGS | ENABLE_QUICK_EDIT_MODE,
            )
        },
        0
    );
}

#[test]
fn mouse_suspend_resume_and_drop_restore_the_original_console_modes() {
    with_console(
        "terminal::tests::windows::mouse_suspend_resume_and_drop_restore_the_original_console_modes",
        || {
            enable_selection();
            let original = modes();
            let mut terminal = CrosstermTerminal::enter().unwrap();
            terminal.set_mouse_capture(true).unwrap();
            assert_eq!(mode("CONIN$") & ENABLE_QUICK_EDIT_MODE, 0);
            terminal.suspend().unwrap();
            assert_eq!(modes(), original);
            terminal.resume().unwrap();
            assert!(terminal.mouse_capture());
            assert_eq!(
                mode("CONIN$") & (ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT),
                0
            );
            drop(terminal);
            assert_eq!(modes(), original);
        },
    );
}

#[test]
fn connection_failure_restores_console_modes_before_returning() {
    with_console(
        "terminal::tests::windows::connection_failure_restores_console_modes_before_returning",
        || {
            struct Unavailable;
            impl crate::ConnectionFactory for Unavailable {
                fn connect(&mut self) -> Result<Box<dyn crate::ConnectionStream>> {
                    anyhow::bail!("connection unavailable")
                }
            }
            enable_selection();
            let original = modes();
            let error = crate::run_connection(&mut Unavailable, &[]).unwrap_err();
            assert!(error.to_string().contains("connection unavailable"));
            assert_eq!(modes(), original);
        },
    );
}

#[test]
fn failed_screen_activation_restores_the_original_console_modes() {
    with_console(
        "terminal::tests::windows::failed_screen_activation_restores_the_original_console_modes",
        || {
            enable_selection();
            // Initialize Crossterm's ANSI capability cache before simulating failed output.
            drop(CrosstermTerminal::enter().unwrap());
            let original = modes();
            let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            assert_ne!(
                unsafe { SetStdHandle(STD_OUTPUT_HANDLE, INVALID_HANDLE_VALUE) },
                0
            );
            let result = CrosstermTerminal::enter();
            assert_ne!(unsafe { SetStdHandle(STD_OUTPUT_HANDLE, output) }, 0);
            assert!(result.is_err());
            assert_eq!(modes(), original);
        },
    );
}
