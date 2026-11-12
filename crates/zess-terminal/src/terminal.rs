use std::{
    io::{self, Write},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, Event},
    execute,
    style::{Attribute, ResetColor, SetAttribute},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

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

pub struct CrosstermTerminal {
    writer: io::Stdout,
    alternate_screen: bool,
    raw_mode: bool,
    mouse_capture: bool,
    bracketed_paste: bool,
    suspended_mouse_capture: Option<bool>,
}

impl CrosstermTerminal {
    pub fn enter() -> Result<Self> {
        let mut terminal = Self {
            writer: io::stdout(),
            alternate_screen: false,
            raw_mode: false,
            mouse_capture: false,
            bracketed_paste: false,
            suspended_mouse_capture: None,
        };
        terminal.activate()?;
        Ok(terminal)
    }

    fn activate(&mut self) -> Result<()> {
        terminal::enable_raw_mode().context("enable terminal raw mode")?;
        self.raw_mode = true;
        self.alternate_screen = true;
        execute!(self.writer, EnterAlternateScreen).context("enter terminal alternate screen")?;
        self.bracketed_paste = true;
        execute!(
            self.writer,
            Hide,
            Clear(ClearType::All),
            EnableBracketedPaste
        )
        .context("initialize terminal screen")?;
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        self.set_mouse_capture(false)?;
        if self.bracketed_paste {
            execute!(self.writer, DisableBracketedPaste)?;
            self.bracketed_paste = false;
        }
        if self.alternate_screen {
            execute!(
                self.writer,
                ResetColor,
                SetAttribute(Attribute::Reset),
                SetCursorStyle::DefaultUserShape,
                Show,
                LeaveAlternateScreen
            )?;
            self.alternate_screen = false;
        }
        if self.raw_mode {
            terminal::disable_raw_mode().context("restore terminal input mode")?;
            self.raw_mode = false;
        }
        Ok(())
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
            self.suspended_mouse_capture = Some(self.mouse_capture);
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
        self.mouse_capture
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_capture != enabled {
            if enabled {
                self.mouse_capture = true;
                self.writer.write_all(b"\x1b[?1002h\x1b[?1006h")?;
                self.writer.flush()?;
            } else {
                execute!(self.writer, DisableMouseCapture)?;
                self.mouse_capture = false;
            }
        }
        Ok(())
    }
}

impl Drop for CrosstermTerminal {
    fn drop(&mut self) {
        if self.mouse_capture {
            let _ = execute!(self.writer, DisableMouseCapture);
        }
        if self.bracketed_paste {
            let _ = execute!(self.writer, DisableBracketedPaste);
        }
        if self.alternate_screen {
            let _ = execute!(
                self.writer,
                ResetColor,
                SetAttribute(Attribute::Reset),
                SetCursorStyle::DefaultUserShape,
                Show,
                LeaveAlternateScreen
            );
        }
        if self.raw_mode {
            let _ = terminal::disable_raw_mode();
        }
    }
}
