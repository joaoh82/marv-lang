//! MARV-11 surface gates: bounded quantifiers (`forall`/`exists … in lo..hi: p`)
//! and `old(e)` in contract clauses round-trip through the canonical formatter
//! (`parse ∘ format == id`), and drafts normalize to the one canonical form.

use marv_syntax::{format, format_module, parse};

/// The canonical form re-parses to the identical AST and re-formats to the
/// identical text.
fn assert_canonical(text: &str) {
    let ast = parse(text).expect("canonical text parses");
    let formatted = format_module(&ast);
    assert_eq!(formatted, text, "canonical text is a formatter fixed point");
    let reparsed = parse(&formatted).expect("formatted text parses");
    assert_eq!(reparsed, ast, "parse ∘ format == id");
}

#[test]
fn forall_in_requires_is_canonical() {
    assert_canonical(
        "mod arrays\n\npure fn pick(a: [4]i64, lo: i64) -> i64\n    requires (forall i in 0..len(a): (a[i] >= lo))\n    ensures (result >= lo)\n{\n    a[2]\n}\n",
    );
}

#[test]
fn exists_with_arithmetic_domain_is_canonical() {
    assert_canonical(
        "mod arrays\n\npure fn first(a: []i64) -> i64\n    requires (len(a) >= 1)\n    ensures (exists i in 0..(len(a) - 1): (a[i] == result))\n{\n    a[0]\n}\n",
    );
}

#[test]
fn nested_quantifiers_are_canonical() {
    assert_canonical(
        "mod arrays\n\npure fn sorted(a: []i64) -> bool\n    ensures (forall i in 0..len(a): (exists j in 0..len(a): (a[j] >= a[i])))\n{\n    true\n}\n",
    );
}

#[test]
fn old_call_is_canonical() {
    assert_canonical(
        "mod contracts\n\npure fn bump(n: i64) -> i64\n    ensures (result == (old(n) + 1))\n{\n    (n + 1)\n}\n",
    );
}

#[test]
fn quantifier_in_loop_invariant_is_canonical() {
    assert_canonical(
        "mod loops\n\npure fn fill(n: i64) -> i64\n    requires (n >= 0)\n{\n    var i: i64 = 0\n    while (i < n)\n        invariant (forall k in 0..i: (k < n))\n    {\n        i = (i + 1)\n    }\n    i\n}\n",
    );
}

/// An unparenthesized draft normalizes to the parenthesized canonical form —
/// the quantifier body extends as far right as possible.
#[test]
fn draft_quantifier_normalizes() {
    let draft = "mod m\n\npure fn f(a: []i64) -> i64\n    requires forall i in 0..len(a): a[i] >= 0 and a[i] <= 9\n{\n    a[0]\n}\n";
    let once = format(draft);
    assert!(
        once.contains("requires (forall i in 0..len(a): ((a[i] >= 0) and (a[i] <= 9)))"),
        "the body is maximal-right and fully parenthesized: {once}"
    );
    // Normalization is idempotent and the result is canonical.
    assert_eq!(format(&once), once);
    assert_canonical(&once);
}

/// `..` only exists inside a quantifier domain; the two halves are ordinary
/// expressions, so a parenthesized arithmetic bound parses.
#[test]
fn quantifier_domain_accepts_compound_bounds() {
    let text = "mod m\n\npure fn f(a: []i64, k: i64) -> bool\n    ensures (forall i in (k - 1)..(k + 1): (a[i] == 0))\n{\n    true\n}\n";
    assert_canonical(text);
}
