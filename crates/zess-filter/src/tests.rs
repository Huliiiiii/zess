use super::*;

#[test]
fn plain_queries_remain_literal_substrings_with_the_requested_case() {
    for (source, candidate, sensitive, insensitive) in [
        ("File", "src/File.rs", true, true),
        ("file", "src/File.rs", false, true),
        ("ÉCOLE", "école.rs", false, true),
        ("[a]*", "prefix[a]*suffix", true, true),
        ("re:foo", "prefix re:foo suffix", true, true),
        ("re:foo", "foo", false, false),
        ("glob:*.rs", "glob:*.rs", true, true),
        ("glob:*.rs", "main.rs", false, false),
        ("", "anything", true, true),
        ("fb", "foo/bar", false, false),
    ] {
        for (case, expected) in [
            (Case::Sensitive, sensitive),
            (Case::Insensitive, insensitive),
        ] {
            let filter = Filter::compile(
                &Query {
                    kind: QueryKind::Plain,
                    text: source.into(),
                },
                case,
            )
            .unwrap();
            assert_eq!(filter.source(), source);
            assert!(matches!(filter.query().kind, QueryKind::Plain));
            assert_eq!(
                filter.is_match(candidate),
                expected,
                "{source}: {candidate}"
            );
        }
    }
}

#[test]
fn glob_stars_respect_path_segments_and_recursive_stars_include_root_files() {
    for (source, matching, missing) in [
        (
            "*.rs",
            vec!["main.rs", "école.rs"],
            vec!["src/main.rs", "main.txt"],
        ),
        (
            "**/*.rs",
            vec!["main.rs", "src/deep/main.rs"],
            vec!["main.txt"],
        ),
        (
            "src/*.{rs,w}",
            vec!["src/main.rs", "src/outline.w"],
            vec!["src/deep/main.rs", "main.rs"],
        ),
        (
            "src/[ab]?.rs",
            vec!["src/a1.rs", "src/b2.rs"],
            vec!["src/c1.rs", "src/a12.rs"],
        ),
    ] {
        let filter = Filter::compile(
            &Query {
                kind: QueryKind::Glob,
                text: source.into(),
            },
            Case::Sensitive,
        )
        .unwrap();
        for candidate in matching {
            assert!(filter.is_match(candidate), "{source}: {candidate}");
        }
        for candidate in missing {
            assert!(!filter.is_match(candidate), "{source}: {candidate}");
        }
    }
    let filter = Filter::compile(
        &Query {
            kind: QueryKind::Glob,
            text: "ÉCOLE/*.RS".into(),
        },
        Case::Insensitive,
    )
    .unwrap();
    assert!(filter.is_match("école/été.rs"));
}

#[test]
fn regexes_keep_their_syntax_and_support_unicode_case_matching() {
    let filter = Filter::compile(
        &Query {
            kind: QueryKind::Regex,
            text: r"^\p{L}+\.rs$".into(),
        },
        Case::Sensitive,
    )
    .unwrap();
    assert!(filter.is_match("école.rs"));
    assert!(!filter.is_match("src/école.rs"));
    let filter = Filter::compile(
        &Query {
            kind: QueryKind::Regex,
            text: r"^ÉCOLE\S+$".into(),
        },
        Case::Insensitive,
    )
    .unwrap();
    assert!(filter.is_match("école.rs"));
    assert!(!filter.is_match("école rs"));
    for kind in [QueryKind::Regex, QueryKind::Glob] {
        let error = Filter::compile(
            &Query {
                kind,
                text: "[".into(),
            },
            Case::Sensitive,
        )
        .err()
        .unwrap();
        assert!(!error.to_string().contains('\n'));
    }
}

#[test]
fn regexes_match_anywhere_unless_the_pattern_has_anchors() {
    let filter = Filter::compile(
        &Query {
            kind: QueryKind::Regex,
            text: "foo".into(),
        },
        Case::Sensitive,
    )
    .unwrap();
    assert!(filter.is_match("prefix foo suffix"));
    let filter = Filter::compile(
        &Query {
            kind: QueryKind::Regex,
            text: "^foo$".into(),
        },
        Case::Sensitive,
    )
    .unwrap();
    assert!(!filter.is_match("prefix foo suffix"));
    assert!(filter.is_match("foo"));
}
