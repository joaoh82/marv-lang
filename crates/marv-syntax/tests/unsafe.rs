use marv_syntax::{format, parse};

#[test]
fn unsafe_fn_requires_safety_doc_comment() {
    let err = parse("mod demo\n\nunsafe fn raw() -> i64 {\n    0\n}\n").unwrap_err();
    assert!(
        err.to_string().contains("SAFETY"),
        "expected missing SAFETY diagnostic, got {err}"
    );
}

#[test]
fn unsafe_extern_fn_requires_safety_doc_comment() {
    let err = parse("mod demo\n\nunsafe extern fn host_add_one(x: i64) -> i64\n").unwrap_err();
    assert!(
        err.to_string().contains("SAFETY"),
        "expected missing SAFETY diagnostic, got {err}"
    );
}

#[test]
fn extern_fn_must_be_unsafe() {
    let err = parse("mod demo\n\nextern fn host_add_one(x: i64) -> i64\n").unwrap_err();
    assert!(
        err.to_string().contains("unsafe extern fn"),
        "expected unsafe extern diagnostic, got {err}"
    );
}

#[test]
fn unsafe_extern_fn_round_trips_with_safety_doc() {
    let src = "\
mod demo

/// SAFETY: the host symbol uses the marv i64 ABI and has no side effects.
unsafe extern fn host_add_one(x: i64) -> i64

/// SAFETY: this wrapper is the audited boundary around host_add_one.
unsafe fn call_host(x: i64) -> i64 {
    host_add_one(x)
}
";
    let module = parse(src).expect("parse unsafe extern fn");
    assert_eq!(format(src), src);
    assert_eq!(format(&format(src)), src);
    assert_eq!(module.items.len(), 2);
}

#[test]
fn unsafe_fn_round_trips_with_safety_doc() {
    let src = "\
mod demo

/// SAFETY: callers uphold the raw boundary documented by the host.
unsafe fn raw() -> i64 {
    0
}

/// SAFETY: this pure function is marked unsafe only for auditability.
pure unsafe fn audited_identity(x: i64) -> i64 {
    x
}
";
    let module = parse(src).expect("parse unsafe fn");
    assert_eq!(format(src), src);
    assert_eq!(format(&format(src)), src);
    assert_eq!(module.items.len(), 2);
}
