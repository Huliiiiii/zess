use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use zess_platform::{Cancellation, CancellationToken};
use zess_protocol::SessionId;
use zess_ui::{Frame, FrameRow, Span};

use crate::{ConnectionFactory, ConnectionStream, clip_to_width, sanitize};

#[cfg(test)]
mod tests;

pub struct ByteProgress {
    pub completed: u64,
    pub total: Option<u64>,
}

pub struct ConnectionProgress {
    pub message: String,
    pub bytes: Option<ByteProgress>,
}

pub struct SessionCandidate {
    pub id: SessionId,
    pub display: String,
    pub workspace: String,
}

pub enum AuthenticationKind {
    Secret,
    Visible,
    Confirmation,
    Information,
}

pub struct AuthenticationPrompt {
    pub message: String,
    pub kind: AuthenticationKind,
}

/// Reports connection work and requests input without taking ownership of the terminal.
#[derive(Clone)]
pub struct ConnectionContext {
    session_id: Option<SessionId>,
    cancellation: CancellationToken,
    progress: Arc<Mutex<Option<ConnectionProgress>>>,
    requests: Sender<Interaction>,
}

impl ConnectionContext {
    /// Identifies the session after the terminal has completed its first attachment.
    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn progress(&self, progress: ConnectionProgress) {
        *self.progress.lock().unwrap() = Some(progress);
    }

    pub fn choose_session(&self, sessions: Vec<SessionCandidate>) -> Result<Option<SessionId>> {
        let (reply, receive) = mpsc::channel();
        let active = Arc::new(AtomicBool::new(true));
        let _request = ActiveRequest(active.clone());
        self.requests.send(Interaction::Sessions {
            sessions,
            selected: 0,
            reply,
            active,
        })?;
        self.receive_reply(receive, &self.cancellation)
    }

    pub fn authenticate(&self, prompt: AuthenticationPrompt) -> Result<Option<String>> {
        self.authenticate_with_cancellation(prompt, &self.cancellation)
    }

    /// An authentication broker can also cancel its own outstanding prompt on shutdown.
    pub fn authenticate_with_cancellation(
        &self,
        prompt: AuthenticationPrompt,
        cancellation: &dyn Cancellation,
    ) -> Result<Option<String>> {
        let (reply, receive) = mpsc::channel();
        let active = Arc::new(AtomicBool::new(true));
        let _request = ActiveRequest(active.clone());
        self.requests.send(Interaction::Authentication {
            prompt,
            input: String::new(),
            reply,
            active,
        })?;
        self.receive_reply(receive, cancellation)
    }

    fn receive_reply<T>(&self, reply: Receiver<T>, cancellation: &dyn Cancellation) -> Result<T> {
        loop {
            self.cancellation.check()?;
            cancellation.check()?;
            match reply.recv_timeout(Duration::from_millis(20)) {
                Ok(value) => return Ok(value),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => anyhow::bail!("connection input was closed"),
            }
        }
    }
}

struct ActiveRequest(Arc<AtomicBool>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

enum Interaction {
    Sessions {
        sessions: Vec<SessionCandidate>,
        selected: usize,
        reply: Sender<Option<SessionId>>,
        active: Arc<AtomicBool>,
    },
    Authentication {
        prompt: AuthenticationPrompt,
        input: String,
        reply: Sender<Option<String>>,
        active: Arc<AtomicBool>,
    },
}

impl Interaction {
    fn active(&self) -> bool {
        let active = match self {
            Self::Sessions { active, .. } | Self::Authentication { active, .. } => active,
        };
        active.load(Ordering::Acquire)
    }
}

type ConnectionResult = (
    Box<dyn ConnectionFactory>,
    Result<Box<dyn ConnectionStream>>,
);

/// Owns one attempt, including cancellation and reclamation of its transport resources.
pub(crate) struct ConnectionTask {
    worker: Option<JoinHandle<ConnectionResult>>,
    context: ConnectionContext,
    requests: Receiver<Interaction>,
}

impl ConnectionTask {
    pub(crate) fn start(
        mut factory: Box<dyn ConnectionFactory>,
        session_id: Option<SessionId>,
    ) -> Result<Self> {
        let (send, requests) = mpsc::channel();
        let context = ConnectionContext {
            session_id,
            cancellation: CancellationToken::new(),
            progress: Arc::new(Mutex::new(None)),
            requests: send,
        };
        let worker_context = context.clone();
        let worker = thread::Builder::new()
            .name("zess-connect".into())
            .spawn(move || {
                let mut result = factory.connect(&worker_context);
                if let Err(error) = worker_context.cancellation.check() {
                    // Drop a transport completed concurrently with cancellation on this worker.
                    result = Err(error);
                }
                (factory, result)
            })
            .context("start connection worker")?;
        Ok(Self {
            worker: Some(worker),
            context,
            requests,
        })
    }

    pub(crate) fn update(&self, page: &mut ConnectionPage) {
        if let Some(progress) = self.context.progress.lock().unwrap().take() {
            page.progress = progress;
        }
        if page
            .interaction
            .as_ref()
            .is_some_and(|input| !input.active())
        {
            page.interaction = None;
        }
        while page.interaction.is_none() {
            let Ok(request) = self.requests.try_recv() else {
                break;
            };
            if request.active() && !page.cancelling {
                page.interaction = Some(request);
                page.scroll = 0;
            }
        }
    }

    pub(crate) fn cancel(&self) {
        self.context.cancellation.cancel();
    }

    pub(crate) fn finish(&mut self) -> Option<Result<ConnectionResult>> {
        if !self.worker.as_ref()?.is_finished() {
            return None;
        }
        Some(
            self.worker
                .take()
                .unwrap()
                .join()
                .map_err(|_| anyhow::anyhow!("connection worker panicked")),
        )
    }
}

impl Drop for ConnectionTask {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.context.cancellation.cancel();
            let _ = worker.join();
        }
    }
}

pub(crate) struct ConnectionPage {
    pub(crate) target: String,
    pub(crate) progress: ConnectionProgress,
    pub(crate) failed: bool,
    pub(crate) cancelling: bool,
    interaction: Option<Interaction>,
    animation: usize,
    scroll: usize,
}

impl ConnectionPage {
    pub(crate) fn new(target: String) -> Self {
        Self {
            target,
            progress: ConnectionProgress {
                message: "Connecting…".into(),
                bytes: None,
            },
            failed: false,
            cancelling: false,
            interaction: None,
            animation: 0,
            scroll: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.progress = ConnectionProgress {
            message: "Connecting…".into(),
            bytes: None,
        };
        self.failed = false;
        self.interaction = None;
        self.scroll = 0;
    }

    pub(crate) fn cancel(&mut self) {
        self.cancelling = true;
        self.interaction = None;
    }

    pub(crate) fn event(&mut self, event: &Event) -> bool {
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
        {
            match key.code {
                KeyCode::PageUp => {
                    self.scroll = self.scroll.saturating_sub(5);
                    return true;
                }
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_add(5);
                    return true;
                }
                _ => {}
            }
        }
        let Some(interaction) = &mut self.interaction else {
            return false;
        };
        if let Event::Paste(text) = event {
            if let Interaction::Authentication { input, prompt, .. } = interaction
                && matches!(
                    prompt.kind,
                    AuthenticationKind::Secret
                        | AuthenticationKind::Visible
                        | AuthenticationKind::Confirmation
                )
            {
                input.extend(text.chars().filter(|c| !c.is_control()));
            }
            return true;
        }
        let Event::Key(key) = event else { return false };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let finished = match interaction {
            Interaction::Sessions {
                sessions,
                selected,
                reply,
                ..
            } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        *selected = (*selected + 1).min(sessions.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        let _ =
                            reply.send(sessions.get(*selected).map(|session| session.id.clone()));
                        self.interaction = None;
                        return true;
                    }
                    _ => {}
                }
                false
            }
            Interaction::Authentication {
                prompt,
                input,
                reply,
                ..
            } => match key.code {
                KeyCode::Enter => {
                    let answer = match prompt.kind {
                        AuthenticationKind::Information => return true,
                        AuthenticationKind::Confirmation if input == "yes" || input == "no" => {
                            Some(std::mem::take(input))
                        }
                        AuthenticationKind::Confirmation => return true,
                        _ => Some(std::mem::take(input)),
                    };
                    let _ = reply.send(answer);
                    true
                }
                KeyCode::Backspace => {
                    if let Some((at, _)) = input.grapheme_indices(true).next_back() {
                        input.truncate(at);
                    }
                    false
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if !matches!(prompt.kind, AuthenticationKind::Information) {
                        input.push(character);
                    }
                    false
                }
                _ => false,
            },
        };
        if finished {
            self.interaction = None;
        }
        true
    }

    pub(crate) fn frame(&mut self, width: u16, height: u16, previous: Option<&Frame>) -> Frame {
        self.animation = self.animation.wrapping_add(1);
        let mut lines = vec![format!("Connecting to {}", self.target), String::new()];
        if self.cancelling {
            lines.push("Cancelling; cleaning up connection…".into());
        } else {
            lines.push(self.progress.message.clone());
            if let Some(bytes) = &self.progress.bytes {
                if let Some(total) = bytes.total.filter(|total| *total > 0) {
                    let cells = usize::from(width.saturating_sub(2)).min(40);
                    let filled = ((u128::from(bytes.completed.min(total)) * cells as u128)
                        / u128::from(total)) as usize;
                    lines.push(format!(
                        "[{}{}]",
                        "=".repeat(filled),
                        " ".repeat(cells - filled)
                    ));
                    lines.push(format!("{} / {} bytes", bytes.completed, total));
                } else {
                    lines.push(format!(
                        "{} {} bytes",
                        ["|", "/", "-", "\\"][self.animation / 8 % 4],
                        bytes.completed
                    ));
                }
            } else if !self.failed {
                lines.push(format!(
                    "[{}]",
                    ["   =", "  = ", " =  ", "=   "][self.animation / 8 % 4]
                ));
            }
        }
        let status = if self.cancelling {
            "Waiting for connection cleanup"
        } else if let Some(interaction) = &self.interaction {
            lines.push(String::new());
            match interaction {
                Interaction::Sessions {
                    sessions, selected, ..
                } => {
                    lines.push("Choose a session".into());
                    let visible = usize::from(height.saturating_sub(9)).max(1);
                    let start = selected.saturating_sub(visible.saturating_sub(1));
                    for (index, session) in sessions.iter().enumerate().skip(start).take(visible) {
                        lines.push(format!(
                            "{} {}  {}",
                            if index == *selected { ">" } else { " " },
                            session.display,
                            session.workspace
                        ));
                    }
                    "Up/Down select · Enter connect · Esc/Ctrl-c cancel"
                }
                Interaction::Authentication { prompt, input, .. } => {
                    lines.push(prompt.message.clone());
                    if !matches!(prompt.kind, AuthenticationKind::Information) {
                        lines.push(match prompt.kind {
                            AuthenticationKind::Secret => "*".repeat(input.graphemes(true).count()),
                            _ => input.clone(),
                        });
                    }
                    if matches!(prompt.kind, AuthenticationKind::Information) {
                        "Esc/Ctrl-c cancel"
                    } else if matches!(prompt.kind, AuthenticationKind::Confirmation) {
                        "Type yes or no · Enter submit · Esc/Ctrl-c cancel"
                    } else {
                        "Enter submit · Esc/Ctrl-c cancel"
                    }
                }
            }
        } else if self.failed {
            "r retry · Esc/Ctrl-c exit"
        } else {
            "Esc/Ctrl-c cancel"
        };
        let mut frame = previous.cloned().unwrap_or_default();
        frame.width = width;
        frame.height = height;
        frame.cursor = None;
        frame.prompt.clear();
        frame.status = status.into();
        let mut rows = Vec::new();
        for line in lines {
            for line in line.split('\n') {
                let mut remaining = sanitize(line);
                loop {
                    let (text, _, clipped) = clip_to_width(&remaining, usize::from(width));
                    let consumed = text.len();
                    rows.push(FrameRow {
                        spans: vec![Span {
                            base_style: None,
                            text,
                            style: String::new(),
                        }],
                    });
                    if !clipped || consumed == 0 {
                        break;
                    }
                    remaining.drain(..consumed);
                }
            }
        }
        let body_height = usize::from(height.saturating_sub(1));
        let scroll_limit = rows.len().saturating_sub(body_height);
        self.scroll = self.scroll.min(scroll_limit);
        if scroll_limit > 0 {
            frame.status = format!("PgUp/PgDn scroll · {}", frame.status);
        }
        let rows = rows
            .into_iter()
            .skip(self.scroll)
            .take(body_height)
            .collect::<Vec<_>>();
        // During reconnect keep the previous editor frame below the connection panel.
        if previous.is_some() {
            let replace = rows.len().min(frame.rows.len());
            frame.rows.splice(..replace, rows);
        } else {
            frame.rows = rows;
        }
        frame
    }
}
