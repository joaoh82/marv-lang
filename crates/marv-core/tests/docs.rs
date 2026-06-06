//! Doc comments are excluded from a definition's content hash (`spec/02` §F —
//! spans/comments are not part of identity). Adding, changing, or removing a
//! `///` doc block must not change the Core hash of the definition it documents
//! (MARV-12).

use marv_core::lower_module;
use marv_syntax::parse;

fn hash_of(src: &str, name: &str) -> String {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    let lowered = lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"));
    lowered
        .defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def `{name}`"))
        .hash
        .to_hex()
}

#[test]
fn doc_comments_do_not_change_the_content_hash() {
    let undocumented = "mod m\n\nfn f(x: i32) -> i32 {\n    x\n}\n";
    let documented = "mod m\n\n/// Identity on i32.\n///\n/// Second paragraph.\nfn f(x: i32) -> i32 {\n    x\n}\n";
    assert_eq!(
        hash_of(undocumented, "f"),
        hash_of(documented, "f"),
        "doc comments must not be part of the Core identity"
    );
}

#[test]
fn changing_doc_text_does_not_change_the_hash() {
    let a = "mod m\n\n/// One wording.\nfn f() {}\n";
    let b = "mod m\n\n/// A completely different wording entirely.\nfn f() {}\n";
    assert_eq!(hash_of(a, "f"), hash_of(b, "f"));
}
