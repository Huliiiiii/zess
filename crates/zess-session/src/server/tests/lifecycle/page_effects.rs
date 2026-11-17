mod cache;

use serde_json::json;
use zess_plugin::{HostRequest, ItemSelection};
use zess_protocol::{
    CommandTarget, SessionSnapshot,
    resource::{PageCommandInput, PageId, PageInput, PageSource, ResourceRequest},
    workspace_view::{DocumentLayer, DocumentPane, WorkspaceView},
};

use super::*;

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    instance: ServerInstanceId,
    session: SessionId,
    epoch: u64,
}

impl Client {
    fn attach(server: &Server, client_id: &str) -> (Self, SessionSnapshot) {
        let (mut stream, mut reader) = server.connect();
        hello(&mut stream, &mut reader);
        framing::write(
            &mut stream,
            &ClientMessage::Attach {
                expected_session: None,
                client_id: client_id.into(),
                width: 80,
                height: 24,
                files: Vec::new(),
            },
        )
        .unwrap();
        let ServerMessage::Attached {
            server_instance,
            context_epoch,
            ..
        } = receive(&mut reader)
        else {
            panic!("attachment expected");
        };
        let ServerMessage::Snapshot {
            session_id, state, ..
        } = receive(&mut reader)
        else {
            panic!("initial workspace expected");
        };
        (
            Self {
                stream,
                reader,
                instance: server_instance,
                session: session_id,
                epoch: context_epoch,
            },
            state,
        )
    }

    fn send(&mut self, message: ClientMessage) {
        framing::write(&mut self.stream, &message).unwrap();
    }

    fn subscribe(
        &mut self,
        subscription_id: u64,
        page_id: PageId,
        kind: &str,
        path: &str,
    ) -> PageCommandInput {
        self.send(ClientMessage::Resource {
            request: ResourceRequest::Subscribe {
                subscription_id,
                page_id,
                clone_from: None,
                source: PageSource {
                    kind: kind.into(),
                    argument: json!({}),
                },
                input: PageInput {
                    generation: 0,
                    mode: "normal".into(),
                    selection: ItemSelection::default(),
                    register: None,
                    filter: None,
                    source_buffer: None,
                    width: 80,
                    active: true,
                    preview_width: 39,
                    preview_height: 22,
                },
            },
        });
        loop {
            if let ServerMessage::Resource { update } = receive(&mut self.reader)
                && update.subscription_id == subscription_id
                && update
                    .output
                    .display
                    .is_some_and(|display| display.rows.iter().any(|row| row.id == path))
            {
                break;
            }
        }
        PageCommandInput {
            generation: 0,
            selection: ItemSelection {
                primary: Some(path.into()),
                anchor: Some(path.into()),
                selected: vec![path.into()],
            },
            register: None,
        }
    }

    fn command(&mut self, id: u64, action: CommandAction) {
        self.send(ClientMessage::Command {
            request_id: RequestId(id),
            server_instance: self.instance.clone(),
            action,
        });
    }

    fn result(&mut self, id: u64) -> OperationResult {
        loop {
            if let ServerMessage::CommandResult {
                request_id, result, ..
            } = receive(&mut self.reader)
                && request_id == RequestId(id)
                && !matches!(result, OperationResult::Pending)
            {
                return result;
            }
        }
    }

    fn detach(mut self) {
        self.send(ClientMessage::Detach);
        self.reader.read_to_end(&mut Vec::new()).unwrap();
    }
}

#[test]
fn a_page_document_survives_lost_delivery_until_its_result_is_acknowledged() {
    for save_view in [false, true] {
        let server = Server::start("keep");
        fs::write(server.root.join("note.txt"), "page document\n").unwrap();
        let (mut first, initial) = Client::attach(&server, "client");
        let page_id = PageId {
            context_epoch: first.epoch,
            sequence: 1,
        };
        let input = first.subscribe(1, page_id, "files", "note.txt");
        first.command(
            1,
            CommandAction::Invoke {
                target: CommandTarget::Page {
                    subscription_id: 1,
                    input,
                },
                name: "files.edit-layer".into(),
                argument: json!({}),
            },
        );
        first.detach(); // Discard the original result without reading its document or acknowledging it.

        let (mut other, _) = Client::attach(&server, "other terminal");
        other.send(ClientMessage::QueryOperation {
            request_id: RequestId(1),
            server_instance: other.instance.clone(),
        });
        assert_eq!(other.result(1), OperationResult::Unknown);
        other.detach();

        let (mut client, state) = Client::attach(&server, "client");
        let OperationResult::Completed {
            outcome:
                CommandOutcome::PageApplied {
                    page_id: origin,
                    subscription_id: 1,
                    navigation,
                    document: Some(document),
                },
            ..
        } = client.result(1)
        else {
            panic!("expected the original page document");
        };
        assert_eq!(origin, page_id);
        assert!(navigation.is_empty());
        assert!(document.fullscreen);
        assert!(
            state
                .buffers
                .iter()
                .any(|buffer| buffer.id == document.buffer_id)
        );

        if save_view {
            client.send(ClientMessage::SaveWorkspaceView {
                view: WorkspaceView {
                    page_document: Some(DocumentLayer {
                        buffer_id: document.buffer_id,
                        previous: DocumentPane {
                            view_id: initial.focused_view.unwrap(),
                            buffer_id: initial.workspace.current.unwrap(),
                        },
                    }),
                    ..Default::default()
                },
            });
            loop {
                if matches!(
                    receive(&mut client.reader),
                    ServerMessage::WorkspaceViewSaved
                ) {
                    break;
                }
            }
        }
        client.send(ClientMessage::ResultAck {
            request_id: RequestId(1),
            server_instance: client.instance.clone(),
        });
        client.send(ClientMessage::QueryOperation {
            request_id: RequestId(1),
            server_instance: client.instance.clone(),
        });
        assert_eq!(client.result(1), OperationResult::Unknown);
        client.detach();

        let (_, after_ack) = Client::attach(&server, "client");
        assert_eq!(
            after_ack
                .buffers
                .iter()
                .any(|buffer| buffer.id == document.buffer_id),
            save_view
        );
    }
}

#[test]
fn a_disconnected_discard_confirmation_cannot_confirm_the_reconnected_page() {
    let server = Server::start("keep");
    let path = server.root.join("note.txt");
    fs::write(&path, "tracked\n").unwrap();
    for arguments in [
        &["init", "--quiet"][..],
        &["add", "note.txt"],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(arguments)
            .current_dir(&server.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::write(&path, "working\n").unwrap();
    let (mut first, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: first.epoch,
        sequence: 1,
    };
    let input = first.subscribe(1, page_id, "git", "file:note.txt");
    let discard = |subscription_id, input| CommandAction::Invoke {
        target: CommandTarget::Page {
            subscription_id,
            input,
        },
        name: "git.discard".into(),
        argument: json!({}),
    };
    let confirmation = |client: &mut Client, subscription_id| loop {
        if let ServerMessage::Resource { update } = receive(&mut client.reader)
            && update.subscription_id == subscription_id
            && let Some(prompt) =
                update
                    .output
                    .requests
                    .into_iter()
                    .find_map(|request| match request {
                        HostRequest::MiniBuffer {
                            purpose,
                            confirmation: Some(token),
                            ..
                        } => Some((purpose, token)),
                        _ => None,
                    })
        {
            break prompt;
        }
    };
    first.command(1, discard(1, input));
    let (purpose, expired) = confirmation(&mut first, 1);
    first.detach();

    let (mut client, _) = Client::attach(&server, "client");
    assert!(matches!(
        client.result(1),
        OperationResult::Completed {
            outcome: CommandOutcome::Failed { .. },
            ..
        }
    ));
    let input = client.subscribe(2, page_id, "git", "file:note.txt");
    let submit = |token| CommandAction::PagePrompt {
        subscription_id: 2,
        input: input.clone(),
        purpose: purpose.clone(),
        confirmation: Some(token),
        text: "yes".into(),
        cancelled: false,
    };
    client.command(2, submit(expired.clone()));
    assert!(matches!(client.result(2), OperationResult::Completed {
        outcome: CommandOutcome::Failed { message }, ..
    } if message.contains("confirmation expired")));
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");

    client.command(3, discard(2, input.clone()));
    let (fresh_purpose, fresh) = confirmation(&mut client, 2);
    assert_eq!(fresh_purpose, purpose);
    client.command(4, submit(expired));
    assert!(matches!(client.result(4), OperationResult::Completed {
        outcome: CommandOutcome::Failed { message }, ..
    } if message.contains("confirmation expired")));
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");

    client.command(5, submit(fresh));
    assert!(matches!(
        client.result(3),
        OperationResult::Completed {
            outcome: CommandOutcome::Applied,
            ..
        }
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "tracked\n");

    client.send(ClientMessage::Resync {
        session_id: client.session.clone(),
        state_revision: 0,
    });
    while !matches!(receive(&mut client.reader), ServerMessage::Snapshot { .. }) {}
    // The fresh discard completed only after the stale and fresh answers both received results.
    for expected in [1, 2, 4, 5, 3] {
        let ServerMessage::CommandResult {
            request_id, result, ..
        } = receive(&mut client.reader)
        else {
            panic!("resync must replay the completed operations");
        };
        assert_eq!(request_id, RequestId(expected));
        assert!(matches!(result, OperationResult::Completed { .. }));
    }
}
