use std::fs::OpenOptions;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::System::Console::{
    AllocConsole, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE, GetConsoleMode, GetStdHandle, INPUT_RECORD,
    KEY_EVENT, KEY_EVENT_RECORD, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode, SetStdHandle,
    WriteConsoleInputW,
};
use windows_sys::Win32::System::Threading::DETACHED_PROCESS;

use super::*;

fn with_console(test_name: &str, test: impl FnOnce()) {
    if std::env::var("ZESS_TERMINAL_CONSOLE_TEST").as_deref() == Ok(test_name) {
        assert!(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("CONIN$")
                .is_err(),
            "helper already has a console"
        );
        let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if unsafe { AllocConsole() } == 0 {
            panic!("allocate helper console: {}", io::Error::last_os_error());
        }
        assert_eq!(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }, stdout);
        assert_eq!(unsafe { GetStdHandle(STD_ERROR_HANDLE) }, stderr);
        test();
    } else {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("ZESS_TERMINAL_CONSOLE_TEST", test_name)
            .creation_flags(DETACHED_PROCESS)
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
                fn connect(
                    &mut self,
                    _: &crate::ConnectionContext,
                ) -> Result<Box<dyn crate::ConnectionStream>> {
                    // The failed connection remains in the UI until the user exits.
                    let console = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open("CONIN$")
                        .unwrap();
                    let mut event = INPUT_RECORD {
                        EventType: KEY_EVENT as u16,
                        Event: unsafe { std::mem::zeroed() },
                    };
                    event.Event.KeyEvent = KEY_EVENT_RECORD {
                        bKeyDown: 1,
                        wRepeatCount: 1,
                        wVirtualKeyCode: 0x1b,
                        wVirtualScanCode: 0,
                        uChar: unsafe { std::mem::zeroed() },
                        dwControlKeyState: 0,
                    };
                    event.Event.KeyEvent.uChar.UnicodeChar = 0x1b;
                    let mut written = 0;
                    assert_ne!(
                        unsafe {
                            WriteConsoleInputW(console.as_raw_handle(), &event, 1, &mut written)
                        },
                        0
                    );
                    assert_eq!(written, 1);
                    anyhow::bail!("connection unavailable")
                }
            }
            enable_selection();
            let original = modes();
            crate::run_connection(Unavailable, &[]).unwrap();
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
            let read_only = OpenOptions::new()
                .read(true)
                .open(std::env::current_exe().unwrap())
                .unwrap();
            assert_ne!(
                unsafe { SetStdHandle(STD_OUTPUT_HANDLE, read_only.as_raw_handle()) },
                0
            );
            let result = CrosstermTerminal::enter();
            assert_ne!(unsafe { SetStdHandle(STD_OUTPUT_HANDLE, output) }, 0);
            let error = result
                .err()
                .expect("read-only stdout must reject screen output");
            assert_eq!(
                error
                    .downcast_ref::<io::Error>()
                    .and_then(io::Error::raw_os_error),
                Some(ERROR_ACCESS_DENIED as i32)
            );
            assert_eq!(modes(), original);
        },
    );
}
