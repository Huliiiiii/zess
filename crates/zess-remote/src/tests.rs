use std::io::{BufReader, Cursor};

use crate::{ProxyOperation, ProxyRequest, SshTarget, read_request, write_request};

#[test]
fn remote_workspace_names_are_preserved_as_data() {
    let workspace = "C:\\work tree\\quote'\";$(touch hacked)\\项目";
    let request = ProxyRequest::new(ProxyOperation::Attach {
        workspace: workspace.to_owned(),
        files: vec!["../file".to_owned()],
        config: None,
    });
    let mut bytes = vec![];
    write_request(&mut bytes, &request).unwrap();
    bytes.extend_from_slice(b"protocol bytes\n");
    let mut reader = BufReader::new(Cursor::new(bytes));
    let decoded: ProxyRequest = read_request(&mut reader).unwrap();
    let ProxyOperation::Attach {
        workspace: decoded, ..
    } = decoded.operation
    else {
        panic!("wrong request")
    };
    assert_eq!(decoded, workspace);
    let mut remainder = String::new();
    std::io::Read::read_to_string(&mut reader, &mut remainder).unwrap();
    assert_eq!(remainder, "protocol bytes\n");
}

#[test]
fn ssh_uses_system_configuration_without_tty_or_forwarding() {
    let mut target = SshTarget::new("my-server".to_owned());
    target.config_file = Some("config with spaces".into());
    target.identity_file = Some("identity with spaces".into());
    target.jump_host = Some("jump".to_owned());
    let spec = target.command("zess remote-proxy").unwrap();
    assert!(spec.env.is_empty());
    assert!(matches!(
        spec.console_mode,
        zess_platform::ConsoleMode::Inherit
    ));
    let args: Vec<_> = spec.args.iter().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "-T",
            "-o",
            "ClearAllForwardings=yes",
            "-i",
            "identity with spaces",
            "-F",
            "config with spaces",
            "-J",
            "jump",
            "--",
            "my-server",
            "zess remote-proxy"
        ]
    );
    assert!(
        SshTarget::new("-oProxyCommand=bad".to_owned())
            .command("ignored")
            .is_err()
    );
}

#[test]
fn interactive_ssh_uses_the_askpass_broker_without_a_console() {
    let mut target = SshTarget::new("host".into());
    target.askpass = Some(crate::AskpassConfig {
        program: "helper path".into(),
        endpoint: std::path::PathBuf::from("endpoint path").into(),
    });
    let spec = target.command("remote command").unwrap();
    assert!(matches!(
        spec.console_mode,
        zess_platform::ConsoleMode::Hidden
    ));
    let value = |name: &str| {
        spec.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string_lossy().into_owned())
    };
    assert_eq!(value("SSH_ASKPASS").as_deref(), Some("helper path"));
    assert_eq!(value("SSH_ASKPASS_REQUIRE").as_deref(), Some("force"));
    assert_eq!(
        value("ZESS_SSH_ASKPASS_ENDPOINT").as_deref(),
        Some("endpoint path")
    );
}
