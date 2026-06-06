//! Doc-comment preservation (MARV-12).
//!
//! `///` doc comments attach to the item that follows them, survive the
//! canonical formatter, and normalize to a single `/// text` spelling (one
//! leading space, trailing whitespace trimmed). `//` and `////…` stay ordinary
//! comments and are dropped. Doc comments are *not* part of a definition's
//! identity — that property is verified in `marv-core` (hashes are unchanged by
//! docs); here we cover the syntax layer.

use marv_syntax::{format, parse, Item};

#[test]
fn doc_comments_attach_to_the_following_item() {
    let src = "mod m\n\n/// One.\n/// Two.\nfn f() {}\n";
    let module = parse(src).expect("parses");
    let Item::Fn(f) = &module.items[0] else {
        panic!("expected a fn item");
    };
    assert_eq!(f.docs, vec!["One.".to_string(), "Two.".to_string()]);
}

#[test]
fn formatter_round_trips_doc_comments() {
    let canonical =
        "mod m\n\n/// A documented struct.\nstruct S {}\n\n/// A documented fn.\nfn f() {}\n";
    assert_eq!(format(canonical), canonical, "doc comments must round-trip");
    // Idempotent.
    assert_eq!(format(&format(canonical)), format(canonical));
}

#[test]
fn doc_comment_text_is_normalized() {
    // No leading space, extra leading space, and trailing whitespace all
    // canonicalize to a single `/// text` form.
    let messy = "mod m\n\n///no space\n///  two spaces\n///trailing   \nfn f() {}\n";
    let expected = "mod m\n\n/// no space\n///  two spaces\n/// trailing\nfn f() {}\n";
    assert_eq!(format(messy), expected);
}

#[test]
fn blank_doc_line_renders_as_bare_triple_slash() {
    let src = "mod m\n\n/// Head.\n///\n/// Tail.\nfn f() {}\n";
    let module = parse(src).expect("parses");
    let Item::Fn(f) = &module.items[0] else {
        panic!("expected a fn");
    };
    assert_eq!(
        f.docs,
        vec!["Head.".to_string(), "".to_string(), "Tail.".to_string()]
    );
    assert_eq!(format(src), src);
}

#[test]
fn ordinary_and_quad_slash_comments_are_dropped() {
    // `//` and `////` are not doc comments and carry no AST text.
    let src = "mod m\n\n// regular\n//// four slashes\nfn f() {}\n";
    let module = parse(src).expect("parses");
    let Item::Fn(f) = &module.items[0] else {
        panic!("expected a fn");
    };
    assert!(
        f.docs.is_empty(),
        "non-doc comments must not attach as docs"
    );
}

#[test]
fn enum_and_error_decls_carry_docs() {
    // Enums format one variant per line; the doc block precedes the header.
    let src =
        "mod m\n\n/// An enum.\nenum E {\n    A,\n    B,\n}\n\n/// An error.\nerror Oops { Bad }\n";
    let module = parse(src).expect("parses");
    match &module.items[0] {
        Item::Enum(e) => assert_eq!(e.docs, vec!["An enum.".to_string()]),
        _ => panic!("expected enum"),
    }
    match &module.items[1] {
        Item::Error(e) => assert_eq!(e.docs, vec!["An error.".to_string()]),
        _ => panic!("expected error"),
    }
    assert_eq!(format(src), src);
}
