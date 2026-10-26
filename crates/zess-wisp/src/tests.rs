use hegel::{TestCase, generators as gs};

use super::read;

#[test]
fn indentation_and_inline_colons_form_scheme_lists() {
    for (source, scheme) in [
        (
            "define : square x\n  * x x\n",
            "(define (square x) (* x x))",
        ),
        ("display : + 1 2\n", "(display (+ 1 2))"),
        ("list\n  . 1 2\n  + 3 4\n", "(list 1 2 (+ 3 4))"),
        (". '(a b)\n", "'(a b)"),
    ] {
        assert_eq!(read(source).unwrap(), scheme, "{source:?}");
    }
}

#[test]
fn explicit_datums_and_comments_preserve_their_scheme_meaning() {
    for (source, scheme) in [
        (
            "display \"a: (b); c\" ; ignored\n",
            "(display \"a: (b); c\")",
        ),
        ("list [1 2] {1 + 2 + 3}\n", "(list (1 2) (+ 1 2 3))"),
        (
            "list #; (ignored datum) #| outer #| inner |# |# 1\n",
            "(list 1)",
        ),
        ("list\n  . \"first\nsecond\"\n", "(list \"first\nsecond\")"),
    ] {
        assert_eq!(read(source).unwrap(), scheme, "{source:?}");
    }
}

#[hegel::test(derandomize = true)]
fn bounded_syntax_fragments_do_not_panic(tc: TestCase) {
    let fragments = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            "a", "λ", "界", " ", "  ", "\n", "\r\n", "\t", "_", ":", ".", "\\:", "'", "`", ",",
            ",@", "\"", "\\", "|", ";comment", "#|", "|#", "#;", "(", ")", "[", "]", "{", "}",
            "#(", "#u8(", "#\\a",
        ]))
        .max_size(64),
    );
    let source = fragments.concat();
    let _ = read(&source);
}

#[hegel::test(derandomize = true)]
fn layout_forms_preserve_explicit_scheme_datums(tc: TestCase) {
    let head = tc.draw(gs::sampled_from(vec!["list", "collect", "λ"]));
    let atoms = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            "0",
            "-1",
            "#t",
            "name",
            "界",
            "λ",
            "'quoted",
            "'(a b)",
            "\"a: (b); c\"",
            "\"界 λ\"",
            "(nested 1 2)",
            "#(1 2)",
            "#u8(1 2)",
            "#\\space",
        ]))
        .max_size(8),
    );
    let tail = atoms
        .iter()
        .map(|atom| format!(" {atom}"))
        .collect::<String>();
    let children = atoms
        .iter()
        .map(|atom| format!("  . {atom}\n"))
        .collect::<String>();
    for (source, expected) in [
        (format!("{head}{tail}\n"), format!("({head}{tail})")),
        (format!("{head}\n{children}"), format!("({head}{tail})")),
        (
            format!("{head} : list{tail}\n"),
            format!("({head} (list{tail}))"),
        ),
    ] {
        assert_eq!(read(&source).unwrap(), expected, "{source:?}");
        let commented = format!("\n; ignored : ( ' [\n\n{source}");
        assert_eq!(read(&commented).unwrap(), expected, "{commented:?}");
    }
}

#[test]
fn source_locations_follow_datums_through_layout_and_infix_reordering() {
    let source =
        "; 前言\ndefine : λ value\n  list \"界\" : list {value + 例}\n  . [named \'(one two)]\n";
    let translation = super::read_with_source_map(source).unwrap();
    for token in ["define", "λ", "\"界\"", "+", "例", "named", "one", "two"] {
        let generated = translation.scheme.find(token).unwrap();
        let original = source.find(token).unwrap();
        for offset in 0..token.len() {
            assert_eq!(
                translation.source_map.original_offset(generated + offset),
                Some(original + offset),
                "{token}"
            );
        }
    }
    let generated = translation.scheme.find("(+").unwrap();
    assert_eq!(
        translation.source_map.original_offset(generated),
        Some(source.find('+').unwrap())
    );
}
