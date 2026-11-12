use super::*;

mod environment;

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
    renderer.pager = Some(PagerCommand {
        program: "cat".into(),
        args: Vec::new(),
    });
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
        let renderer = repository
            .diff_renderer(
                &DiffRenderOptions {
                    file_header: false,
                    width: Some(width),
                    pager: Some(PagerCommand {
                        program: "sh".into(),
                        args: vec![
                            "-c".into(),
                            "printf '%s:%s\\n' \"$COLUMNS\" \"$DFT_WIDTH\"; cat".into(),
                        ],
                    }),
                },
                &crate::tests::Execute,
            )
            .unwrap();
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
    renderer.pager = Some(PagerCommand {
        program: "sh".into(),
        args: vec![
            "-c".into(),
            "test \"$OSC1717\" = V1 && sed 's/^rendered:/paged:/'".into(),
        ],
    });
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
fn git_pager_configuration_does_not_enable_a_zess_pager() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    fixture.git(&["config", "core.pager", "printf inherited-pager"]);
    fixture.git(&["config", "pager.diff", "true"]);
    fixture.git(&["config", "diff.external", ""]);
    let repository = Repository::open(&fixture.0).unwrap();
    let renderer = repository
        .diff_renderer(&DiffRenderOptions::default(), &crate::tests::Execute)
        .unwrap();
    assert!(
        renderer
            .render(
                &repository,
                &DiffSource::Changes {
                    side: DiffSide::Unstaged
                },
                &DiffTarget {
                    path: "file".into(),
                    original_path: None
                },
                "patch",
                &crate::tests::Execute
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn pager_arguments_are_passed_literally_in_the_repository_directory() {
    let fixture = crate::tests::Repository::new();
    fixture.commit("old\n");
    let name = "literal $HOME; 'quoted file";
    std::fs::write(fixture.0.join(name), "pager output\n").unwrap();
    let repository = Repository::open(&fixture.0).unwrap();
    let renderer = repository
        .diff_renderer(
            &DiffRenderOptions {
                pager: Some(PagerCommand {
                    program: "cat".into(),
                    args: vec![name.into()],
                }),
                ..Default::default()
            },
            &crate::tests::Execute,
        )
        .unwrap();
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
    assert_eq!(rendered.text, "pager output\n");
}
