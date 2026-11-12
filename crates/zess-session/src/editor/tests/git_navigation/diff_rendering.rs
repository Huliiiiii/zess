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
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(fs::read_to_string(path).unwrap(), "working\n");
}

#[test]
fn changing_the_diff_pager_preserves_the_preview_view_and_buffer() {
    let (fixture, _) = repository();
    git(
        &fixture,
        &["config", "diff.external", "sh .git/renderer.sh"],
    );
    git(&fixture, &["config", "pager.diff", "false"]);
    fs::write(
        fixture.0.join(".git/renderer.sh"),
        "printf 'automatic\\n'\n",
    )
    .unwrap();
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"-c\" \"printf 'first\\n'; cat\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let buffer = session.views[&preview].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff preview");
    };
    let raw = diff.raw.clone();
    assert_eq!(
        diff.files[0].rendered.as_ref().unwrap().text,
        format!("first\n{raw}")
    );
    fs::write(
        &config,
        "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"-c\" \"printf 'second\\n'; cat\"\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    assert_eq!(session.preview_target(source), Some(preview));
    assert_eq!(session.views[&preview].location.buffer, buffer);
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff preview");
    };
    assert_eq!(diff.raw, raw);
    assert_eq!(
        diff.files[0].rendered.as_ref().unwrap().text,
        format!("second\n{raw}")
    );
    fs::write(&config, "").unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    assert_eq!(session.preview_target(source), Some(preview));
    assert_eq!(session.views[&preview].location.buffer, buffer);
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff preview");
    };
    assert_eq!(diff.raw, raw);
    assert_eq!(diff.files[0].rendered.as_ref().unwrap().text, "automatic\n");
}

#[test]
fn historical_diffs_use_the_configured_pager_and_file_headers() {
    let (fixture, _) = repository();
    configure_renderer(&fixture);
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        "set-view-options! 'git-commit-files : obj 'diff-file-header #t 'diff-pager : obj 'program \"sh\" 'args : list \"-c\" \"printf 'historical\\n'; cat\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    let source = session.focus;
    assert!(
        matches!(&session.buffers[&session.views[&source].location.buffer], Buffer::Data { kind, .. } if kind == "git-commit-files")
    );
    let preview = session.preview_target(source).unwrap();
    let Buffer::PreviewDiff { diff, .. } =
        &session.buffers[&session.views[&preview].location.buffer]
    else {
        panic!("expected historical diff preview");
    };
    assert!(matches!(
        diff.source,
        Some(zess_git::DiffSource::Commit { .. })
    ));
    assert_eq!(
        diff.files[0].rendered.as_ref().unwrap().text,
        format!("historical\n{}", diff.raw)
    );
    let header = &diff.rendered.as_ref().unwrap().rows[0];
    assert_eq!(header.spans.len(), 1);
    assert_eq!(header.spans[0].text, "note.txt");
    assert_eq!(header.spans[0].style, "ui.heading");
}

#[test]
fn a_compact_diff_without_context_keeps_navigation_and_partial_staging() {
    let (fixture, path) = repository();
    fs::write(&path, "old\ncontext\nsecond old\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
    fs::write(&path, "new\ncontext\nsecond new\n").unwrap();
    // Captured from delta 0.20.1 with --no-gitconfig --paging=never --side-by-side
    // --width=44 --file-decoration-style=none --hunk-header-decoration-style=none
    // --hunk-header-style='line-number syntax' and OSC1717=V1.
    let output = include_str!("../../diff/tests/fixtures/delta-0.20.1-compact.ansi")
        .lines()
        .filter(|line| !line.starts_with("\x1b]1717;1;c;"))
        .collect::<Vec<_>>()
        .join("\n");
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(&script, format!("cat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n")).unwrap();
    git(&fixture, &["config", "diff.external", "false"]);
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"{}\"\n",
            script.display()
        ),
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let preview = session.preview_target(session.focus).unwrap();
    session.focus_view(preview).unwrap();
    let Buffer::PreviewDiff { diff, .. } =
        &session.buffers[&session.views[&preview].location.buffer]
    else {
        panic!("expected diff preview");
    };
    assert!(diff.mapped_rendering());
    assert_eq!(diff.index(diff::Presentation::Rendered).blocks.len(), 2);
    let rendered = diff
        .rendered
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(rendered.contains("note.txt"));
    for header in ["diff --git", "index ", "--- a/", "+++ b/", "@@ -"] {
        assert!(!rendered.contains(header));
    }
    session
        .execute_command("diff.hunk-next", json!({}))
        .unwrap();
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff preview");
    };
    assert_eq!(
        diff.index(view.location.diff_presentation).rows
            [view.location.diff_selection.row().unwrap()]
        .source_line,
        Some(2)
    );
    session
        .execute_command("diff.hunk-previous", json!({}))
        .unwrap();
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff preview");
    };
    assert_eq!(
        diff.index(view.location.diff_presentation).rows
            [view.location.diff_selection.row().unwrap()]
        .source_line,
        Some(0)
    );
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
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
fn clicking_a_mapped_change_keeps_its_rendered_block_for_staging() {
    let (fixture, path) = repository();
    fs::write(&path, "old\ncontext\nsecond old\n").unwrap();
    let sentinel = fixture.0.join("z-sentinel.txt");
    fs::write(&sentinel, "old\ncontext\nsecond old\n").unwrap();
    git(&fixture, &["add", "note.txt", "z-sentinel.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
    fs::write(&path, "new\ncontext\nsecond new\n").unwrap();
    fs::write(&sentinel, "new\ncontext\nsecond new\n").unwrap();
    let script = fixture.0.join(".git/renderer.sh");
    let output = include_str!("../../diff/tests/fixtures/delta-0.20.1-side-by-side.ansi");
    fs::write(&script, format!("cat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n")).unwrap();
    git(&fixture, &["config", "diff.external", "false"]);
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"{}\"\n",
            script.display()
        ),
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
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
        diff::Presentation::Rendered
    ));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff")
    };
    assert_eq!(
        view.location
            .diff_selection
            .rows(diff.index(diff::Presentation::Rendered))
            .unwrap(),
        6..=7
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
    assert_eq!(
        git(&fixture, &["show", ":z-sentinel.txt"]),
        "old\ncontext\nsecond old"
    );
    assert_eq!(
        fs::read_to_string(sentinel).unwrap(),
        "new\ncontext\nsecond new\n"
    );
}

#[test]
fn a_mapped_line_stages_its_original_changes_without_the_next_addition() {
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
        diff::Presentation::Rendered
    ));
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(1)
    );
    let (rows, cursor) = session.render_view(preview, 80, 12);
    assert!(cursor.is_none());
    assert!(rows.iter().flat_map(|row| &row.spans).any(|span| {
        span.text.trim_end() == "historical => working" && span.style == "diff.selection"
    }));
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
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
fn splitting_a_mapped_diff_keeps_navigation_independent_of_the_browsing_view() {
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
        diff::Presentation::Rendered
    ));
    assert!(matches!(
        session.views[&source].location.diff_presentation,
        diff::Presentation::Rendered
    ));
    assert_eq!(
        session.views[&source].location.diff_selection.row(),
        Some(1)
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
        Some(1)
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "working\nsecond addition\n"
    );
}

#[test]
fn mapped_navigation_and_search_select_the_original_change_block_for_staging() {
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
        diff::Presentation::Rendered
    ));
    session.input("a");
    session.input("j");
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(2)
    );
    session
        .execute_command("diff.search-forward", json!({}))
        .unwrap();
    for key in ["w", "o", "r", "k", "i", "n", "g", "Enter"] {
        session.input(key);
    }
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(1)
    );
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "working\nsecond addition"
    );
}

#[test]
fn separate_display_segments_highlight_and_stage_only_their_original_change_block() {
    let (fixture, path) = repository();
    fs::write(&path, "old-a1\nold-a2\ncontext\nold-b\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
    fs::write(&path, "new-a1\nnew-a2\ncontext\nnew-b\n").unwrap();
    let output = concat!(
        "\x1b]1717;1;f;;;note.txt\x07note.txt\n",
        "\x1b]1717;1;d;1;1;note.txt\x07\x1b]1717;1;a;1;;note.txt\x07A first\n",
        "\x1b]1717;1;d;4;4;note.txt\x07\x1b]1717;1;a;4;;note.txt\x07B\n",
        "\x1b]1717;1;d;1;2;note.txt\x07\x1b]1717;1;a;2;;note.txt\x07A second\n",
    );
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(
        &script,
        format!("cat >/dev/null\ncat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n"),
    )
    .unwrap();
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"{}\"\n",
            script.display()
        ),
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    let preview = session.focus;
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
            row: (body.y + 3) as u16,
        },
    );
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(3)
    );
    for (key, expected) in [
        (None, vec!["A first", "A second"]),
        (Some("j"), vec!["B"]),
        (Some("k"), vec!["A first", "A second"]),
    ] {
        if let Some(key) = key {
            session.input(key);
        }
        let selected = session
            .frame()
            .rows
            .iter()
            .filter_map(|row| {
                let selected = row
                    .spans
                    .iter()
                    .filter(|span| span.style == "diff.selection")
                    .map(|span| span.text.as_str())
                    .collect::<String>();
                (!selected.trim().is_empty()).then(|| selected.trim().to_owned())
            })
            .collect::<Vec<_>>();
        assert_eq!(selected, expected);
    }
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "new-a1\nnew-a2\ncontext\nold-b"
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "new-a1\nnew-a2\ncontext\nnew-b\n"
    );
}

#[test]
fn an_unresolved_reference_inside_a_range_rejects_the_entire_index_update() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    let output = concat!(
        "\x1b]1717;1;f;;;note.txt\x07note.txt\n",
        "\x1b]1717;1;d;1;1;note.txt\x07\x1b]1717;1;a;1;;note.txt\x07replacement\n",
        "\x1b]1717;1;a;99;;note.txt\x07unresolved\n",
        "\x1b]1717;1;a;2;;note.txt\x07second addition\n",
    );
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(
        &script,
        format!("cat >/dev/null\ncat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n"),
    )
    .unwrap();
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"{}\"\n",
            script.display()
        ),
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    for key in ["Enter", "a", "v", "j", "j"] {
        session.input(key);
    }
    let view = &session.views[&session.focus];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        unreachable!()
    };
    assert_eq!(
        view.location
            .diff_selection
            .rows(diff.index(diff::Presentation::Rendered)),
        Some(1..=3)
    );
    let before = git(&fixture, &["write-tree"]);
    assert_eq!(
        session
            .apply_diff_selection(session.focus)
            .unwrap_err()
            .to_string(),
        "selected row has no reliable original patch mapping"
    );
    assert!(session.git_patch.is_none());
    assert_eq!(git(&fixture, &["write-tree"]), before);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "working\nsecond addition\n"
    );
}

#[test]
fn unresolved_rendered_references_cannot_stage_the_previous_selection() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    configure_mapped_renderer(&fixture);
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(&config, "").unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let index_before = git(&fixture, &["write-tree"]);
    session.input("Enter");
    let preview = session.focus;
    for key in ["a", "v"] {
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
            .rows(diff.index(diff::Presentation::Rendered)),
        Some(1..=1)
    );
    let invalid = "\x1b]1717;1;a;1;;another.txt\x07unsupported renderer\n";
    let script = fixture.0.join(".git/invalid-renderer.sh");
    fs::write(&script, format!("cat <<'ZESS_DIFF'\n{invalid}ZESS_DIFF\n")).unwrap();
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"{}\"\n",
            script.display()
        ),
    )
    .unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    session.resize(132, 28);
    settle(&mut session);
    let view = &session.views[&preview];
    assert_eq!(view.location.buffer, buffer);
    assert!(matches!(
        view.location.diff_presentation,
        diff::Presentation::Rendered
    ));
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Line { head: None }
    ));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff");
    };
    assert_eq!(diff.files[0].rendered.as_ref().unwrap().text, invalid);
    assert!(diff.mapped_rendering());
    session
        .execute_command("diff.goto-first", json!({}))
        .unwrap();
    assert_eq!(
        session
            .apply_diff_selection(session.focus)
            .unwrap_err()
            .to_string(),
        "selected row has no reliable original patch mapping"
    );
    assert!(session.git_patch.is_none());
    assert_eq!(git(&fixture, &["write-tree"]), index_before);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
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
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
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
        session
            .move_preview(session.focus, ViewDirection::Down)
            .unwrap();
        session
            .move_preview(session.focus, ViewDirection::Right)
            .unwrap();
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
fn returning_from_external_preview_preserves_the_second_block_and_line_mode() {
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
        assert!(
            session.views[&preview].location.diff_presentation
                == if mapped {
                    diff::Presentation::Rendered
                } else {
                    diff::Presentation::Raw
                }
        );
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
fn browsing_mapped_output_keeps_its_scroll_and_focuses_the_visible_change_block() {
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
        session
            .move_preview(session.focus, ViewDirection::Down)
            .unwrap();
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
        diff::Presentation::Rendered
    ));
    assert_eq!(view.location.scroll, scroll);
    assert_eq!(
        diff.raw_row(
            view.location.diff_presentation,
            view.location.diff_selection.row().unwrap()
        ),
        Some(9)
    );
    assert_eq!(
        view.location
            .diff_selection
            .rows(diff.index(view.location.diff_presentation)),
        Some(49..=49)
    );
}

#[test]
fn file_headers_keep_multifile_changes_and_source_navigation_mapped() {
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
    fs::write(
        fixture.0.join(".git/renderer.sh"),
        "printf '\\033]1717;1;d;1;1;%s\\007before\\n\\033]1717;1;a;1;;%s\\007after\\n' \"$1\" \"$1\"\n",
    )
    .unwrap();
    git(
        &fixture,
        &["config", "diff.external", "sh .git/renderer.sh"],
    );
    git(&fixture, &["config", "pager.diff", "false"]);
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        "set-view-options! 'git : obj 'diff-file-header #t\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "g"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "directory:dir");
    let preview = session.preview_target(source).unwrap();
    let buffer = session.views[&preview].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff");
    };
    assert!(diff.mapped_rendering());
    assert_eq!(
        diff.index(diff::Presentation::Rendered).blocks,
        [1..=2, 4..=5]
    );
    let lines = diff
        .rendered
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            "dir/first.txt",
            "before",
            "after",
            "dir/second.txt",
            "before",
            "after"
        ]
    );
    assert!(diff.raw_row(diff::Presentation::Rendered, 3).is_none());
    let selected = session.views[&preview].location.diff_selection.row();
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
            row: (body.y + 3) as u16,
        },
    );
    assert_eq!(session.focus, preview);
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        selected
    );
    let frame = session.frame();
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::Press,
            column: body.x as u16,
            row: (body.y + 5) as u16,
        },
    );
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        unreachable!()
    };
    assert!(view.location.diff_presentation == diff::Presentation::Rendered);
    assert_eq!(
        view.location
            .diff_selection
            .rows(diff.index(view.location.diff_presentation)),
        Some(4..=5)
    );
    let (file, rows) = diff
        .selected_patch_rows(
            view.location.diff_presentation,
            &view.location.diff_selection,
        )
        .unwrap();
    assert_eq!(file.target.path, "dir/second.txt");
    assert_eq!(
        rows.iter()
            .map(|range| range.start().0..=range.end().0)
            .collect::<Vec<_>>(),
        [5..=5, 6..=6]
    );
    session.input("e");
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("source was not opened: {}", session.message);
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("dir/second.txt").as_path())
    );
    assert_eq!(text.view(editor).cursor().0, 0);
}

#[test]
fn changing_pager_arguments_and_file_headers_preserves_the_mapped_selection() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(
        &script,
        concat!(
            "cat >/dev/null\nprintf '%s\\n' \"$1\"\n",
            "printf '\\033]1717;1;d;1;1;note.txt\\007\\033]1717;1;a;1;;note.txt\\007replacement\\n'\n",
            "printf '\\033]1717;1;a;2;;note.txt\\007second addition\\n'\n",
        ),
    )
    .unwrap();
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-pager : obj 'program \"sh\" 'args : list \"{}\" \"initial\"\n",
            script.display()
        ),
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    session.input("a");
    let preview = session.focus;
    let buffer = session.views[&preview].location.buffer;
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        Some(1)
    );
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-file-header #t 'diff-pager : obj 'program \"sh\" 'args : list \"{}\" \"changed\"\n",
            script.display()
        ),
    )
    .unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    session.resize(132, 28);
    settle(&mut session);
    assert_eq!(session.focus, preview);
    let view = &session.views[&preview];
    assert_eq!(view.location.buffer, buffer);
    assert!(view.location.diff_presentation == diff::Presentation::Rendered);
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Line { .. }
    ));
    assert_eq!(view.location.diff_selection.row(), Some(2));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        unreachable!()
    };
    assert!(diff.mapped_rendering());
    assert_eq!(
        diff.rendered
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .map(|row| row
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>())
            .collect::<Vec<_>>(),
        ["note.txt", "changed", "replacement", "second addition"]
    );
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "working\nsecond addition\n"
    );
}

#[test]
fn a_mapped_change_block_can_be_unstaged_without_the_next_block() {
    let (fixture, path) = repository();
    fs::write(&path, "old\ncontext\nsecond old\n").unwrap();
    let sentinel = fixture.0.join("z-sentinel.txt");
    fs::write(&sentinel, "old\ncontext\nsecond old\n").unwrap();
    git(&fixture, &["add", "note.txt", "z-sentinel.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "two change blocks"]);
    fs::write(&path, "new\ncontext\nsecond new\n").unwrap();
    fs::write(&sentinel, "new\ncontext\nsecond new\n").unwrap();
    git(&fixture, &["add", "note.txt", "z-sentinel.txt"]);
    let output = include_str!("../../diff/tests/fixtures/delta-0.20.1-compact.ansi");
    let script = fixture.0.join(".git/renderer.sh");
    fs::write(&script, format!("cat <<'ZESS_DIFF'\n{output}ZESS_DIFF\n")).unwrap();
    let config = fixture.0.join(".git/pager-config.w");
    fs::write(
        &config,
        format!(
            "set-view-options! 'git : obj 'diff-file-header #t 'diff-pager : obj 'program \"sh\" 'args : list \"{}\"\n",
            script.display()
        ),
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    let view = &session.views[&session.focus];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        unreachable!()
    };
    assert!(diff.mapped_rendering());
    assert!(view.location.diff_presentation == diff::Presentation::Rendered);
    assert_eq!(
        diff.source,
        Some(zess_git::DiffSource::Changes {
            side: zess_git::DiffSide::Staged
        })
    );
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "old\ncontext\nsecond new"
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "new\ncontext\nsecond new\n"
    );
    assert_eq!(
        git(&fixture, &["show", ":z-sentinel.txt"]),
        "new\ncontext\nsecond new"
    );
    assert_eq!(
        fs::read_to_string(sentinel).unwrap(),
        "new\ncontext\nsecond new\n"
    );
}

#[test]
fn refreshing_a_shared_diff_cancels_search_before_remapping_its_original_selection() {
    let (fixture, path) = repository();
    fs::write(&path, "working\nsecond addition\n").unwrap();
    configure_mapped_renderer(&fixture);
    let script = fixture.0.join(".git/renderer.sh");
    let compact = fs::read_to_string(&script).unwrap();
    fs::write(
        &script,
        format!("printf 'decoration\\n%.0s' {}\n{compact}", "x ".repeat(40)),
    )
    .unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let browser = session.focus;
    session.input("Enter");
    let preview = session.focus;
    session.command("vsplit").unwrap();
    let split = session.focus;
    assert_ne!(split, preview);
    assert_eq!(
        session.views[&preview].location.buffer,
        session.views[&split].location.buffer
    );
    for key in ["a", "j"] {
        session.input(key);
    }
    assert_eq!(
        session.views[&split].location.diff_selection.row(),
        Some(42)
    );
    session.input("/");
    for key in ["w", "o", "r", "k", "i", "n", "g"] {
        session.input(key);
    }
    assert_eq!(session.prompt.as_ref().unwrap().target.view, split);
    assert_eq!(
        session.views[&split].location.diff_selection.row(),
        Some(41)
    );
    fs::write(&script, compact).unwrap();
    session.dispatch(browser, "refresh", Value::Null).unwrap();
    settle(&mut session);
    assert_eq!(session.focus, split);
    assert!(session.prompt.is_none());
    let view = &session.views[&split];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        unreachable!()
    };
    assert!(view.location.diff_presentation == diff::Presentation::Rendered);
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Line { .. }
    ));
    assert_eq!(view.location.diff_selection.row(), Some(2));
    assert_eq!(diff.raw_row(view.location.diff_presentation, 2), Some(7));
    session.input("e");
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("source was not opened: {}", session.message);
    };
    assert_eq!(text.path(), Some(path.as_path()));
    assert_eq!(text.view(editor).cursor().0, 1);
}
