use super::*;

#[test]
fn renderings_keep_the_repository_root_when_the_workspace_is_a_subdirectory() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    let workspace = fixture.0.join("sub");
    std::fs::create_dir(&workspace).unwrap();
    let repository = Repository::open(&workspace).unwrap();
    let mut renderer = repository
        .diff_renderer(&crate::DiffRenderOptions::default(), &crate::tests::Execute)
        .unwrap();
    renderer.external = false;
    renderer.pager = Some("cat".into());
    let rendered = renderer
        .render(
            &repository,
            &DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            "patch",
            &crate::tests::Execute,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rendered.text, "patch");
    assert_eq!(rendered.repository_root, fixture.0.canonicalize().unwrap());
}

#[test]
fn pagers_receive_the_requested_view_width() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    let repository = Repository::open(&fixture.0).unwrap();
    for (width, expected) in [(44, "44:44\npatch"), (120, "120:120\npatch")] {
        let mut renderer = repository
            .diff_renderer(
                &DiffRenderOptions { width: Some(width) },
                &crate::tests::Execute,
            )
            .unwrap();
        renderer.external = false;
        renderer.pager = Some("printf '%s:%s\\n' \"$COLUMNS\" \"$DFT_WIDTH\"; cat".into());
        let rendered = renderer
            .render(
                &repository,
                &DiffSource::Changes {
                    side: DiffSide::Unstaged,
                },
                &DiffTarget {
                    path: "file".into(),
                    original_path: None,
                },
                "patch",
                &crate::tests::Execute,
            )
            .unwrap()
            .unwrap();
        assert_eq!(rendered.text, expected);
    }
}

#[test]
fn configured_pagers_filter_the_external_rendering_instead_of_the_applicable_patch() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    std::fs::write(fixture.0.join("file"), "new\n").unwrap();
    std::fs::write(
        fixture.0.join("renderer.sh"),
        "test \"$OSC1717\" = V1 || exit 2\nprintf 'rendered:%s\\n' \"$1\"\n",
    )
    .unwrap();
    fixture.git(&["config", "diff.external", "sh ./renderer.sh"]);
    let repository = Repository::open(&fixture.0).unwrap();
    let mut renderer = repository
        .diff_renderer(&crate::DiffRenderOptions::default(), &crate::tests::Execute)
        .unwrap();
    renderer.pager = Some("test \"$OSC1717\" = V1 && sed 's/^rendered:/paged:/'".into());
    let raw = fixture.diff(DiffSide::Unstaged);
    let rendered = renderer
        .render(
            &repository,
            &DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            &raw,
            &crate::tests::Execute,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rendered.text, "paged:file\n");
    assert_eq!(rendered.repository_root, fixture.0.canonicalize().unwrap());
    assert!(raw.contains("-old\n+new\n"));
}

#[test]
fn pager_configuration_respects_git_precedence_and_explicit_disabling() {
    for (git_pager, diff_pager, core_pager, pager, expected) in [
        (
            Some("environment"),
            Some("diff"),
            Some("core"),
            Some("fallback"),
            Some("environment"),
        ),
        (
            Some(""),
            Some("diff"),
            Some("core"),
            Some("fallback"),
            Some(""),
        ),
        (
            None,
            Some("diff"),
            Some("core"),
            Some("fallback"),
            Some("diff"),
        ),
        (None, Some("false"), Some("core"), Some("fallback"), None),
        (
            Some("environment"),
            Some("FALSE"),
            Some("core"),
            Some("fallback"),
            None,
        ),
        (None, Some(""), Some("core"), Some("fallback"), None),
        (
            None,
            Some("true"),
            Some("core"),
            Some("fallback"),
            Some("core"),
        ),
        (None, None, Some("core"), Some("fallback"), Some("core")),
        (
            None,
            Some("2"),
            Some("core"),
            Some("fallback"),
            Some("core"),
        ),
        (None, None, None, Some("fallback"), Some("fallback")),
        (None, None, None, None, None),
    ] {
        assert_eq!(
            select_pager(
                git_pager.map(str::to_owned),
                diff_pager.map(str::to_owned),
                core_pager.map(str::to_owned),
                pager.map(str::to_owned)
            )
            .as_deref(),
            expected
        );
    }
}
