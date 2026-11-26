mod clipboard;
mod lifecycle;

use std::fs;
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};

use zess_ipc::Listener;

use super::*;

fn stream_pair() -> (Stream, Stream) {
    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
    let socket = SocketPath::from(std::env::temp_dir().join(format!(
        "zess-session-ipc-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    )));
    let listener = Listener::bind(&socket).unwrap();
    let client = Stream::connect(&socket).unwrap();
    let server = listener.accept().unwrap();
    (server, client)
}

#[test]
fn unacknowledged_results_are_replayed_in_completion_order() {
    let mut operations = OperationTable::default();
    let client = operations.clients.entry("client".into()).or_default();
    for request_id in [RequestId(1), RequestId(2)] {
        client.unacknowledged.insert(
            request_id,
            Operation {
                action: CommandAction::CommandLine {
                    text: String::new(),
                },
                completed: None,
            },
        );
    }
    operations
        .complete("client:2", CommandOutcome::Applied, 1)
        .unwrap();
    operations
        .complete("client:1", CommandOutcome::Applied, 1)
        .unwrap();

    let (server, peer) = stream_pair();
    let mut connection = Connection::new(server).unwrap();
    operations
        .resend(
            "client",
            &ServerInstanceId("instance".into()),
            &mut connection,
        )
        .unwrap();
    connection.flush().unwrap();
    let mut reader = BufReader::new(peer);
    for expected in [RequestId(2), RequestId(1)] {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        assert!(
            matches!(serde_json::from_str::<ServerMessage>(&line).unwrap(), ServerMessage::CommandResult {
            request_id, result: OperationResult::Completed { outcome: CommandOutcome::Applied, .. }, ..
        } if request_id == expected)
        );
    }
}

#[test]
fn a_limit_sized_input_and_the_next_message_can_share_a_socket_read() {
    for eof in [false, true] {
        let (server, mut client) = stream_pair();
        let mut connection = Connection::new(server).unwrap();
        let envelope = framing::encode(&ClientMessage::Command {
            request_id: RequestId(1),
            server_instance: ServerInstanceId("instance".into()),
            action: CommandAction::Paste {
                text: String::new(),
            },
        })
        .unwrap()
        .len();
        let text = "x".repeat(MAX_INPUT_BYTES - envelope);
        let message = framing::encode(&ClientMessage::Command {
            request_id: RequestId(1),
            server_instance: ServerInstanceId("instance".into()),
            action: CommandAction::Paste { text: text.clone() },
        })
        .unwrap();
        assert_eq!(message.len(), MAX_INPUT_BYTES);
        // Earlier reads retained the prefix; the tail and the next frame arrive together.
        let split = message.len() - 100;
        connection
            .input
            .push(&message[..split], MAX_INPUT_BYTES)
            .unwrap();
        let mut tail = message[split..].to_vec();
        tail.extend(
            framing::encode(&ClientMessage::Resize {
                width: 100,
                height: 31,
            })
            .unwrap(),
        );
        client.write_all(&tail).unwrap();
        if eof {
            client.shutdown(Shutdown::Write).unwrap();
        }

        let messages = connection.receive().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], ClientMessage::Command {
            request_id, server_instance, action: CommandAction::Paste { text: actual },
        } if *request_id == RequestId(1) && server_instance.0 == "instance" && actual == &text));
        assert!(matches!(
            &messages[1],
            ClientMessage::Resize {
                width: 100,
                height: 31
            }
        ));
        assert_eq!(connection.eof, eof);
    }
}

#[test]
fn an_input_message_over_the_limit_is_rejected() {
    let (server, mut client) = stream_pair();
    let mut connection = Connection::new(server).unwrap();
    let message = framing::encode(&ClientMessage::Command {
        request_id: RequestId(1),
        server_instance: ServerInstanceId("instance".into()),
        action: CommandAction::Paste {
            text: "x".repeat(MAX_INPUT_BYTES),
        },
    })
    .unwrap();
    connection
        .input
        .push(&message[..MAX_INPUT_BYTES], MAX_INPUT_BYTES)
        .unwrap();
    client.write_all(&message[MAX_INPUT_BYTES..]).unwrap();

    let error = match connection.receive() {
        Ok(_) => panic!("oversized input was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "input message exceeds limit");
}

fn receive_in_bounded_frames(message: &ServerMessage) -> ServerMessage {
    let (stream, _client) = stream_pair();
    let mut connection = Connection::new(stream).unwrap();
    connection.send(message).unwrap();
    assert!(connection.output.len() > 1);
    let mut stream = zess_protocol::chunks::MessageStream::default();
    for frame in connection.output {
        assert!(frame.len() <= MAX_MESSAGE_BYTES);
        let ServerMessage::Chunk { json, last } = serde_json::from_slice(&frame).unwrap() else {
            panic!("expected a message fragment");
        };
        let received = stream.server(json, last).unwrap();
        if last {
            return received.expect("missing message");
        }
        assert!(received.is_none());
    }
    panic!("missing end of message")
}

#[test]
fn a_large_resource_update_arrives_in_bounded_frames() {
    let original = "\"界".repeat(MAX_MESSAGE_BYTES / 5);
    let update = zess_protocol::resource::ResourceUpdate {
        cache: Default::default(),
        kind: Default::default(),
        subscription_id: 8,
        generation: 12,
        document_scoped: false,
        output: zess_plugin::PluginOutput {
            requests: vec![zess_plugin::HostRequest::Message {
                text: original.clone(),
            }],
            ..Default::default()
        },
        metadata: None,
        patch_revision: None,
        document: None,
    };
    let ServerMessage::Resource { update: received } =
        receive_in_bounded_frames(&ServerMessage::Resource { update })
    else {
        panic!("missing resource update");
    };
    assert_eq!((received.subscription_id, received.generation), (8, 12));
    assert!(
        matches!(&received.output.requests[..], [zess_plugin::HostRequest::Message {text}] if text == &original)
    );
}

#[test]
fn a_large_yank_result_arrives_in_bounded_frames() {
    let instance = ServerInstanceId("instance".into());
    let value = "\"界".repeat(MAX_MESSAGE_BYTES / 5);
    let result = OperationResult::Completed {
        outcome: CommandOutcome::Yanked {
            values: vec![value],
            register: 'a',
        },
        state_revision: 3,
    };
    let received = receive_in_bounded_frames(&ServerMessage::CommandResult {
        request_id: RequestId(7),
        server_instance: instance.clone(),
        result: result.clone(),
    });
    assert!(matches!(
        received,
        ServerMessage::CommandResult {
            request_id: RequestId(7),
            server_instance,
            result: received,
        } if server_instance == instance && received == result
    ));
}

#[test]
fn oversized_clipboard_writes_leave_the_connection_usable() {
    let (server, client) = stream_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut connection = Connection::new(server).unwrap();
    // Each NUL is six bytes in JSON, so the envelope crosses the limit
    // even though the selected text itself is smaller than the limit.
    for message in [ServerMessage::ClipboardWrite {
        text: "\0".repeat(MAX_MESSAGE_BYTES / 6),
    }] {
        let error = connection.send(&message).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("selection exceeds the clipboard message limit ({MAX_MESSAGE_BYTES} bytes)")
        );
    }
    connection.send(&ServerMessage::Exited).unwrap();
    connection.flush().unwrap();
    connection.stream.shutdown(Shutdown::Write).unwrap();
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&line).unwrap(),
        ServerMessage::Exited
    ));
}

#[test]
fn messages_arrive_in_order_after_a_partial_write() {
    let (server, client) = stream_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut connection = Connection::new(server).unwrap();
    let original = "x".repeat(8 * 1024 * 1024);
    connection
        .send(&ServerMessage::ClipboardWrite {
            text: original.clone(),
        })
        .unwrap();
    connection.flush().unwrap();
    assert!(connection.written > 0, "socket must block before reading");
    connection
        .send(&ServerMessage::ClipboardRead {
            id: 1,
            primary: false,
        })
        .unwrap();
    connection
        .send(&ServerMessage::ClipboardWrite {
            text: "next".into(),
        })
        .unwrap();
    let reader = thread::spawn(move || {
        BufReader::new(client)
            .lines()
            .map(|line| serde_json::from_str::<ServerMessage>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !connection.output.is_empty() {
        connection.flush().unwrap();
        assert!(Instant::now() < deadline, "client did not drain the output");
        thread::yield_now();
    }
    connection.stream.shutdown(Shutdown::Write).unwrap();
    let messages = reader.join().unwrap();
    assert_eq!(messages.len(), 3);
    assert!(matches!(&messages[0], ServerMessage::ClipboardWrite { text } if text == &original));
    assert!(matches!(
        &messages[1],
        ServerMessage::ClipboardRead {
            id: 1,
            primary: false
        }
    ));
    assert!(matches!(&messages[2], ServerMessage::ClipboardWrite { text } if text == "next"));
}

#[test]
fn shutdown_delivers_exited_after_socket_backpressure_clears() {
    let (server, client) = stream_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut connection = Connection::new(server).unwrap();
    connection
        .send(&ServerMessage::ClipboardWrite {
            text: "x".repeat(8 * 1024 * 1024),
        })
        .unwrap();
    connection.flush().unwrap();
    assert!(
        !connection.output.is_empty(),
        "the peer must exert backpressure"
    );
    connection.send(&ServerMessage::Exited).unwrap();
    connection.close_after_flush();
    let mut connections = vec![connection];
    let mut state = ServerState::Draining {
        deadline: Instant::now() + Duration::from_secs(5),
    };
    state.drain(&mut connections);
    assert!(matches!(state, ServerState::Draining { .. }));
    let reader = thread::spawn(move || {
        BufReader::new(client)
            .lines()
            .map(|line| serde_json::from_str::<ServerMessage>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(state, ServerState::Stopped) {
        state.drain(&mut connections);
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let messages = reader.join().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[1], ServerMessage::Exited));
}

#[test]
fn shutdown_stops_waiting_for_a_client_that_does_not_drain_output() {
    let (server, _client) = stream_pair();
    let mut connection = Connection::new(server).unwrap();
    connection
        .send(&ServerMessage::ClipboardWrite {
            text: "x".repeat(8 * 1024 * 1024),
        })
        .unwrap();
    connection.send(&ServerMessage::Exited).unwrap();
    let mut connections = vec![connection];
    let mut state = ServerState::Draining {
        deadline: Instant::now(),
    };
    state.drain(&mut connections);
    assert!(matches!(state, ServerState::Stopped));
    assert!(connections.is_empty());
}

#[test]
fn disconnecting_a_rejected_attached_client_releases_the_attachment() {
    let (server, peer) = stream_pair();
    let mut connection = Connection::new(server).unwrap();
    connection.role = ConnectionRole::Attached {
        client: "rejected".into(),
    };
    connection
        .send(&ServerMessage::Rejected {
            reason: "input context changed".into(),
        })
        .unwrap();
    connection.close_after_flush();
    drop(peer);
    assert!(connection.flush().is_err());
    let root = std::env::temp_dir().join(format!(
        "zess-rejected-client-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let mut session = Session::new(root.clone(), &[], None).unwrap();
    let mut active = Some("rejected".to_owned());
    connection.disconnect(&mut session, &mut active, None, |_| false);
    assert_eq!(active, None);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reconnecting_recovers_unacknowledged_results_without_repeating_commands() {
    struct IdentityPlatform;
    impl Platform for IdentityPlatform {
        fn host_info(&self) -> zess_platform::HostInfo {
            panic!("unexpected host information request")
        }
        fn user_dirs(&self) -> Result<zess_platform::UserDirs> {
            panic!("unexpected user directory request")
        }
        fn current_dir(&self) -> std::io::Result<std::path::PathBuf> {
            panic!("unexpected current directory request")
        }
        fn current_exe(&self) -> std::io::Result<std::path::PathBuf> {
            panic!("unexpected executable request")
        }
        fn absolute_path(&self, path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
            NativePlatform.absolute_path(path)
        }
        fn process_id(&self) -> u32 {
            4242
        }
        fn system_time(&self) -> std::time::SystemTime {
            std::time::UNIX_EPOCH + Duration::from_nanos(9000)
        }
    }
    struct RunningServer {
        root: std::path::PathBuf,
        socket: std::path::PathBuf,
        thread: Option<thread::JoinHandle<Result<()>>>,
    }
    impl Drop for RunningServer {
        fn drop(&mut self) {
            if let Ok(mut stream) = UnixStream::connect(&self.socket) {
                let _ = framing::write(
                    &mut stream,
                    &ClientMessage::Hello {
                        version: VersionInfo::new(zess_build::version()),
                    },
                );
                let _ = framing::write(&mut stream, &ClientMessage::Stop { force: true });
            }
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap().unwrap();
            }
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn send(stream: &mut UnixStream, message: &ClientMessage) {
        if matches!(
            message,
            ClientMessage::Attach { .. } | ClientMessage::Status | ClientMessage::Stop { .. }
        ) {
            framing::write(
                stream,
                &ClientMessage::Hello {
                    version: VersionInfo::new(zess_build::version()),
                },
            )
            .unwrap();
        }
        serde_json::to_writer(&mut *stream, message).unwrap();
        stream.write_all(b"\n").unwrap();
    }
    fn receive(reader: &mut BufReader<UnixStream>) -> ServerMessage {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let message = serde_json::from_str(&line).unwrap();
        if let ServerMessage::Hello { version } = message {
            assert_eq!(version, VersionInfo::new(zess_build::version()));
            receive(reader)
        } else {
            message
        }
    }
    fn receive_result(reader: &mut BufReader<UnixStream>, expected: RequestId) -> OperationResult {
        loop {
            if let ServerMessage::CommandResult {
                request_id, result, ..
            } = receive(reader)
            {
                assert_eq!(request_id, expected);
                return result;
            }
        }
    }
    fn attach(
        socket: &std::path::Path,
    ) -> (
        UnixStream,
        BufReader<UnixStream>,
        ServerInstanceId,
        SessionId,
        SessionSnapshot,
    ) {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        send(
            &mut stream,
            &ClientMessage::Attach {
                expected_session: None,
                client_id: "command-reconnect".into(),
                width: 80,
                height: 24,
                files: Vec::new(),
            },
        );
        assert!(
            matches!(receive(&mut reader), ServerMessage::Attached { server_instance, context_epoch: 9000, .. } if server_instance.0 == "4242-9000")
        );
        let ServerMessage::Snapshot {
            server_instance,
            session_id,
            state,
            ..
        } = receive(&mut reader)
        else {
            panic!("attach must provide a semantic snapshot")
        };
        (stream, reader, server_instance, session_id, state)
    }
    let root = std::env::temp_dir().join(format!(
        "zess-command-reconnect-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let socket = root.join("server.sock");
    let workspace = root.clone();
    let server_socket = socket.clone();
    let handle = thread::spawn(move || {
        serve_with_platform(
            &zess_ipc::NativeLocalIpc,
            &SocketPath::from(server_socket),
            Session::new(workspace, &[], None)?,
            &IdentityPlatform,
            SessionLifetime::Retained,
        )
    });
    let server = RunningServer {
        root,
        socket,
        thread: Some(handle),
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while !server.socket.exists() {
        assert!(Instant::now() < deadline);
        assert!(!server.thread.as_ref().unwrap().is_finished());
        thread::sleep(Duration::from_millis(10));
    }
    let mut status_stream = UnixStream::connect(&server.socket).unwrap();
    status_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(&mut status_stream, &ClientMessage::Status);
    assert!(
        matches!(receive(&mut BufReader::new(status_stream)), ServerMessage::Status { pid: 4242, server_instance, .. } if server_instance.0 == "4242-9000")
    );
    let (mut stream, mut reader, instance, session_id, before) = attach(&server.socket);
    assert_eq!(instance.0, "4242-9000");
    let request_id = RequestId(3);
    let target = before
        .views
        .iter()
        .find(|view| Some(view.view_id) == before.focused_view)
        .unwrap();
    let command = ClientMessage::Command {
        request_id,
        server_instance: instance.clone(),
        action: CommandAction::Invoke {
            target: zess_protocol::CommandTarget::View {
                view_id: target.view_id,
                buffer_id: target.buffer_id,
            },
            name: "new".into(),
            argument: serde_json::json!({}),
        },
    };
    send(&mut stream, &command);
    let original_result = receive_result(&mut reader, request_id);
    assert!(matches!(
        original_result,
        OperationResult::Completed {
            outcome: CommandOutcome::Applied,
            ..
        }
    ));

    let newer = RequestId(5);
    send(
        &mut stream,
        &ClientMessage::Command {
            request_id: newer,
            server_instance: instance.clone(),
            action: CommandAction::CommandLine { text: "new".into() },
        },
    );
    assert!(matches!(
        receive_result(&mut reader, newer),
        OperationResult::Completed {
            outcome: CommandOutcome::Applied,
            ..
        }
    ));
    send(
        &mut stream,
        &ClientMessage::ResultAck {
            request_id: newer,
            server_instance: instance.clone(),
        },
    );
    send(
        &mut stream,
        &ClientMessage::ResultAck {
            request_id,
            server_instance: ServerInstanceId("another-instance".into()),
        },
    );
    send(&mut stream, &ClientMessage::Detach);
    let mut remaining = String::new();
    while reader.read_line(&mut remaining).unwrap() != 0 {
        remaining.clear();
    }
    drop(reader);
    drop(stream);
    let (mut stream, mut reader, reconnected_instance, reconnected_session, after) =
        attach(&server.socket);
    assert_eq!(reconnected_instance, instance);
    assert_eq!(reconnected_session, session_id);
    assert_ne!(after.views[0].buffer_id, before.views[0].buffer_id);
    assert!(
        matches!(receive(&mut reader), ServerMessage::CommandResult { request_id: received, result, .. }
        if received == request_id && result == original_result)
    );
    send(&mut stream, &command);
    assert!(
        matches!(receive(&mut reader), ServerMessage::CommandResult { request_id: received, result, .. }
        if received == request_id && result == original_result)
    );

    for request_id in [request_id, request_id, RequestId(7)] {
        send(
            &mut stream,
            &ClientMessage::ResultAck {
                request_id,
                server_instance: instance.clone(),
            },
        );
    }
    send(
        &mut stream,
        &ClientMessage::QueryOperation {
            request_id,
            server_instance: instance.clone(),
        },
    );
    assert!(
        matches!(receive(&mut reader), ServerMessage::CommandResult {
        request_id: received, result: OperationResult::Unknown, ..
    } if received == request_id)
    );

    send(&mut stream, &command);
    let skipped = RequestId(2);
    send(
        &mut stream,
        &ClientMessage::Command {
            request_id: skipped,
            server_instance: instance.clone(),
            action: CommandAction::CommandLine { text: "new".into() },
        },
    );
    for retired in [request_id, skipped] {
        assert!(
            matches!(receive(&mut reader), ServerMessage::CommandResult {
            request_id: received, result: OperationResult::Unknown, ..
        } if received == retired)
        );
    }
    send(
        &mut stream,
        &ClientMessage::Resync {
            session_id,
            state_revision: 0,
        },
    );
    let ServerMessage::Snapshot { state, .. } = receive(&mut reader) else {
        panic!("expected resync snapshot")
    };
    assert_eq!(
        state, after,
        "retired request IDs must not create another buffer"
    );
    send(
        &mut stream,
        &ClientMessage::Command {
            request_id: RequestId(6),
            server_instance: instance,
            action: CommandAction::CommandLine { text: "new".into() },
        },
    );
    assert!(matches!(
        receive_result(&mut reader, RequestId(6)),
        OperationResult::Completed {
            outcome: CommandOutcome::Applied,
            ..
        }
    ));
}
