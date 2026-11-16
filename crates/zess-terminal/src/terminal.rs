use std::{
    io::{self, Write},
    time::Duration,
};

use anyhow::{Context, Result};
#[cfg(windows)]
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
    style::{Attribute, ResetColor, SetAttribute},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

#[cfg(test)]
mod tests;

/// Owns the terminal event reader, output writer, and restoration lifetime.
pub trait TerminalIo {
    type Writer: Write;

    fn writer(&mut self) -> &mut Self::Writer;
    fn size(&self) -> Result<(u16, u16)>;
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>>;
    fn mouse_capture(&self) -> bool;
    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()>;
    fn suspend(&mut self) -> Result<()> {
        Ok(())
    }
    fn resume(&mut self) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct NativeMode {
    #[cfg(not(windows))]
    owns_raw_mode: bool,
    #[cfg(windows)]
    console: Option<zess_platform::ConsoleState>,
}

impl NativeMode {
    fn enter(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.console = zess_platform::ConsoleState::capture()?;
            if let Some(console) = &self.console {
                // Crossterm caches ANSI support, so each activation must enable VT again.
                console.enable_virtual_terminal_output()?;
            }
            terminal::enable_raw_mode()?;
        }
        #[cfg(not(windows))]
        if !terminal::is_raw_mode_enabled()? {
            self.owns_raw_mode = true;
            terminal::enable_raw_mode()?;
        }
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        if let Some(console) = &mut self.console {
            console.restore()?;
            self.console = None;
        }
        #[cfg(not(windows))]
        if self.owns_raw_mode {
            terminal::disable_raw_mode()?;
            self.owns_raw_mode = false;
        }
        Ok(())
    }
}

#[derive(Default)]
struct ScreenState {
    alternate_screen: bool,
    mouse_capture: bool,
    bracketed_paste: bool,
}

impl ScreenState {
    fn activate(&mut self, writer: &mut impl Write) -> Result<()> {
        self.alternate_screen = true;
        execute!(writer, EnterAlternateScreen).context("enter terminal alternate screen")?;
        self.bracketed_paste = true;
        execute!(writer, Hide, Clear(ClearType::All), EnableBracketedPaste)
            .context("initialize terminal screen")?;
        Ok(())
    }

    fn restore(&mut self, writer: &mut impl Write) -> Result<()> {
        let mouse = self.set_mouse_capture(writer, false);
        let paste = if self.bracketed_paste {
            let result = execute!(writer, DisableBracketedPaste);
            if result.is_ok() {
                self.bracketed_paste = false;
            }
            result
        } else {
            Ok(())
        };
        let screen = if self.alternate_screen {
            let style = execute!(writer, ResetColor, SetAttribute(Attribute::Reset));
            let shape = execute!(writer, SetCursorStyle::DefaultUserShape);
            let cursor = execute!(writer, Show);
            let leave = execute!(writer, LeaveAlternateScreen);
            if leave.is_ok() {
                self.alternate_screen = false;
            }
            style.and(shape).and(cursor).and(leave)
        } else {
            Ok(())
        };
        mouse
            .and(paste.map_err(Into::into))
            .and(screen.map_err(Into::into))
    }

    fn set_mouse_capture(&mut self, writer: &mut impl Write, enabled: bool) -> Result<()> {
        if self.mouse_capture != enabled {
            if enabled {
                self.mouse_capture = true;
                #[cfg(windows)]
                execute!(writer, EnableMouseCapture)?;
                #[cfg(not(windows))]
                writer.write_all(b"\x1b[?1002h\x1b[?1006h")?;
            } else {
                #[cfg(windows)]
                execute!(writer, DisableMouseCapture)?;
                #[cfg(not(windows))]
                writer.write_all(b"\x1b[?1006l\x1b[?1002l")?;
            }
            writer.flush()?;
            self.mouse_capture = enabled;
        }
        Ok(())
    }
}

pub struct CrosstermTerminal {
    writer: io::Stdout,
    mode: NativeMode,
    screen: ScreenState,
    suspended_mouse_capture: Option<bool>,
}

impl CrosstermTerminal {
    pub fn enter() -> Result<Self> {
        let mut terminal = Self {
            writer: io::stdout(),
            mode: NativeMode::default(),
            screen: ScreenState::default(),
            suspended_mouse_capture: None,
        };
        terminal.activate()?;
        Ok(terminal)
    }

    fn activate(&mut self) -> Result<()> {
        self.mode.enter().context("enable terminal input mode")?;
        self.screen.activate(&mut self.writer)
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        let screen = self.screen.restore(&mut self.writer);
        let mode = self.mode.restore().context("restore terminal input mode");
        screen.and(mode)
    }
}

impl TerminalIo for CrosstermTerminal {
    type Writer = io::Stdout;

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }

    fn size(&self) -> Result<(u16, u16)> {
        terminal::size().context("read terminal size")
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>> {
        if event::poll(timeout).context("poll terminal input")? {
            Ok(Some(event::read().context("read terminal input")?))
        } else {
            Ok(None)
        }
    }

    fn suspend(&mut self) -> Result<()> {
        if self.suspended_mouse_capture.is_none() {
            self.suspended_mouse_capture = Some(self.screen.mouse_capture);
            self.restore()?;
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if let Some(mouse_capture) = self.suspended_mouse_capture {
            self.activate()?;
            self.set_mouse_capture(mouse_capture)?;
            self.suspended_mouse_capture = None;
        }
        Ok(())
    }

    fn mouse_capture(&self) -> bool {
        self.screen.mouse_capture
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        self.screen.set_mouse_capture(&mut self.writer, enabled)
    }
}

impl Drop for CrosstermTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
