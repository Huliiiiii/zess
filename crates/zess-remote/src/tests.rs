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
