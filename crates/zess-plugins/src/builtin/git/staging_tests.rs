use super::*;

#[test]
fn pending_line_counts_are_distinct_from_completed_unknown_counts() {
    let mut tree = GitTree::default();
    let entry = GitEntry {
        path: "binary.bin".into(),
        original_path: None,
        index: " ".into(),
        worktree: "M".into(),
        untracked: false,
        line_stats: None,
    };
    tree.rebuild(
        vec![TreeFile::Changed(entry.clone())],
        GitTreeMode::Flat,
        1,
        &Filter::default(),
        LineCountState::Loading,
    );
    let row = &tree.nodes["file:binary.bin"].row;
    assert_eq!(row.cells[1].spans[0].text, "+…");
    assert_eq!(row.cells[2].spans[0].text, "-…");
    tree.rebuild(
        vec![TreeFile::Changed(entry)],
        GitTreeMode::Flat,
        2,
        &Filter::default(),
        LineCountState::Ready,
    );
    assert_eq!(
        tree.nodes["file:binary.bin"].row.cells[1].spans[0].text,
        "uncounted"
    );
}

#[test]
fn changed_files_show_index_and_worktree_status_separately_from_names_and_counts() {
    for (index, worktree, style) in [
        ("M", " ", "modified"),
        (" ", "M", "modified"),
        ("M", "M", "modified"),
        ("U", "U", "error"),
    ] {
        let mut tree = GitTree::default();
        tree.rebuild(
            vec![TreeFile::Changed(GitEntry {
                path: "folder/note.txt".into(),
                original_path: None,
                index: index.into(),
                worktree: worktree.into(),
                untracked: index == "?",
                line_stats: Some(LineStats {
                    additions: 3,
                    deletions: 2,
                }),
            })],
            GitTreeMode::Tree,
            1,
            &Filter::default(),
            LineCountState::Ready,
        );
        let row = &tree.nodes["file:folder/note.txt"].row;
        assert_eq!(row.text, "note.txt");
        assert_eq!(row.depth, 1);
        assert_eq!(row.style, style);
        assert_eq!(row.cells.len(), 3);
        assert_eq!(row.cells[0].spans[0].text, index);
        assert_eq!(row.cells[0].spans[1].text, worktree);
        assert_eq!(
            row.cells[0].spans[0].style,
            if index == "U" { "error" } else { "git.added" }
        );
        assert_eq!(
            row.cells[0].spans[1].style,
            if worktree == "U" {
                "error"
            } else {
                "git.deleted"
            }
        );
        assert_eq!(row.cells[1].spans[0].text, "+3");
        assert_eq!(row.cells[2].spans[0].text, "-2");
    }
}

#[test]
fn untracked_files_keep_their_names_and_counts_without_question_marks() {
    let mut tree = GitTree::default();
    tree.rebuild(
        vec![TreeFile::Changed(GitEntry {
            path: "new.txt".into(),
            original_path: None,
            index: "?".into(),
            worktree: "?".into(),
            untracked: true,
            line_stats: Some(LineStats {
                additions: 3,
                deletions: 0,
            }),
        })],
        GitTreeMode::Tree,
        1,
        &Filter::default(),
        LineCountState::Ready,
    );
    let row = &tree.nodes["file:new.txt"].row;
    assert_eq!(row.text, "new.txt");
    assert_eq!(row.style, "git.added");
    assert!(row.cells[0].spans.is_empty());
    assert_eq!(row.cells[1].spans[0].text, "+3");
}

#[test]
fn directory_targets_follow_the_filtered_tree_beyond_collapsed_children() {
    let mut tree = GitTree::default();
    let files = [
        ("dir/nested/keep.txt", " ", "M", None),
        ("dir/keep.txt", "R", "M", Some("dir/old\tname\n.txt")),
        ("dir/skip.txt", "M", " ", None),
    ]
    .into_iter()
    .map(|(path, index, worktree, original)| {
        TreeFile::Changed(GitEntry {
            path: path.into(),
            original_path: original.map(str::to_owned),
            index: index.into(),
            worktree: worktree.into(),
            untracked: false,
            line_stats: None,
        })
    })
    .collect();
    tree.rebuild(
        files,
        GitTreeMode::Tree,
        1,
        &Filter::compile(
            &zess_filter::Query {
                kind: zess_filter::QueryKind::Plain,
                text: "keep".into(),
            },
            Case::Sensitive,
        )
        .unwrap(),
        LineCountState::Ready,
    );
    tree.collapsed.insert("dir/nested".into());
    let targets = tree.file_targets("directory:dir", Some(DiffSide::Unstaged));
    assert_eq!(
        targets
            .iter()
            .map(|target| target.path.as_str())
            .collect::<Vec<_>>(),
        ["dir/nested/keep.txt", "dir/keep.txt"]
    );
    assert_eq!(
        targets[1].original_path.as_deref(),
        Some("dir/old\tname\n.txt")
    );
    let staged = tree.file_targets("directory:dir", Some(DiffSide::Staged));
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].path, "dir/keep.txt");
}
