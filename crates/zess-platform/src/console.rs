use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_INVALID_HANDLE};
use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, SetConsoleMode,
};

#[cfg(test)]
mod tests;

struct SavedMode {
    handle: File,
    mode: Option<u32>,
}

impl SavedMode {
    fn open(device: &str) -> io::Result<Self> {
        let handle = OpenOptions::new().read(true).write(true).open(device)?;
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle.as_raw_handle(), &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            mode: Some(mode),
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        if let Some(mode) = self.mode {
            if unsafe { SetConsoleMode(self.handle.as_raw_handle(), mode) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.mode = None;
        }
        Ok(())
    }
}

/// Saves attached console modes even when a subprocess uses redirected standard I/O.
pub struct ConsoleState {
    input: SavedMode,
    output: SavedMode,
}

impl ConsoleState {
    pub fn capture() -> io::Result<Option<Self>> {
        let input = match SavedMode::open("CONIN$") {
            Ok(input) => input,
            Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_INVALID_HANDLE as i32) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        Ok(Some(Self {
            input,
            output: SavedMode::open("CONOUT$")?,
        }))
    }

    pub fn enable_virtual_terminal_output(&self) -> io::Result<()> {
        let mut mode = 0;
        if unsafe { GetConsoleMode(self.output.handle.as_raw_handle(), &mut mode) } == 0
            || unsafe {
                SetConsoleMode(
                    self.output.handle.as_raw_handle(),
                    mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                )
            } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn restore(&mut self) -> io::Result<()> {
        let input = self.input.restore();
        let output = self.output.restore();
        input.and(output)
    }
}

impl Drop for ConsoleState {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
