use marv_core::{lower_module, DefKind, LowerError};
use marv_syntax::parse;

fn lower(src: &str) -> Result<marv_core::LoweredModule, LowerError> {
    let module = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n--- source ---\n{src}"));
    lower_module(&module)
}

#[test]
fn unsafe_extern_fn_lowers_to_bodyless_signature() {
    let module = lower(
        "\
mod demo

/// SAFETY: the host symbol follows the marv i64 ABI.
unsafe extern fn host_add_one(x: i64) -> i64
",
    )
    .expect("lower unsafe extern declaration");

    assert_eq!(module.defs.len(), 1);
    let def = &module.defs[0].def;
    assert_eq!(def.kind, DefKind::Fn);
    assert!(def.body.is_none(), "extern declarations have no Core body");
}

#[test]
fn safe_function_cannot_directly_call_ffi_declaration() {
    let err = lower(
        "\
mod demo

/// SAFETY: the host symbol follows the marv i64 ABI.
unsafe extern fn host_add_one(x: i64) -> i64

fn accidental(x: i64) -> i64 {
    host_add_one(x)
}
",
    )
    .unwrap_err();

    assert_eq!(
        err,
        LowerError::UnsafeFfiOutsideUnsafe {
            name: "demo.host_add_one".to_string()
        }
    );
}

#[test]
fn unsafe_function_can_hold_direct_ffi_call_boundary() {
    let module = lower(
        "\
mod demo

/// SAFETY: the host symbol follows the marv i64 ABI.
unsafe extern fn host_add_one(x: i64) -> i64

/// SAFETY: callers use this audited wrapper to cross the host boundary.
unsafe fn call_host(x: i64) -> i64 {
    host_add_one(x)
}
",
    )
    .expect("lower audited FFI wrapper");

    assert_eq!(module.defs.len(), 2);
    assert!(module.defs[0].def.body.is_none());
    assert!(module.defs[1].def.body.is_some());
}
