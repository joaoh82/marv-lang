//! Capabilities and `perform` from real `.mv` source (parse → lower →
//! [`World::from_module`] → [`check_def`]), MARV-6 / `spec/01` §5.
//!
//! A capability is a *non-generic* `interface`. A method call on a value of such
//! a type lowers to [`Core::Perform`] (a narrowing op like `io.fs()` yields a
//! narrowed capability value); the effect row is inferred from those sites and
//! checked against the declared row — the set of capability parameters, with
//! narrowing authorizing what a held capability can attenuate to. A `pure fn`
//! (empty row) that nonetheless performs is `MissingCapability` (E0110).

use marv_core::ir::Core;
use marv_core::{lower_module, LoweredModule};
use marv_syntax::parse;
use marv_types::{check_def, Code, Severity, World};

const SRC: &str = "\
mod demo

interface Io {
    fn fs(io: &Io) -> Fs
    fn stdout(io: &Io) -> Stream
}

interface Fs {
    fn read(fs: &Fs, path: str) -> ![]u8
}

interface Stream {
    fn write(s: &Stream, text: str) -> !
}

fn use_fs(fs: Fs, path: str) -> ![]u8 {
    fs.read(path)
}

fn narrow(io: Io, path: str) -> ![]u8 {
    let fs = io.fs()
    fs.read(path)
}

pure fn leak(fs: Fs, path: str) -> ![]u8 {
    fs.read(path)
}
";

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}"))
}

fn body<'a>(m: &'a LoweredModule, name: &str) -> &'a Core {
    m.defs
        .iter()
        .find(|d| d.name == name)
        .and_then(|d| d.def.body.as_ref())
        .unwrap_or_else(|| panic!("no body for `{name}`"))
}

/// Count every [`Core::Perform`] in a Core term.
fn performs(c: &Core) -> usize {
    let here = matches!(c, Core::Perform { .. }) as usize;
    let kids = match c {
        Core::Let { value, body } => performs(value) + performs(body),
        Core::Lam { body, .. } => performs(body),
        Core::Match { branches, .. } => branches.iter().map(|b| performs(&b.body)).sum(),
        Core::Loop { cond, body, .. } => performs(cond) + performs(body),
        // Every other node carries only atomic children (no nested Core terms).
        _ => 0,
    };
    here + kids
}

fn error_codes(world: &World, m: &LoweredModule, name: &str) -> Vec<Code> {
    let def = &m.defs.iter().find(|d| d.name == name).unwrap().def;
    check_def(world, def, Some(name))
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn non_generic_interface_is_a_capability() {
    let m = lower(SRC);
    for name in ["Io", "Fs", "Stream"] {
        let iface = m.interfaces.iter().find(|i| i.name == name).unwrap();
        assert!(iface.is_capability, "{name} should be a capability");
    }
}

#[test]
fn capability_method_call_lowers_to_perform() {
    let m = lower(SRC);
    assert_eq!(performs(body(&m, "use_fs")), 1, "fs.read is one perform");
}

#[test]
fn narrowing_lowers_to_two_performs() {
    let m = lower(SRC);
    // `io.fs()` (narrow) then `fs.read(path)` (use).
    assert_eq!(performs(body(&m, "narrow")), 2);
}

#[test]
fn receiving_the_capability_checks_clean() {
    let m = lower(SRC);
    let world = World::from_module(&m);
    assert!(error_codes(&world, &m, "use_fs").is_empty());
}

#[test]
fn narrowing_a_held_capability_is_authorized() {
    let m = lower(SRC);
    let world = World::from_module(&m);
    // `narrow` never receives `Fs` as a parameter, but narrows it from the `Io`
    // it holds — so no E0110 for `Fs` (`spec/01` §5 attenuation).
    assert!(error_codes(&world, &m, "narrow").is_empty());
}

#[test]
fn pure_fn_that_performs_is_missing_capability() {
    let m = lower(SRC);
    let world = World::from_module(&m);
    // `pure fn leak` asserts the empty row but performs `Fs` → E0110.
    assert_eq!(
        error_codes(&world, &m, "leak"),
        vec![Code::MissingCapability]
    );
}
