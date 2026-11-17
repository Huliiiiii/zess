use super::*;

#[test]
fn an_authentication_broker_can_cancel_a_prompt_without_cancelling_the_connection() {
    let (send, receive) = mpsc::channel();
    let context = ConnectionContext {
        cancellation: CancellationToken::new(),
        progress: Arc::new(Mutex::new(None)),
        requests: send,
    };
    let cancellation = CancellationToken::new();
    let worker_context = context.clone();
    let worker_cancel = cancellation.clone();
    let worker = thread::spawn(move || {
        worker_context.authenticate_with_cancellation(
            AuthenticationPrompt {
                message: "Password".into(),
                kind: AuthenticationKind::Secret,
            },
            &worker_cancel,
        )
    });
    let request = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    cancellation.cancel();
    assert!(worker.join().unwrap().is_err());
    assert!(!request.active());
    context.cancellation().check().unwrap();
}

#[test]
fn progress_uses_actual_byte_counts_and_stays_within_narrow_unicode_rows() {
    let mut page = ConnectionPage::new("服务器 👩‍👩‍👧‍👦".into());
    page.progress = ConnectionProgress {
        message: "Downloading".into(),
        bytes: Some(ByteProgress {
            completed: 40,
            total: Some(100),
        }),
    };
    let frame = page.frame(12, 24, None);
    let lines = frame
        .rows
        .iter()
        .map(|row| row.spans[0].text.as_str())
        .collect::<Vec<_>>();
    assert!(lines.contains(&"[====      ]"));
    assert!(lines.contains(&"40 / 100 byt"));
    assert!(
        lines
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(*line) <= 12)
    );
    page.progress.bytes = Some(ByteProgress {
        completed: 12,
        total: None,
    });
    let frame = page.frame(80, 24, None);
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.spans[0].text.contains("12 bytes"))
    );
    assert!(!frame.rows.iter().any(|row| row.spans[0].text.contains('%')));
}

struct RecordedTerminal {
    output: Arc<Mutex<Vec<u8>>>,
    step: usize,
    polls: usize,
    cleanup_allowed: Arc<AtomicBool>,
}

struct RecordedWriter {
    output: Arc<Mutex<Vec<u8>>>,
    flushed: Arc<AtomicBool>,
}

impl std::io::Write for RecordedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.output.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flushed.store(true, Ordering::Release);
        Ok(())
    }
}

struct InteractiveTerminal {
    state: RecordedTerminal,
    writer: RecordedWriter,
}

impl crate::TerminalIo for InteractiveTerminal {
    type Writer = RecordedWriter;
    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }
    fn size(&self) -> Result<(u16, u16)> {
        Ok((80, 24))
    }
    fn mouse_capture(&self) -> bool {
        false
    }
    fn set_mouse_capture(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn poll_event(&mut self, _: Duration) -> Result<Option<Event>> {
        self.state.polls += 1;
        anyhow::ensure!(self.state.polls < 2000, "connection UI did not finish");
        thread::sleep(Duration::from_millis(1));
        let output = String::from_utf8(self.state.output.lock().unwrap().clone()).unwrap();
        let key = |code| {
            Some(Event::Key(crossterm::event::KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )))
        };
        let event = match self.state.step {
            0 if output.contains("Choose a session") => key(KeyCode::Down),
            1 => key(KeyCode::Enter),
            2 if output.contains("Key passphrase") => Some(Event::Paste("private-secret".into())),
            3 => key(KeyCode::Enter),
            4 if output.contains("Opening tunnel") => key(KeyCode::Esc),
            5 if output.contains("Cancelling; cleaning up") => {
                self.state.cleanup_allowed.store(true, Ordering::Release);
                None
            }
            _ => return Ok(None),
        };
        self.state.step += 1;
        Ok(event)
    }
}

#[test]
fn startup_presents_the_ui_before_connecting_and_keeps_it_until_cancelled_work_is_reclaimed() {
    struct Factory {
        output: Arc<Mutex<Vec<u8>>>,
        flushed: Arc<AtomicBool>,
        cleanup_allowed: Arc<AtomicBool>,
        cleaned: Arc<AtomicBool>,
    }
    impl ConnectionFactory for Factory {
        fn display_target(&self) -> &str {
            "remote workspace"
        }
        fn connect(&mut self, context: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
            assert!(self.flushed.load(Ordering::Acquire));
            assert!(
                String::from_utf8_lossy(&self.output.lock().unwrap())
                    .contains("Connecting to remote workspace")
            );
            context.progress(ConnectionProgress {
                message: "Downloading server".into(),
                bytes: Some(ByteProgress {
                    completed: 40,
                    total: Some(100),
                }),
            });
            let selected = context
                .choose_session(vec![
                    SessionCandidate {
                        id: SessionId("first".into()),
                        display: "first".into(),
                        workspace: "/one".into(),
                    },
                    SessionCandidate {
                        id: SessionId("second".into()),
                        display: "second".into(),
                        workspace: "/two".into(),
                    },
                ])?
                .unwrap();
            assert_eq!(selected, SessionId("second".into()));
            assert_eq!(
                context
                    .authenticate(AuthenticationPrompt {
                        message: "Key passphrase".into(),
                        kind: AuthenticationKind::Secret,
                    })?
                    .as_deref(),
                Some("private-secret")
            );
            context.progress(ConnectionProgress {
                message: "Opening tunnel".into(),
                bytes: None,
            });
            while context.cancellation().check().is_ok() {
                thread::sleep(Duration::from_millis(1));
            }
            while !self.cleanup_allowed.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            self.cleaned.store(true, Ordering::Release);
            anyhow::bail!("cancelled")
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let flushed = Arc::new(AtomicBool::new(false));
    let cleanup_allowed = Arc::new(AtomicBool::new(false));
    let cleaned = Arc::new(AtomicBool::new(false));
    let mut terminal = InteractiveTerminal {
        state: RecordedTerminal {
            output: output.clone(),
            step: 0,
            polls: 0,
            cleanup_allowed: cleanup_allowed.clone(),
        },
        writer: RecordedWriter {
            output: output.clone(),
            flushed: flushed.clone(),
        },
    };
    crate::run_connection_with_devices(
        Factory {
            output: output.clone(),
            flushed,
            cleanup_allowed,
            cleaned: cleaned.clone(),
        },
        &[],
        &mut terminal,
        &mut crate::Osc52Clipboard,
    )
    .unwrap();
    assert!(cleaned.load(Ordering::Acquire));
    let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(output.contains("40 / 100 bytes"));
    assert!(output.contains("second  /two"));
    assert!(output.contains("**************"));
    assert!(!output.contains("private-secret"));
}

#[test]
fn information_prompts_display_without_requesting_or_submitting_an_answer() {
    let (reply, receive) = mpsc::channel();
    let mut page = ConnectionPage::new("remote".into());
    page.interaction = Some(Interaction::Authentication {
        prompt: AuthenticationPrompt {
            message: "Touch your security key".into(),
            kind: AuthenticationKind::Information,
        },
        input: String::new(),
        reply,
        active: Arc::new(AtomicBool::new(true)),
    });
    let frame = page.frame(80, 24, None);
    assert!(!frame.status.contains("Enter"));
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.spans[0].text == "Touch your security key")
    );
    assert!(page.event(&Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE
    ))));
    assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(page.interaction.is_some());
}
