use super::*;

#[test]
fn default_yank_register_requires_one_writable_character() {
    let registry = Registry::<()>::default();
    for register in ["+", "*", "\"", "a", "_"] {
        registry
            .validate_view_options_value(&serde_json::json!({
                "default-yank-register": register,
            }))
            .unwrap();
    }
    for register in [
        serde_json::json!(""),
        serde_json::json!("ab"),
        serde_json::json!(true),
        serde_json::json!("#"),
    ] {
        assert!(
            registry
                .validate_view_options_value(&serde_json::json!({
                    "default-yank-register": register,
                }))
                .is_err()
        );
    }
}

#[test]
fn aliases_follow_command_replacement_and_reassignment() {
    let mut registry = Registry::default();
    registry.register_command_alias("open", "files");
    registry.register_command("files", "Files", CommandImplementation::External(1));
    registry.register_command("files", "Updated files", CommandImplementation::External(2));
    let info = registry.catalog().resolve("open").unwrap();
    assert_eq!(info.id, "files");
    assert_eq!(info.description, "Updated files");
    assert_eq!(info.command_line.as_ref().unwrap().aliases, ["open"]);
    assert!(matches!(
        registry.lookup_command("open"),
        Ok(("files", CommandImplementation::External(2)))
    ));

    registry.register_command("search", "Search", CommandImplementation::External(3));
    registry.register_command_alias("open", "search");
    registry.register_command_alias("open", "search");
    assert!(
        registry
            .catalog()
            .resolve("files")
            .unwrap()
            .command_line
            .as_ref()
            .unwrap()
            .aliases
            .is_empty()
    );
    assert_eq!(
        registry
            .catalog()
            .resolve("search")
            .unwrap()
            .command_line
            .as_ref()
            .unwrap()
            .aliases,
        ["open"]
    );
    assert!(matches!(
        registry.lookup_command("open"),
        Ok(("search", CommandImplementation::External(3)))
    ));
}

#[test]
fn internal_actions_have_no_command_line_entry() {
    let mut registry = Registry::default();
    registry.register_action("move", "Move selection", CommandImplementation::External(1));
    registry.register_command_alias("step", "move");
    assert!(registry.catalog().resolve_command_line("move").is_none());
    assert!(registry.catalog().resolve_command_line("step").is_none());
    assert_eq!(registry.catalog().command_line_entries().count(), 0);
    assert!(matches!(
        registry.lookup_command("step"),
        Ok(("move", CommandImplementation::External(1)))
    ));
}

#[test]
fn command_line_names_and_aliases_share_one_action() {
    let mut registry = Registry::default();
    registry.register_action("file.save", "Save file", CommandImplementation::External(1));
    registry.register_command_line("file.save", "write");
    registry.register_command_alias("w", "write");
    let entries = registry
        .catalog()
        .command_line_entries()
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let (action, entry) = entries[0];
    assert_eq!(action.id, "file.save");
    assert_eq!(entry.name, "write");
    assert_eq!(entry.aliases, ["w"]);
    for name in ["write", "w"] {
        assert_eq!(
            registry.catalog().resolve_command_line(name).unwrap().id,
            "file.save"
        );
    }
    assert!(
        registry
            .catalog()
            .resolve_command_line("file.save")
            .is_none()
    );
    assert!(matches!(
        registry.lookup_command("w"),
        Ok(("file.save", CommandImplementation::External(1)))
    ));
}

#[test]
fn command_line_aliases_do_not_replace_action_ids() {
    let mut registry = Registry::default();
    registry.register_action(
        "test",
        "Internal action",
        CommandImplementation::External(1),
    );
    registry.register_command(
        "check",
        "Public command",
        CommandImplementation::External(2),
    );
    registry.register_command_alias("test", "check");
    registry.register_command_alias("verify", "check");

    assert_eq!(registry.catalog().resolve("test").unwrap().id, "test");
    assert!(matches!(
        registry.lookup_command("test"),
        Ok(("test", CommandImplementation::External(1)))
    ));
    let command = registry.catalog().resolve_command_line("test").unwrap();
    assert_eq!(command.id, "check");
    assert!(matches!(
        registry.lookup_command(&command.id),
        Ok(("check", CommandImplementation::External(2)))
    ));
    assert!(matches!(
        registry.lookup_command("verify"),
        Ok(("check", CommandImplementation::External(2)))
    ));
}
