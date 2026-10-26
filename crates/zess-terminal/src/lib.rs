use std::{
    collections::VecDeque,
    io::{self, BufReader, Write},
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    SynchronizedUpdate,
    clipboard::CopyToClipboard,
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    event::{
        self, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{Attribute, Print, ResetColor, SetAttribute},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_ipc::{SocketPath, Stream, framing};
use zess_protocol::{ClientMessage, MAX_INPUT_BYTES, MAX_MESSAGE_BYTES, ServerMessage};
use zess_ui::{CursorShape, Frame, FrameRow, MouseEvent, MouseKind};

mod style;

#[cfg(test)]
mod tests;

struct TerminalGuard {
    alternate_screen: bool,
    raw_mode: bool,
    mouse_capture: bool,
}

impl TerminalGuard {
    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_capture == enabled {
            return Ok(());
        }
        let mut stdout = io::stdout();
        if enabled {
            self.mouse_capture = true;
            // Button-motion tracking includes dragging, without idle-motion reports.
            stdout.write_all(b"\x1b[?1002h\x1b[?1006h")?;
            stdout.flush()?;
        } else {
            execute!(stdout, DisableMouseCapture)?;
            self.mouse_capture = false;
        }
        Ok(())
    }

    fn enter() -> Result<Self> {
        let mut guard = Self {
            alternate_screen: false,
            raw_mode: false,
            mouse_capture: false,
        };

        terminal::enable_raw_mode().context("enable terminal raw mode")?;
        guard.raw_mode = true;

        guard.alternate_screen = true;
        execute!(io::stdout(), EnterAlternateScreen).context("enter terminal alternate screen")?;
        execute!(io::stdout(), Hide, Clear(ClearType::All))
            .context("initialize terminal screen")?;

        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();

        if self.mouse_capture {
            let _ = execute!(stdout, DisableMouseCapture);
        }

        if self.alternate_screen {
            let _ = execute!(
                stdout,
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

fn render(
    stdout: &mut impl Write,
    frame: &Frame,
    width: u16,
    height: u16,
    notice: Option<&str>,
) -> Result<()> {
    stdout
        .sync_update(|mut stdout| {
            let body_rows = height.saturating_sub(1) as usize;

            for row in 0..body_rows {
                queue!(
                    stdout,
                    MoveTo(0, row as u16),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                    Clear(ClearType::CurrentLine)
                )?;

                if let Some(frame_row) = frame.rows.get(row) {
                    render_row(&mut stdout, frame_row, width, &frame.styles)?;
                }
            }

            let on_prompt = frame
                .cursor
                .is_some_and(|(_, y)| y == frame.height.saturating_sub(1));
            let prompt = sanitize(&frame.prompt);
            let visible_prompt = clip_to_width(&prompt, width.saturating_sub(1) as usize).0;
            if height > 0 {
                queue!(
                    stdout,
                    MoveTo(0, height - 1),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                    Clear(ClearType::CurrentLine)
                )?;
                if on_prompt && notice.is_none() {
                    queue!(stdout, Print(&visible_prompt))?;
                } else {
                    let pending = clip_to_width(&prompt, width as usize).0;
                    let pending_width = UnicodeWidthStr::width(pending.as_str());
                    let message_width = if pending_width > 0 && notice.is_none() {
                        (width as usize).saturating_sub(pending_width + 1)
                    } else {
                        width as usize
                    };
                    queue!(
                        stdout,
                        Print(
                            clip_to_width(
                                &sanitize(notice.unwrap_or(&frame.status)),
                                message_width
                            )
                            .0
                        )
                    )?;
                    if pending_width > 0 && notice.is_none() {
                        queue!(
                            stdout,
                            MoveTo(width - pending_width as u16, height - 1),
                            Print(pending)
                        )?;
                    }
                }
            }

            if let Some((x, y)) = frame.cursor.filter(|_| notice.is_none()) {
                if width > 0 && height > 0 {
                    let x = x.min(width - 1);
                    let y = if on_prompt {
                        height - 1
                    } else {
                        y.min(height.saturating_sub(2))
                    };
                    let style = match frame.cursor_shape {
                        CursorShape::Block => SetCursorStyle::SteadyBlock,
                        CursorShape::Bar => SetCursorStyle::SteadyBar,
                        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
                    };
                    queue!(stdout, style, MoveTo(x, y), Show)?;
                } else {
                    queue!(stdout, Hide)?;
                }
            } else {
                queue!(stdout, Hide)?;
            }

            queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
            Ok(())
        })
        .context("flush terminal frame")?
}

fn render_row(
    stdout: &mut impl Write,
    row: &FrameRow,
    width: u16,
    styles: &std::collections::HashMap<String, zess_ui::TextStyle>,
) -> Result<()> {
    let mut remaining = width as usize;

    for span in &row.spans {
        if remaining == 0 {
            break;
        }

        let resolve = |role: &str| {
            styles
                .get(role)
                .cloned()
                .unwrap_or_else(|| style::resolve(role))
        };
        let base = span.base_style.as_deref().map(resolve).unwrap_or_default();
        style::apply(stdout, &base.patch(&resolve(&span.style)))?;
        let (text, cells, truncated) = clip_to_width(&sanitize(&span.text), remaining);
        queue!(stdout, Print(text))?;
        remaining -= cells;
        if truncated {
            break;
        }
    }

    queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    Ok(())
}

fn clip_to_width(text: &str, maximum: usize) -> (String, usize, bool) {
    let mut output = String::new();
    let mut cells = 0;

    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if cells + width > maximum {
            return (output, cells, true);
        }

        output.push_str(grapheme);
        cells += width;
    }

    (output, cells, false)
}

fn sanitize(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{1b}' => escaped.push_str("\\x1b"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{{{:x}}}", character as u32));
            }
            character => escaped.push(character),
        }
    }

    escaped
}
