use super::*;

struct Device {
    phase: Arc<Phase>,
    value: Arc<Mutex<String>>,
    calls: Arc<Mutex<Vec<String>>>,
    fail_write: bool,
}

struct ReadJob {
    phase: Arc<Phase>,
    ready_at: usize,
    text: Option<String>,
}
impl ClipboardReadJob for ReadJob {
    fn poll(&mut self) -> Option<Result<String>> {
        if self.phase.load(Ordering::SeqCst) >= self.ready_at {
            self.text.take().map(Ok)
        } else {
            None
        }
    }
}

struct WriteJob {
    phase: Arc<Phase>,
    ready_at: usize,
    value: Arc<Mutex<String>>,
    text: String,
    fail: bool,
}
impl ClipboardWriteJob for WriteJob {
    fn poll(&mut self) -> Option<Result<()>> {
        if self.phase.load(Ordering::SeqCst) >= self.ready_at {
            if self.fail {
                Some(Err(anyhow::anyhow!("native clipboard write failed")))
            } else {
                *self.value.lock().unwrap() = self.text.clone();
                Some(Ok(()))
            }
        } else {
            None
        }
    }
}
impl Clipboard for Device {
    type ReadJob = ReadJob;
    type WriteJob = WriteJob;
    fn read(&mut self, _: bool) -> Result<ReadJob> {
        let text = self.value.lock().unwrap().clone();
        self.calls.lock().unwrap().push(format!("read:{text}"));
        Ok(ReadJob {
            phase: self.phase.clone(),
            ready_at: self.phase.load(Ordering::SeqCst),
            text: Some(text),
        })
    }
    fn write(&mut self, _: bool, text: String, _: &mut impl Write) -> Result<Option<WriteJob>> {
        self.calls.lock().unwrap().push(format!("write:{text}"));
        Ok(Some(WriteJob {
            phase: self.phase.clone(),
            ready_at: self.phase.load(Ordering::SeqCst) + 2,
            value: self.value.clone(),
            text,
            fail: self.fail_write,
        }))
    }
}

fn device(phase: Arc<Phase>) -> Device {
    Device {
        phase,
        value: Arc::new(Mutex::new("old".into())),
        calls: Arc::default(),
        fail_write: false,
    }
}

#[test]
fn clipboard_replies_follow_received_writes_and_reads_in_order() {
    let phase = Arc::new(Phase::default());
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                0,
                vec![
                    attached(),
                    snapshot(1, SessionSnapshot::default()),
                    ServerMessage::ClipboardWrite {
                        text: "first".into(),
                    },
                    ServerMessage::ClipboardRead {
                        id: 7,
                        primary: false,
                    },
                    ServerMessage::ClipboardWrite {
                        text: "second".into(),
                    },
                ],
            ),
            (6, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut provider = device(phase.clone());
    let mut terminal = ScriptedTerminal {
        phase,
        events: std::iter::repeat_n(None, 12).collect(),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(
        ScriptedFactory(VecDeque::from([stream])),
        &[],
        &mut terminal,
        &mut provider,
    )
    .unwrap();
    assert_eq!(
        *provider.calls.lock().unwrap(),
        ["write:first", "read:first", "write:second"]
    );
    assert_eq!(*provider.value.lock().unwrap(), "second");
    assert!(messages(&output).iter().any(|message| matches!(message, ClientMessage::ClipboardResponse { id: 7, text: Some(text), error: None } if text == "first")));
}
