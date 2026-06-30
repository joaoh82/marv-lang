//! MARV-52 iterator-protocol lowering.
//!
//! Existing arrays/slices/strings/List keep the direct `len`/index `for` fast
//! path. `std.iter.IndexIter[T]` is the first protocol-backed iterable: lowering
//! rewrites `for x in it` through the generic `std.iter.iter_len` /
//! `std.iter.iter_get` wrappers, whose specialized bodies dispatch through the
//! `Iter[T]` interface implementation.

use marv_core::ir::*;
use marv_core::{lower_modules, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower_with_std_iter(src: &str) -> Vec<LoweredModule> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    let collections = parse(&std::fs::read_to_string(root.join("std/collections.mv")).unwrap())
        .expect("parse std.collections");
    let iter =
        parse(&std::fs::read_to_string(root.join("std/iter.mv")).unwrap()).expect("parse std.iter");
    let app = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_modules(&[collections, iter, app]).expect("lower std + app")
}

fn module<'a>(mods: &'a [LoweredModule], name: &[&str]) -> &'a LoweredModule {
    mods.iter()
        .find(|m| m.module.iter().map(String::as_str).eq(name.iter().copied()))
        .unwrap_or_else(|| panic!("no module {name:?}"))
}

fn def<'a>(m: &'a LoweredModule, name: &str) -> &'a DefEntry {
    m.defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def `{name}`"))
}

#[test]
fn for_over_index_iter_uses_iter_protocol_wrappers() {
    let mods = lower_with_std_iter(
        "\
mod app

import std.io (Alloc)
import std.collections (List)
import std.iter (IndexIter, from_list)

fn exercise(alloc: Alloc) -> i64 {
    let xs: List[i64] = List { alloc: alloc, items: [2, 4, 6] }
    let it: IndexIter[i64] = from_list(xs)
    var total: i64 = 0
    for x in it {
        total = (total + x)
    }
    total
}
",
    );
    let app = module(&mods, &["app"]);
    let body = def(app, "exercise").def.body.clone().expect("fn body");
    let globals = globals(&body);

    assert!(
        globals.contains(&marv_core::symbol_hash("std.iter.iter_len@i64")),
        "for over IndexIter should call the specialized iter_len wrapper"
    );
    assert!(
        globals.contains(&marv_core::symbol_hash("std.iter.iter_get@i64")),
        "for over IndexIter should call the specialized iter_get wrapper"
    );

    let iter_mod = module(&mods, &["std", "iter"]);
    assert!(
        iter_mod.defs.iter().any(|d| d.name == "iter_len@i64"),
        "iter_len@i64 instance should be generated in std.iter"
    );
    assert!(
        iter_mod.defs.iter().any(|d| d.name == "iter_get@i64"),
        "iter_get@i64 instance should be generated in std.iter"
    );
}

fn globals(c: &Core) -> Vec<Hash> {
    let mut out = Vec::new();
    walk(c, &mut out);
    out
}

fn walk(c: &Core, out: &mut Vec<Hash>) {
    let mut atom = |a: &Atom| {
        if let Atom::Global(h) = a {
            out.push(*h);
        }
    };
    match c {
        Core::Atom(a) => atom(a),
        Core::Let { value, body } => {
            walk(value, out);
            walk(body, out);
        }
        Core::Lam { body, .. } => walk(body, out),
        Core::App { func, arg } => {
            atom(func);
            atom(arg);
        }
        Core::Ctor { fields, .. } | Core::Array { items: fields, .. } => {
            fields.iter().for_each(atom);
        }
        Core::IndexSet { base, index, value }
        | Core::ListSet {
            list: base,
            index,
            value,
        } => {
            atom(base);
            atom(index);
            atom(value);
        }
        Core::ListNew {
            alloc, capacity, ..
        } => {
            atom(alloc);
            atom(capacity);
        }
        Core::ListPush { alloc, list, value } => {
            atom(alloc);
            atom(list);
            atom(value);
        }
        Core::ListPop { list } => atom(list),
        Core::Proj { base, .. } => atom(base),
        Core::Match {
            scrutinee,
            branches,
        } => {
            atom(scrutinee);
            branches.iter().for_each(|b| walk(&b.body, out));
        }
        Core::Prim { args, .. } => args.iter().for_each(atom),
        Core::Cast { value, .. } | Core::Ref { of: value, .. } | Core::Return { value } => {
            atom(value)
        }
        Core::Perform { cap, args, .. } => {
            atom(cap);
            args.iter().for_each(atom);
        }
        Core::Raise { args, .. } => args.iter().for_each(atom),
        Core::Loop {
            state, cond, body, ..
        } => {
            state.iter().for_each(atom);
            walk(cond, out);
            walk(body, out);
        }
    }
}
