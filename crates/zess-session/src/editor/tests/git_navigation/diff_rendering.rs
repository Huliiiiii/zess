use super::*;

fn configure_renderer(fixture: &Fixture) {
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(
        &script,
        "i=0\nwhile [ \"$i\" -lt 40 ]; do\n  printf '\\033[31mEXTERNAL %s %03d %0100d\\033[0m\\n' \"$1\" \"$i\" 0\n  i=$((i + 1))\ndone\n",
    )
    .unwrap();
    git(
        fixture,
        &[
            "config",
            "diff.external",
            &format!("sh {}", script.display()),
        ],
    );
}

fn configure_mapped_renderer(fixture: &Fixture) {
    let script = fixture.0.join(".git/renderer.sh");
    let output = concat!(
        "\x1b]1717;1\x1b\\",
        "\x1b]1717;1;f;;;note.txt\x1b\\\x1b]1717;1;h;1;;note.txt\x1b\\rendered file\n",
        "\x1b]1717;1;d;1;1;note.txt\x1b\\\x1b]1717;1;a;1;;note.txt\x1b\\\x1b[31mhistorical => working\x1b[0m\n",
        "\x1b]1717;1;a;2;;note.txt\x1b\\second addition\n",
    );
    fs::write(&script, format!("test \"$OSC1717\" = V1 || exit 2\ncat >/dev/null\ncat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n")).unwrap();
    git(
        fixture,
        &[
            "config",
            "diff.external",
            &format!("sh {}", script.display()),
        ],
    );
}

fn configure_width_renderer(fixture: &Fixture) -> Fixture {
    let renders = Fixture::new();
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(
        &script,
        format!("printf '%s\\n' \"$COLUMNS\" >> '{}'/widths\nprintf 'width=%s:%s\\n' \"$COLUMNS\" \"$DFT_WIDTH\"\n", renders.0.display()),
    )
    .unwrap();
    git(
        fixture,
        &[
            "config",
            "diff.external",
            &format!("sh {}", script.display()),
        ],
    );
    git(fixture, &["config", "pager.diff", "false"]);
    renders
}

#[test]
fn resizing_keeps_external_diffs_fitted_to_their_preview_panes() {
    let (fixture, path) = repository();
    git(&fixture, &["add", "note.txt"]);
    fs::write(path, "new worktree\n").unwrap();
    let renders = configure_width_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.resize(300, 30);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let mut initial_widths = fs::read_to_string(renders.0.join("widths"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    initial_widths.sort();
    assert_eq!(initial_widths, ["74", "75"]);
    let mut original = None;
    for (width, expected) in [(300, [74, 75]), (180, [90, 90]), (302, [75, 75])] {
        session.resize(width, 30);
        settle(&mut session);
        let page = session.views.page(source);
        let slots = &page.previews.as_ref().unwrap().members;
        assert_eq!(slots.len(), 2);
        let identities = slots
            .iter()
            .map(|slot| (slot.view().unwrap().id, slot.buffer()))
            .collect::<Vec<_>>();
        if let Some(original) = &original {
            assert_eq!(&identities, original);
        } else {
            original = Some(identities);
        }
        let rectangles = session.visible_rectangles();
        for (slot, expected) in slots.iter().zip(expected) {
            let view = slot.view().unwrap();
            let rect = rectangles
                .iter()
                .find(|(target, _)| *target == RenderTarget::View(view.id))
                .unwrap()
                .1;
            assert_eq!(rect.width, expected);
            let Buffer::PreviewDiff { diff, .. } = &session.buffers[&slot.buffer()] else {
                panic!("expected diff preview");
            };
            assert!(!diff.files.is_empty());
            for file in &diff.files {
                assert_eq!(
                    file.rendered.as_ref().unwrap().text,
                    format!("width={expected}:{expected}\n"),
                );
            }
        }
    }
}

#[test]
fn splitting_refits_external_diffs_in_both_pages() {
    let (fixture, _) = repository();
    let _renders = configure_width_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.resize(300, 30);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let buffer = session.views[&preview].location.buffer;
    session.command("vsplit").unwrap();
    let split = session.focus;
    settle(&mut session);
    assert_eq!(session.focus, split);
    assert_eq!(session.preview_target(source), Some(preview));
    assert_eq!(session.views[&preview].location.buffer, buffer);
    for (page, expected) in [(source, 74), (split, 75)] {
        let preview = session.preview_target(page).unwrap();
        let Buffer::PreviewDiff { diff, .. } =
            &session.buffers[&session.views[&preview].location.buffer]
        else {
            panic!("expected diff preview");
        };
        assert_eq!(
            diff.files[0].rendered.as_ref().unwrap().text,
            format!("width={expected}:{expected}\n")
        );
    }
    session.command("q").unwrap();
    settle(&mut session);
    assert_eq!(session.focus, source);
    assert_eq!(session.preview_target(source), Some(preview));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff preview");
    };
    assert_eq!(
        diff.files[0].rendered.as_ref().unwrap().text,
        "width=150:150\n"
    );
}

#[test]
fn selecting_a_directory_fits_the_planned_diff_panes_before_publication() {
    let fixture = nested_changes();
    let renders = configure_width_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.resize(300, 30);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .len(),
        1
    );
    for key in ["g", "e", "h", "h"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "directory:dir");
    let slots = &session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members;
    assert_eq!(slots.len(), 2);
    for (slot, expected) in slots.iter().zip([74, 75]) {
        let Buffer::PreviewDiff { diff, .. } = &session.buffers[&slot.buffer()] else {
            panic!("expected diff preview");
        };
        assert!(!diff.files.is_empty());
        for file in &diff.files {
            assert_eq!(
                file.rendered.as_ref().unwrap().text,
                format!("width={expected}:{expected}\n")
            );
        }
    }
    let widths = fs::read_to_string(renders.0.join("widths")).unwrap();
    assert!(
        widths
            .lines()
            .all(|width| matches!(width, "150" | "74" | "75"))
    );
}

#[test]
fn opening_and_clicking_a_whole_file_diff_keeps_its_entire_block_selected_for_staging() {
    let (fixture, path) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let preview = session.preview_target(session.focus).unwrap();
    session.frame();
    assert!(matches!(
        session.views[&preview].location.diff_selection,
        diff::Selection::Block { .. }
    ));
    session.input("Enter");
    assert_eq!(session.focus, preview);
    assert!(matches!(
        session.views[&preview].location.diff_selection,
        diff::Selection::Block { .. }
    ));
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(preview))
        .unwrap();
    let body = region.body;
    let crate::editor::mouse::Content::Diff { scroll, .. } = region.content else {
        panic!("expected diff rows");
    };
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff");
    };
    let rows = view.location.diff_selection.rows(&diff.index).unwrap();
    assert_eq!(diff.line(*rows.start()), "-historical");
    assert_eq!(diff.line(*rows.end()), "+working");
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::Press,
            column: body.x as u16,
            row: (body.y + *rows.end() - scroll) as u16,
        },
    );
    let view = &session.views[&preview];
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Block { .. }
    ));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff");
    };
    assert_eq!(
        view.location.diff_selection.rows(&diff.index).unwrap(),
        rows
    );
    session.input("space");
    session.input("s");
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(fs::read_to_string(path).unwrap(), "working\n");
}

#[test]
fn clicking_a_rendered_change_opens_its_raw_block_for_staging() {
    let (fixture, path) = repository();
    fs::write(&path, "old\ncontext\nsecond old\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
    fs::write(&path, "new\ncontext\nsecond new\n").unwrap();
    let script = fixture.0.join(".git/renderer.sh");
    let output = include_str!("../../diff/tests/fixtures/delta-0.20.1-side-by-side.ansi");
    fs::write(&script, format!("cat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n")).unwrap();
    git(
        &fixture,
        &[
            "config",
            "diff.external",
            &format!("sh {}", script.display()),
        ],
    );
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let preview = session.preview_target(session.focus).unwrap();
    let frame = session.frame();
    let body = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(preview))
        .unwrap()
        .body;
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::Press,
            column: body.x as u16,
            row: (body.y + 7) as u16,
        },
    );
    assert_eq!(session.focus, preview);
    let view = &session.views[&session.focus];
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Block { .. }
    ));
    assert!(matches!(
        view.location.diff_presentation,
        diff::Presentation::Raw
    ));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff")
    };
    assert_eq!(
        view.location
            .diff_selection
            .rows(diff.index(diff::Presentation::Raw))
            .unwrap(),
        5..=6
    );
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "new\ncontext\nsecond old"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "new\ncontext\nsecond new\n"
    );
}

#[test]
fn clicking_a_rendered_change_allows_staging_its_raw_lines_without_the_next_addition() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    configure_mapped_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let preview = session.preview_target(session.focus).unwrap();
    session
        .views
        .get_mut(&preview)
        .unwrap()
        .location
        .diff_selection = diff::Selection::Line { head: None };
    let frame = session.frame();
    let body = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(preview))
        .unwrap()
        .body;
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::Press,
            column: body.x as u16,
            row: (body.y + 1) as u16,
        },
    );
    assert_eq!(session.focus, preview);
    assert!(matches!(
        session.views[&preview].location.diff_presentation,
        diff::Presentation::Raw
    ));
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(6)
    );
    let (rows, cursor) = session.render_view(preview, 80, 12);
    assert!(cursor.is_none());
    assert!(
        rows.iter().flat_map(|row| &row.spans).any(|span| {
            span.text.trim_end() == "+working" && span.style == "selection.git.added"
        })
    );
    for key in ["k", "v", "j"] {
        session.input(key);
    }
    session.input("space");
    session.input("s");
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "working\nsecond addition\n"
    );
}

#[test]
fn splitting_a_diff_keeps_raw_navigation_independent_of_the_browsing_view() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    configure_mapped_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    let source = session.focus;
    session.command("vsplit").unwrap();
    assert_ne!(session.focus, source);
    assert!(matches!(
        session.views[&session.focus].location.diff_presentation,
        diff::Presentation::Raw
    ));
    assert!(matches!(
        session.views[&source].location.diff_presentation,
        diff::Presentation::Rendered
    ));
    assert_eq!(
        session.views[&source].location.diff_selection.row(),
        Some(5)
    );
    for key in ["a", "j", "j", "e"] {
        session.input(key);
    }
    settle(&mut session);
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("selected source was not opened: {}", session.message);
    };
    assert_eq!(text.path(), Some(path.as_path()));
    assert_eq!(text.view(editor).cursor().0, 1);
    assert_eq!(
        session.views[&source].location.diff_selection.row(),
        Some(5)
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "working\nsecond addition\n"
    );
}

#[test]
fn raw_navigation_and_search_select_the_original_change_block_for_staging() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    configure_mapped_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    let preview = session.focus;
    assert!(matches!(
        session.views[&preview].location.diff_presentation,
        diff::Presentation::Raw
    ));
    session.input("a");
    session.input("j");
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(6)
    );
    session
        .execute_command("diff.search-forward", json!({}))
        .unwrap();
    for key in ["w", "o", "r", "k", "i", "n", "g", "Enter"] {
        session.input(key);
    }
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(6)
    );
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session.input("space");
    session.input("s");
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "working\nsecond addition"
    );
}

#[test]
fn changing_the_renderer_preserves_the_raw_selection_for_staging() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    configure_mapped_renderer(&fixture);
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(&config, "").unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    let preview = session.focus;
    for key in ["a", "v", "j"] {
        session.input(key);
    }
    let buffer = session.views[&preview].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff");
    };
    assert_eq!(
        session.views[&preview]
            .location
            .diff_selection
            .rows(&diff.index),
        Some(5..=6)
    );
    fs::write(
        fixture.0.join(".git/renderer.sh"),
        "printf 'unsupported renderer\n'\n",
    )
    .unwrap();
    session.focus_view(source).unwrap();
    session.execute_command("git.refresh", json!({})).unwrap();
    settle(&mut session);
    session.focus_view(preview).unwrap();
    session.resize(132, 28);
    settle(&mut session);
    let view = &session.views[&preview];
    assert_eq!(view.location.buffer, buffer);
    assert!(matches!(
        view.location.diff_presentation,
        diff::Presentation::Raw
    ));
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Range { .. }
    ));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff");
    };
    assert_eq!(
        diff.files[0].rendered.as_ref().unwrap().text,
        "unsupported renderer\n"
    );
    assert_eq!(view.location.diff_selection.rows(&diff.index), Some(5..=6));
    session.apply_diff_selection().unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "working\nsecond addition\n"
    );
    assert_eq!(session.preview_source(preview), Some(source));
}

#[test]
fn external_diff_is_colored_for_browsing_and_raw_when_focused() {
    let (fixture, _) = repository();
    configure_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let frame = session.frame();
    let (rows, _) = session.render_view(preview, 160, 12);
    let external = rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| {
            frame
                .styles
                .get(&span.style)
                .is_some_and(|style| style.foreground == Some(zess_ui::StyleColor::Indexed(1)))
        })
        .unwrap();
    let expected = format!("EXTERNAL note.txt 000 {:0100}", 0);
    assert_eq!(external.text, expected);
    let external_style = external.style.clone();
    assert_eq!(
        frame.styles[&external.style].foreground,
        Some(zess_ui::StyleColor::Indexed(1))
    );
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| !span.style.starts_with("selection") && span.style != "linenr.current")
    );

    session.focus_view(preview).unwrap();
    let (rows, _) = session.render_view(preview, 80, 12);
    let added = rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| matches!(span.style.as_str(), "git.added" | "selection.git.added"))
        .unwrap();
    assert_eq!(added.text.trim_end(), "+working");
    assert!(matches!(
        session.views[&preview].location.diff_presentation,
        diff::Presentation::Raw
    ));
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| span.style != external_style)
    );
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.style.starts_with("selection"))
    );

    session.focus_view(source).unwrap();
    assert!(matches!(
        session.views[&preview].location.diff_presentation,
        diff::Presentation::Rendered
    ));
    let (rows, _) = session.render_view(preview, 160, 12);
    let external = rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| span.style == external_style)
        .unwrap();
    assert_eq!(external.text, expected);
}

#[test]
fn staging_after_external_browsing_uses_the_original_patch() {
    let (fixture, path) = repository();
    configure_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session.input("space");
    session.input("s");
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(fs::read_to_string(path).unwrap(), "working\n");
}

#[test]
fn clicking_external_output_focuses_raw_without_selecting_an_unrelated_patch_row() {
    let (fixture, _) = repository();
    configure_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let preview = session.preview_target(session.focus).unwrap();
    let frame = session.frame();
    let body = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(preview))
        .unwrap()
        .body;
    assert!(body.height > 6);
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::Press,
            column: body.x as u16,
            row: (body.y + 6) as u16,
        },
    );
    assert_eq!(session.focus, preview);
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff")
    };
    assert_eq!(
        diff.line(view.location.diff_selection.row().unwrap()),
        "-historical"
    );
    assert!(matches!(
        view.location.diff_presentation,
        diff::Presentation::Raw
    ));
    let (rows, _) = session.render_view(preview, 80, 12);
    let added = rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| matches!(span.style.as_str(), "git.added" | "selection.git.added"))
        .unwrap();
    assert_eq!(added.text.trim_end(), "+working");
}

#[test]
fn mouse_wheel_scrolls_external_rows_without_focusing_the_preview() {
    let (fixture, _) = repository();
    configure_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let frame = session.frame();
    let body = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(preview))
        .unwrap()
        .body;
    for _ in 0..100 {
        session.mouse(
            frame.mouse_context,
            zess_ui::MouseEvent {
                kind: zess_ui::MouseKind::ScrollDown,
                column: body.x as u16,
                row: body.y as u16,
            },
        );
    }
    let view = &session.views[&preview];
    assert_eq!(view.location.scroll, 40 - view.viewport_height);
    assert_eq!(session.focus, source);
}

#[test]
fn browsing_scrolls_the_external_output_before_opening_its_current_file() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir(fixture.0.join("dir")).unwrap();
    for name in ["first", "second"] {
        fs::write(fixture.0.join(format!("dir/{name}.txt")), "before\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    for name in ["first", "second"] {
        fs::write(fixture.0.join(format!("dir/{name}.txt")), "after\n").unwrap();
    }
    configure_renderer(&fixture);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "g"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "directory:dir");
    let preview = session.preview_target(source).unwrap();
    for _ in 0..200 {
        session.move_preview(ViewDirection::Down).unwrap();
        session.move_preview(ViewDirection::Right).unwrap();
    }
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff")
    };
    let rendered = diff.rendered.as_ref().unwrap();
    assert_eq!(
        view.location.scroll,
        rendered.rows.len() - view.viewport_height
    );
    assert!(view.location.scroll > diff.index.rows.len());
    assert_eq!(
        view.location.horizontal_scroll,
        rendered.width.saturating_sub(view.viewport_width)
    );
    assert!(view.location.horizontal_scroll > 0);

    session.focus_view(preview).unwrap();
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff")
    };
    assert_eq!(
        diff.file_at(view.location.diff_selection.row().unwrap())
            .unwrap()
            .target
            .path,
        "dir/second.txt"
    );
    session.input("e");
    let view = &session.views[&session.focus];
    let Buffer::Text(text) = &session.buffers[&view.location.buffer] else {
        panic!("expected source editor: {}", session.message)
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("dir/second.txt").as_path())
    );
    assert_eq!(text.text(), "after\n");
}

#[test]
fn returning_from_external_preview_preserves_the_second_raw_block_and_line_mode() {
    for mapped in [false, true] {
        let (fixture, path) = repository();
        fs::write(&path, "old\ncontext\nsecond old\n").unwrap();
        git(&fixture, &["add", "note.txt"]);
        git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
        fs::write(&path, "new\ncontext\nsecond new\n").unwrap();
        configure_renderer(&fixture);
        if mapped {
            let output = include_str!("../../diff/tests/fixtures/delta-0.20.1-side-by-side.ansi");
            fs::write(
                fixture.0.join(".git/renderer.sh"),
                format!("cat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n"),
            )
            .unwrap();
        }
        let mut session = fixture.session(&[]);
        session.command("git").unwrap();
        settle(&mut session);
        let source = session.focus;
        session.input("Enter");
        let preview = session.focus;
        for key in ["}", "a", "j"] {
            session.input(key);
        }
        assert_eq!(
            session.views[&preview].location.diff_selection.row(),
            Some(9)
        );
        session.input("Esc");
        assert_eq!(session.focus, source);
        assert!(matches!(
            session.views[&preview].location.diff_presentation,
            diff::Presentation::Rendered
        ));
        let (rows, cursor) = session.render_view(preview, 80, 12);
        assert!(cursor.is_none());
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans)
                .all(|span| span.style != "diff.selection")
        );
        session.input("Enter");
        assert_eq!(session.focus, preview);
        assert!(matches!(
            session.views[&preview].location.diff_presentation,
            diff::Presentation::Raw
        ));
        assert!(matches!(
            session.views[&preview].location.diff_selection,
            diff::Selection::Line { .. }
        ));
        assert_eq!(
            session.views[&preview].location.diff_selection.row(),
            Some(9)
        );
    }
}

#[test]
fn browsing_mapped_output_keeps_its_scroll_and_opens_the_visible_raw_block() {
    let (fixture, path) = repository();
    fs::write(&path, "old\ncontext\nsecond old\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
    fs::write(&path, "new\ncontext\nsecond new\n").unwrap();
    configure_renderer(&fixture);
    let output = include_str!("../../diff/tests/fixtures/delta-0.20.1-side-by-side.ansi");
    let (before, second) = output.rsplit_once("\x1b]1717;1;d;3;3;").unwrap();
    let output = format!(
        "{before}{}\x1b]1717;1;d;3;3;{second}",
        "decoration\n".repeat(40)
    );
    fs::write(
        fixture.0.join(".git/renderer.sh"),
        format!("cat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n"),
    )
    .unwrap();
    let mut session = fixture.session(&[]);
    session.resize(100, 16);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let selected = session.views[&preview].location.diff_selection.row();
    for _ in 0..50 {
        session.move_preview(ViewDirection::Down).unwrap();
    }
    let scroll = session.views[&preview].location.scroll;
    assert!(scroll > 9);
    session.frame();
    assert_eq!(session.views[&preview].location.scroll, scroll);
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        selected
    );
    session.input("Enter");
    assert_eq!(session.focus, preview);
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff");
    };
    assert!(matches!(
        view.location.diff_presentation,
        diff::Presentation::Raw
    ));
    assert_eq!(view.location.diff_selection.rows(&diff.index), Some(8..=9));
}
