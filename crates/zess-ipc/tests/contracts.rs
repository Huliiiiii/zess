use std::{
    fs,
    io::{self, Cursor, Read},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
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
    assert!(!fixture.socket.as_path().exists());
}

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
