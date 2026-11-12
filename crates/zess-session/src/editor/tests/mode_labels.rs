use super::*;
use zess_plugin::StatusContext;

#[test]
fn modes_have_short_default_labels() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    for (mode, label) in [
        ("normal", " NOR "),
        ("insert", " INS "),
        ("select", " SEL "),
        ("filter", " FLT "),
        ("find", " FND "),
        ("block", " BLK "),
        ("line", " LIN "),
        ("range", " RNG "),
        ("custom", " CUS "),
        ("go", " GO "),
    ] {
        let spans = session.statusline(
            id,
            "default",
            StatusContext {
                mode: Some(mode.into()),
                width: 40,
                active: true,
                ..StatusContext::default()
            },
            40,
            true,
        );
        assert_eq!(spans[0].text, label, "mode {mode}");
    }
}

#[test]
fn configured_labels_override_defaults_and_keep_their_full_length() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "require \"zess/status\"\nset-mode-labels! : obj 'normal \"Editing\" \"custom\" \"Custom\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    let id = session.focus;
    for (mode, label) in [
        ("normal", " Editing "),
        ("custom", " Custom "),
        ("insert", " INS "),
    ] {
        let spans = session.statusline(
            id,
            "default",
            StatusContext {
                mode: Some(mode.into()),
                width: 40,
                active: true,
                ..StatusContext::default()
            },
            40,
            true,
        );
        assert_eq!(spans[0].text, label, "mode {mode}");
    }
}

#[test]
fn invalid_mode_label_configuration_is_rejected() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    for labels in ["#f", "(obj 'normal 42)", "(hash 42 \"bad\")"] {
        fs::write(
            &config,
            format!("require \"zess/status\"\nset-mode-labels! {labels}\n"),
        )
        .unwrap();
        let session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
        assert_eq!(session.generation, 0, "labels {labels}");
    }
}
