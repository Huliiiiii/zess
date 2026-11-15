use std::{
    fs,
    io::{self, Cursor, Read, Write},
    net::Shutdown,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use zess_ipc::{Listener, ServerId, SocketPath, Stream, framing};

struct SocketFixture {
    root: PathBuf,
    socket: SocketPath,
}

impl SocketFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zess-ipc-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let socket = SocketPath::from(root.join("server.sock"));
        Self { root, socket }
    }
}

impl Drop for SocketFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn server_ids_require_sixteen_hexadecimal_digits() {
    let id = "0000ABCDEF123456".parse::<ServerId>().unwrap();
    assert_eq!(id.to_string(), "0000abcdef123456");
    for invalid in [
        "",
        "123",
        "00000000000000000",
        "000000000000000g",
        "../0000000000000",
    ] {
        assert!(invalid.parse::<ServerId>().is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn fragmented_messages_are_emitted_in_order_after_their_delimiters() {
    let mut decoder = framing::Decoder::default();
    decoder.push(b"{\"value\":", 1024).unwrap();
    assert!(decoder.decode_next::<Value>().unwrap().is_none());
    decoder.push(b"1}", 1024).unwrap();
    assert!(decoder.decode_next::<Value>().unwrap().is_none());
    decoder.push(b"\n{\"value\":2}\n{", 1024).unwrap();
    assert_eq!(
        decoder.decode_next::<Value>().unwrap(),
        Some(json!({"value": 1}))
    );
    assert_eq!(
        decoder.decode_next::<Value>().unwrap(),
        Some(json!({"value": 2}))
    );
    assert!(decoder.decode_next::<Value>().unwrap().is_none());
    decoder.push(b"\"value\":3}\n", 1024).unwrap();
    assert_eq!(
        decoder.decode_next::<Value>().unwrap(),
        Some(json!({"value": 3}))
    );
}

#[test]
fn message_limits_include_the_delimiter() {
    let mut reader = Cursor::new(b"null\ntrue\n");
    assert_eq!(framing::read(&mut reader, 5).unwrap().unwrap(), b"null\n");
    assert_eq!(framing::read(&mut reader, 5).unwrap().unwrap(), b"true\n");
    assert!(framing::read(&mut reader, 5).unwrap().is_none());
    let error = framing::read(&mut Cursor::new(b"null\n"), 4).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn messages_are_encoded_as_json_followed_by_one_delimiter() {
    assert_eq!(
        framing::encode(&json!({"value": "a\nb"})).unwrap(),
        b"{\"value\":\"a\\nb\"}\n"
    );
}

#[test]
fn failed_binding_preserves_the_existing_listener() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    assert!(Listener::bind(&fixture.socket).is_err());
    assert!(Stream::connect(&fixture.socket).is_ok());
    assert!(listener.accept().is_ok());
    drop(listener);
    assert!(!fixture.socket.is_present().unwrap());
}

#[cfg(unix)]
#[test]
fn dropping_a_listener_preserves_a_replacement_at_the_same_path() {
    let fixture = SocketFixture::new();
    let original = Listener::bind(&fixture.socket).unwrap();
    fs::remove_file(fixture.socket.as_path()).unwrap();
    let replacement = Listener::bind(&fixture.socket).unwrap();

    drop(original);

    let _client = Stream::connect(&fixture.socket).unwrap();
    assert!(replacement.accept().is_ok());
}

#[cfg(unix)]
#[test]
fn binding_does_not_replace_a_regular_file() {
    let fixture = SocketFixture::new();
    fs::write(fixture.socket.as_path(), b"keep me").unwrap();
    assert!(Listener::bind(&fixture.socket).is_err());
    let mut contents = String::new();
    fs::File::open(fixture.socket.as_path())
        .unwrap()
        .read_to_string(&mut contents)
        .unwrap();
    assert_eq!(contents, "keep me");
}

#[test]
fn cloned_streams_share_the_connection_and_preserve_buffered_data_before_eof() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    let client = Stream::connect(&fixture.socket).unwrap();
    let mut server = listener.accept().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut writer = client.try_clone().unwrap();
    writer.write_all(b"first").unwrap();
    writer.write_all(b"second").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut received = Vec::new();
    server.read_to_end(&mut received).unwrap();
    assert_eq!(received, b"firstsecond");
    assert_eq!(
        writer.write(b"closed").unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    server.write_all(b"response").unwrap();
    let mut client = client;
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reply = [0; 8];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"response");
}

#[test]
fn polling_an_empty_connection_does_not_consume_the_next_message() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut client = Stream::connect(&fixture.socket).unwrap();
    let mut server = listener.accept().unwrap();
    server.set_nonblocking(true).unwrap();
    let mut bytes = [0; 7];
    for _ in 0..20 {
        assert_eq!(
            server.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    client.write_all(b"message").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match server.read(&mut bytes) {
            Ok(count) => {
                assert_eq!(&bytes[..count], b"message");
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            result => panic!("unexpected read: {result:?}"),
        }
    }
    assert_eq!(
        server.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn a_read_timeout_leaves_the_connection_usable() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    let mut client = Stream::connect(&fixture.socket).unwrap();
    let mut server = listener.accept().unwrap();
    server
        .set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let mut bytes = [0; 5];
    let error = server.read(&mut bytes).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    client.write_all(b"after").unwrap();
    server.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"after");
}

#[test]
fn polling_writes_deliver_large_messages_once_and_in_order() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    let mut client = Stream::connect(&fixture.socket).unwrap();
    let mut server = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    let expected = (0..4 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let mut written = 0;
    loop {
        match client.write(&expected[written..]) {
            Ok(count) => {
                assert_ne!(count, 0);
                written += count;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            result => panic!("unexpected write: {result:?}"),
        }
        assert!(
            written < expected.len(),
            "test payload must exceed the pipe capacity"
        );
    }
    let received = thread::spawn(move || {
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut received = Vec::new();
        server.read_to_end(&mut received).unwrap();
        received
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while written < expected.len() {
        match client.write(&expected[written..]) {
            Ok(count) => {
                assert_ne!(count, 0);
                written += count;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::yield_now(),
            result => panic!("unexpected write: {result:?}"),
        }
        assert!(Instant::now() < deadline);
    }
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(received.join().unwrap(), expected);
}

#[test]
fn shutting_down_a_clone_wakes_a_blocked_reader() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    let _client = Stream::connect(&fixture.socket).unwrap();
    let server = listener.accept().unwrap();
    let mut reader = server.try_clone().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reading = thread::spawn(move || {
        let result = reader.read(&mut [0; 1]);
        send.send(result).unwrap();
    });
    server.shutdown(Shutdown::Both).unwrap();
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        0
    );
    reading.join().unwrap();
}

#[test]
fn shutting_down_a_clone_wakes_a_blocked_writer() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    let mut client = Stream::connect(&fixture.socket).unwrap();
    let _server = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    let bytes = vec![0; 65536];
    loop {
        match client.write(&bytes) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            result => panic!("unexpected write: {result:?}"),
        }
    }
    client.set_nonblocking(false).unwrap();
    let mut writer = client.try_clone().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let writing = thread::spawn(move || send.send(writer.write_all(&bytes)).unwrap());
    client.shutdown(Shutdown::Both).unwrap();
    assert!(
        receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    writing.join().unwrap();
}

#[test]
fn discovery_follows_the_listener_lifetime() {
    let fixture = SocketFixture::new();
    #[cfg(unix)]
    if std::env::var_os("ZESS_IPC_DISCOVERY_CHILD").is_none() {
        // Isolate the runtime directory without mutating this multithreaded
        // process's environment or using the user's live server directory.
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery_follows_the_listener_lifetime",
                "--nocapture",
            ])
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .env("ZESS_IPC_DISCOVERY_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let endpoint = SocketPath::for_workspace(&fixture.root).unwrap();
    let listener = Listener::bind(&endpoint).unwrap();
    assert!(endpoint.is_present().unwrap());
    assert!(SocketPath::discover().unwrap().contains(&endpoint));
    drop(listener);
    assert!(!endpoint.is_present().unwrap());
    assert!(!SocketPath::discover().unwrap().contains(&endpoint));
}

#[test]
fn a_client_that_closes_before_accept_does_not_break_the_listener() {
    let fixture = SocketFixture::new();
    let listener = Listener::bind(&fixture.socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    drop(Stream::connect(&fixture.socket).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok(stream) => drop(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => panic!("unexpected accept: {:?}", result.err()),
        }
        assert!(Instant::now() < deadline);
    }
    let mut client = Stream::connect(&fixture.socket).unwrap();
    client.write_all(b"next").unwrap();
    let mut accepted = loop {
        match listener.accept() {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            result => panic!("unexpected accept: {:?}", result.err()),
        }
        assert!(Instant::now() < deadline);
    };
    accepted
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = [0; 4];
    accepted.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"next");
}
