use anyhow::Result;

pub const CLIPBOARD_HELPER_ARG: &str = "--zess-clipboard-helper";

/// Runs the isolated native clipboard operation. `read` emits UTF-8 to stdout;
/// `write` consumes UTF-8 from stdin.
/// The parent owns the three-second deadline.
pub fn run_clipboard_helper(operation: &str) -> Result<()> {
    #[cfg(windows)]
    {
        windows::run(operation)
    }
    #[cfg(not(windows))]
    {
        let _ = operation;
        anyhow::bail!("native clipboard helper is only available on Windows")
    }
}

#[cfg(windows)]
mod windows {
    use std::{
        io::{self, Read, Write},
        ptr,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail, ensure};
    use windows_sys::{
        Win32::{
            Foundation::{GlobalFree, HGLOBAL, HWND},
            System::{
                DataExchange::{
                    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
                    OpenClipboard, SetClipboardData,
                },
                Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
                Ole::CF_UNICODETEXT,
            },
            UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
        },
        core::w,
    };
    use zess_protocol::{MAX_INPUT_BYTES, MAX_MESSAGE_BYTES};

    struct Window(HWND);

    impl Drop for Window {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    struct Clipboard;

    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    struct Memory(HGLOBAL);

    impl Drop for Memory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    GlobalFree(self.0);
                }
            }
        }
    }

    struct LockedMemory(HGLOBAL);

    impl Drop for LockedMemory {
        fn drop(&mut self) {
            unsafe {
                GlobalUnlock(self.0);
            }
        }
    }

    pub(super) fn run(operation: &str) -> Result<()> {
        ensure!(
            matches!(operation, "read" | "write"),
            "invalid clipboard helper operation"
        );
        let mut input = String::new();
        if operation == "write" {
            io::stdin()
                .take(MAX_MESSAGE_BYTES as u64 + 1)
                .read_to_string(&mut input)
                .context("read clipboard text")?;
            ensure!(
                input.len() <= MAX_MESSAGE_BYTES,
                "clipboard contents exceed the input limit"
            );
            ensure!(
                !input.contains('\0'),
                "clipboard text contains a NUL character"
            );
        }
        // EmptyClipboard requires a real owner HWND before SetClipboardData.
        let window = Window(unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                w!("Zess Clipboard"),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        });
        ensure!(
            !window.0.is_null(),
            "create clipboard owner window: {}",
            io::Error::last_os_error()
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if unsafe { OpenClipboard(window.0) } != 0 {
                break;
            }
            if Instant::now() >= deadline {
                bail!("open Windows clipboard: {}", io::Error::last_os_error());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _clipboard = Clipboard;
        if operation != "write" {
            if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
                return Ok(());
            }
            let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
            ensure!(
                !handle.is_null(),
                "read Windows clipboard: {}",
                io::Error::last_os_error()
            );
            let size = unsafe { GlobalSize(handle) };
            ensure!(
                size <= (MAX_INPUT_BYTES + 1) * 2,
                "clipboard contents exceed the input limit"
            );
            ensure!(
                size >= 2 && size % 2 == 0,
                "invalid Windows clipboard text size"
            );
            let pointer = unsafe { GlobalLock(handle) };
            ensure!(
                !pointer.is_null(),
                "lock Windows clipboard memory: {}",
                io::Error::last_os_error()
            );
            let _locked = LockedMemory(handle);
            let words = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2) };
            let end = words
                .iter()
                .position(|word| *word == 0)
                .context("Windows clipboard text is not terminated")?;
            let text = String::from_utf16(&words[..end])
                .context("Windows clipboard text is not valid UTF-16")?;
            ensure!(
                text.len() <= MAX_INPUT_BYTES,
                "clipboard contents exceed the input limit"
            );
            io::stdout()
                .write_all(text.as_bytes())
                .context("write clipboard text")?;
        } else {
            let words = input.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let mut memory = Memory(unsafe { GlobalAlloc(GMEM_MOVEABLE, words.len() * 2) });
            ensure!(
                !memory.0.is_null(),
                "allocate clipboard memory: {}",
                io::Error::last_os_error()
            );
            let pointer = unsafe { GlobalLock(memory.0) };
            ensure!(
                !pointer.is_null(),
                "lock clipboard memory: {}",
                io::Error::last_os_error()
            );
            {
                let _locked = LockedMemory(memory.0);
                unsafe {
                    ptr::copy_nonoverlapping(words.as_ptr(), pointer.cast::<u16>(), words.len());
                }
            }
            ensure!(
                unsafe { EmptyClipboard() } != 0,
                "empty Windows clipboard: {}",
                io::Error::last_os_error()
            );
            let result = unsafe { SetClipboardData(CF_UNICODETEXT as u32, memory.0) };
            ensure!(
                !result.is_null(),
                "write Windows clipboard: {}",
                io::Error::last_os_error()
            );
            // SetClipboardData transferred ownership to Windows.
            memory.0 = ptr::null_mut();
        }
        Ok(())
    }
}
