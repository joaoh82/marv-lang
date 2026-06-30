//! MARV-51 collection literal lowering.
//!
//! `List { alloc: a, items: [...] }` is deliberately explicit sugar for
//! `ListNew` followed by one `ListPush` per item. The allocation capability is
//! present at the literal site; omitting it is accepted by the parser only so
//! lowering can report a targeted error.

use marv_core::ir::*;
use marv_core::lower::LowerError;
use marv_core::{lower_module, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"))
}

fn try_lower(src: &str) -> Result<LoweredModule, LowerError> {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m)
}

fn def<'a>(m: &'a LoweredModule, name: &str) -> &'a DefEntry {
    m.defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def `{name}`"))
}

fn fn_body(m: &LoweredModule, name: &str) -> Core {
    let mut body = def(m, name).def.body.clone().expect("fn body");
    while let Core::Lam { body: inner, .. } = body {
        body = *inner;
    }
    body
}

#[derive(Default)]
struct ListCounts {
    new: usize,
    pushes: usize,
    new_capacity: Option<Atom>,
    push_values: Vec<Atom>,
}

fn count_lists(c: &Core, counts: &mut ListCounts) {
    match c {
        Core::ListNew { capacity, .. } => {
            counts.new += 1;
            counts.new_capacity = Some(capacity.clone());
        }
        Core::ListPush { value, .. } => {
            counts.pushes += 1;
            counts.push_values.push(value.clone());
        }
        _ => {}
    }

    match c {
        Core::Let { value, body } => {
            count_lists(value, counts);
            count_lists(body, counts);
        }
        Core::Lam { body, .. } | Core::Loop { body, .. } => count_lists(body, counts),
        Core::Match { branches, .. } => {
            for branch in branches {
                count_lists(&branch.body, counts);
            }
        }
        _ => {}
    }
}

#[test]
fn list_literal_lowers_to_new_then_pushes() {
    let src = "\
mod demo

import std.io (Alloc)
import std.collections (List)

fn make(alloc: Alloc) -> List[i64] {
    List { alloc: alloc, items: [3, 5, 8] }
}
";
    let body = fn_body(&lower(src), "make");
    let mut counts = ListCounts::default();
    count_lists(&body, &mut counts);

    assert_eq!(counts.new, 1, "literal allocates exactly one list header");
    assert_eq!(counts.pushes, 3, "one push per literal item");
    assert_eq!(
        counts.new_capacity,
        Some(Atom::Lit(Literal::Int(3))),
        "initial capacity matches the item count"
    );
    assert_eq!(
        counts.push_values,
        vec![
            Atom::Lit(Literal::Int(3)),
            Atom::Lit(Literal::Int(5)),
            Atom::Lit(Literal::Int(8)),
        ],
        "items are pushed in source order"
    );
}

#[test]
fn list_literal_requires_explicit_alloc() {
    let src = "\
mod demo

import std.collections (List)

fn make() -> List[i64] {
    List { items: [1] }
}
";
    assert!(matches!(
        try_lower(src),
        Err(LowerError::CollectionLiteralMissingAlloc { kind: "List" })
    ));
}

#[test]
fn set_literal_lowers_with_explicit_alloc() {
    let src = "\
mod demo

import std.io (Alloc)
import std.collections (Set)

fn make(alloc: Alloc) -> Set[str] {
    Set { alloc: alloc, items: [\"red\", \"blue\"] }
}
";
    let body = fn_body(&lower(src), "make");
    let globals = globals(&body);
    assert!(
        globals.contains(&marv_core::symbol_hash("std.collections.set_with_capacity")),
        "set literal should call set_with_capacity"
    );
    assert!(
        globals.contains(&marv_core::symbol_hash("std.collections.set_insert")),
        "set literal should insert each item through the public set operation"
    );
}

#[test]
fn map_literal_lowers_to_backing_entries() {
    let src = "\
mod demo

import std.io (Alloc)
import std.collections (Map)

fn make(alloc: Alloc) -> Map[str, i64] {
    Map { alloc: alloc, keys: [\"red\", \"blue\"], values: [1, 2] }
}
";
    let body = fn_body(&lower(src), "make");
    let mut counts = ListCounts::default();
    count_lists(&body, &mut counts);

    assert_eq!(
        counts.new, 1,
        "map literal allocates one backing entry list"
    );
    assert_eq!(counts.pushes, 2, "one entry per key/value pair");
    assert_eq!(
        counts.new_capacity,
        Some(Atom::Lit(Literal::Int(2))),
        "entry-list capacity matches the key/value pair count"
    );
    assert!(
        has_ctor(&body, marv_core::symbol_hash("std.collections.Map")),
        "map literal should construct the std Map value"
    );
    assert!(
        has_ctor(&body, marv_core::symbol_hash("std.collections.Entry")),
        "map literal should construct std Entry values"
    );
    assert!(
        has_entry_hash_sentinel(&body),
        "map literal entries should carry the hash-0 compatibility sentinel"
    );
}

#[test]
fn collection_literals_require_explicit_alloc() {
    let set_src = "\
mod demo

import std.collections (Set)

fn make() -> Set[str] {
    Set { items: [\"red\"] }
}
";
    assert!(matches!(
        try_lower(set_src),
        Err(LowerError::CollectionLiteralMissingAlloc { kind: "Set" })
    ));

    let map_src = "\
mod demo

import std.collections (Map)

fn make() -> Map[str, i64] {
    Map { keys: [\"red\"], values: [1] }
}
";
    assert!(matches!(
        try_lower(map_src),
        Err(LowerError::CollectionLiteralMissingAlloc { kind: "Map" })
    ));
}

#[test]
fn map_literal_rejects_mismatched_keys_and_values() {
    let src = "\
mod demo

import std.io (Alloc)
import std.collections (Map)

fn make(alloc: Alloc) -> Map[str, i64] {
    Map { alloc: alloc, keys: [\"red\", \"blue\"], values: [1] }
}
";
    assert!(matches!(
        try_lower(src),
        Err(LowerError::MapLiteralLengthMismatch { keys: 2, values: 1 })
    ));
}

fn globals(c: &Core) -> Vec<Hash> {
    let mut out = Vec::new();
    collect_globals(c, &mut out);
    out
}

fn collect_globals(c: &Core, out: &mut Vec<Hash>) {
    match c {
        Core::Atom(Atom::Global(h)) => out.push(*h),
        Core::App { func, arg } => {
            if let Atom::Global(h) = func {
                out.push(*h);
            }
            if let Atom::Global(h) = arg {
                out.push(*h);
            }
        }
        _ => {}
    }

    match c {
        Core::Let { value, body } => {
            collect_globals(value, out);
            collect_globals(body, out);
        }
        Core::Lam { body, .. } | Core::Loop { body, .. } => collect_globals(body, out),
        Core::Match { branches, .. } => {
            for branch in branches {
                collect_globals(&branch.body, out);
            }
        }
        _ => {}
    }
}

fn has_ctor(c: &Core, ty: Hash) -> bool {
    match c {
        Core::Ctor { ty: found, .. } if *found == ty => true,
        Core::Let { value, body } => has_ctor(value, ty) || has_ctor(body, ty),
        Core::Lam { body, .. } | Core::Loop { body, .. } => has_ctor(body, ty),
        Core::Match { branches, .. } => branches.iter().any(|b| has_ctor(&b.body, ty)),
        _ => false,
    }
}

fn has_entry_hash_sentinel(c: &Core) -> bool {
    match c {
        Core::Ctor { ty, fields, .. } if *ty == marv_core::symbol_hash("std.collections.Entry") => {
            fields.first() == Some(&Atom::Lit(Literal::Int(0)))
        }
        Core::Let { value, body } => {
            has_entry_hash_sentinel(value) || has_entry_hash_sentinel(body)
        }
        Core::Lam { body, .. } | Core::Loop { body, .. } => has_entry_hash_sentinel(body),
        Core::Match { branches, .. } => branches.iter().any(|b| has_entry_hash_sentinel(&b.body)),
        _ => false,
    }
}
