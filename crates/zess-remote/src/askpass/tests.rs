use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use super::*;
use zess_ipc::NativeLocalIpc;

struct Answer;

impl SshPromptHandler for Answer {
    fn authenticate(&self, prompt: SshPrompt, _: &dyn Cancellation) -> Result<Option<String>> {
        match prompt.kind {
            SshPromptKind::Secret => {
                assert_eq!(prompt.message, "Password:\nsecond line");
                Ok(Some("secret".into()))
            }
            SshPromptKind::Confirmation => Ok(match prompt.message.as_str() {
                "yes" => Some("yes".into()),
                "no" => Some("no".into()),
                _ => None,
            }),
            _ => panic!("unexpected prompt"),
        }
    }
}

#[test]
fn prompts_and_rejections_pass_through_local_ipc() {
    let broker = AskpassBroker::start(
        Arc::new(NativeLocalIpc),
        PathBuf::from("zess"),
        CancellationToken::new(),
        Arc::new(Answer),
    )
    .unwrap();
    let endpoint = &broker.config().endpoint;
    assert_eq!(
        run_askpass(
            &NativeLocalIpc,
            endpoint,
            "Password:\nsecond line".into(),
            None
        )
        .unwrap()
        .as_deref(),
        Some("secret")
    );
    assert_eq!(
        run_askpass(&NativeLocalIpc, endpoint, "Confirm".into(), Some("confirm")).unwrap(),
        None
    );
    assert_eq!(
        run_askpass(&NativeLocalIpc, endpoint, "no".into(), Some("confirm")).unwrap(),
        None
    );
    assert_eq!(
        run_askpass(&NativeLocalIpc, endpoint, "yes".into(), Some("confirm"))
            .unwrap()
            .as_deref(),
        Some("")
    );
}

struct Waiting(mpsc::SyncSender<()>, mpsc::Sender<()>);

impl SshPromptHandler for Waiting {
    fn authenticate(
        &self,
        _: SshPrompt,
        cancellation: &dyn Cancellation,
    ) -> Result<Option<String>> {
        self.0.send(()).unwrap();
        while cancellation.check().is_ok() {
            thread::sleep(POLL_INTERVAL);
        }
        self.1.send(())?;
        Ok(None)
    }
}

#[test]
fn closing_the_broker_cancels_a_waiting_prompt_and_removes_its_endpoint() {
    let (started, waiting) = mpsc::sync_channel(1);
    let (stopped, finished) = mpsc::channel();
    let task = CancellationToken::new();
    let broker = AskpassBroker::start(
        Arc::new(NativeLocalIpc),
        PathBuf::from("zess"),
        task.clone(),
        Arc::new(Waiting(started, stopped)),
    )
    .unwrap();
    let endpoint = broker.config().endpoint.clone();
    let helper_endpoint = endpoint.clone();
    let helper = thread::spawn(move || {
        run_askpass(&NativeLocalIpc, &helper_endpoint, "Password".into(), None)
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(broker);
    finished.try_recv().unwrap();
    assert!(helper.join().unwrap().is_err());
    assert!(task.check().is_ok());
    assert!(!NativeLocalIpc.is_present(&endpoint).unwrap());
}

struct Notification {
    started: mpsc::SyncSender<()>,
    stopped: mpsc::SyncSender<()>,
}

impl SshPromptHandler for Notification {
    fn authenticate(
        &self,
        prompt: SshPrompt,
        cancellation: &dyn Cancellation,
    ) -> Result<Option<String>> {
        match prompt.kind {
            SshPromptKind::Information => {
                self.started.send(())?;
                while cancellation.check().is_ok() {
                    thread::sleep(POLL_INTERVAL);
                }
                self.stopped.send(())?;
                Ok(None)
            }
            SshPromptKind::Secret => Ok(Some("answer".into())),
            _ => panic!("unexpected prompt"),
        }
    }
}

#[test]
fn disconnected_notification_does_not_block_authentication() {
    let (started, notification_started) = mpsc::sync_channel(1);
    let (stopped, notification_stopped) = mpsc::sync_channel(1);
    let broker = AskpassBroker::start(
        Arc::new(NativeLocalIpc),
        PathBuf::from("zess"),
        CancellationToken::new(),
        Arc::new(Notification { started, stopped }),
    )
    .unwrap();
    let endpoint = &broker.config().endpoint;
    let mut notification = NativeLocalIpc.connect(endpoint).unwrap();
    framing::write(
        &mut notification,
        &SshPrompt {
            message: "Touch your key".into(),
            kind: SshPromptKind::Information,
        },
    )
    .unwrap();
    notification_started
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        run_askpass(&NativeLocalIpc, endpoint, "Password".into(), None)
            .unwrap()
            .as_deref(),
        Some("answer")
    );
    drop(notification);
    notification_stopped
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
}

#[test]
fn cancelling_the_connection_cancels_a_waiting_prompt() {
    let (started, waiting) = mpsc::sync_channel(1);
    let (stopped, _finished) = mpsc::channel();
    let task = CancellationToken::new();
    let broker = AskpassBroker::start(
        Arc::new(NativeLocalIpc),
        PathBuf::from("zess"),
        task.clone(),
        Arc::new(Waiting(started, stopped)),
    )
    .unwrap();
    let endpoint = broker.config().endpoint.clone();
    let helper =
        thread::spawn(move || run_askpass(&NativeLocalIpc, &endpoint, "Password".into(), None));
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    task.cancel();
    assert!(helper.join().unwrap().is_err());
}
