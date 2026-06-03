//! Core-IR snapshot ingestion (`spec/03` §3.1, the agent-facing `openSnapshot`).
//!
//! A workspace file is normally marv *source* (`.mv`), which the front end
//! parses and lowers. But three of the six Tier-0 checks — capabilities, error
//! sets, exhaustiveness — have **no M0 surface syntax** yet: the parser emits no
//! `perform`, `raise`, enum, or `linear`-consuming form, so a function that
//! exercises a capability cannot be *written* in source today (see the
//! `marv_types::check` scope notes). The protocol is agent-facing, and agents
//! routinely hold Core directly (they generated it, or pulled it from the
//! content store), so a snapshot file may instead be ingested as **Core IR**: a
//! [`CoreModuleSpec`] in the exact JSON the `marv/core` query emits, paired with
//! a [`WorldSpec`] declaring the capabilities/errors/enums the bodies reference.
//!
//! This is what lets the spec's worked example — `report.load` performs an `Fs`
//! operation its signature does not declare, `check` returns the
//! `MissingCapability` fix, `applyFix` repairs it, re-`check` is clean
//! (`spec/03` §4.1) — run end to end through the *real* checker.

use marv_core::ir::*;
use marv_core::symbol_hash;
use marv_types::{OpSig, World, WorldBuilder};
use serde::{Deserialize, Serialize};

/// A module ingested as Core IR rather than parsed from source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreModuleSpec {
    /// Dotted module path, e.g. `"report"`.
    #[serde(default)]
    pub module: String,
    /// The declaration environment the bodies resolve against (caps, errors,
    /// enums, structs, globals). Defaulted so a body that references nothing
    /// external can omit it.
    #[serde(default)]
    pub world: WorldSpec,
    /// The definitions, in order.
    pub defs: Vec<CoreDefSpec>,
}

/// One Core definition plus the metadata the protocol surfaces that the names-
/// erased Core itself does not carry: the source name and (for `signature`) the
/// parameter names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreDefSpec {
    /// The definition's source name, e.g. `"load"`.
    pub name: String,
    /// Parameter names, in order, purely for `marv/signature` display (Core has
    /// none — names are erased). May be shorter than the arity; missing names
    /// render as `arg{i}`.
    #[serde(default)]
    pub params: Vec<String>,
    /// The lowered Core definition (serde mirror of `marv_core::ir::Def`).
    pub def: Def,
}

/// A declaration environment as plain data, mirroring what [`WorldBuilder`]
/// assembles. Errors are referenced *by name* (their `symbol_hash` is the wire
/// identity), matching how a Core body's `Raise`/`Perform` name them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldSpec {
    #[serde(default)]
    pub caps: Vec<CapSpec>,
    #[serde(default)]
    pub errors: Vec<ErrorSpec>,
    #[serde(default)]
    pub enums: Vec<EnumSpec>,
    #[serde(default)]
    pub structs: Vec<StructSpec>,
    #[serde(default)]
    pub globals: Vec<GlobalSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapSpec {
    pub name: String,
    #[serde(default)]
    pub ops: Vec<OpSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpSpec {
    #[serde(default)]
    pub params: Vec<Type>,
    #[serde(default = "unit_type")]
    pub ret: Type,
    /// Errors performing this op may raise, by name.
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorSpec {
    pub name: String,
    #[serde(default)]
    pub payload: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumSpec {
    pub name: String,
    pub variants: Vec<VariantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantSpec {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructSpec {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<Type>,
    #[serde(default)]
    pub linear: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalSpec {
    pub name: String,
    pub ty: Type,
}

fn unit_type() -> Type {
    Type::Unit
}

impl WorldSpec {
    /// Assemble a [`World`] from this spec, keying every declaration under
    /// `symbol_hash(name)` — exactly the key a Core `Global`/`Nominal`/`Perform`
    /// uses to reference it.
    pub fn build(&self) -> World {
        let mut b = WorldBuilder::new();
        for c in &self.caps {
            let ops = c
                .ops
                .iter()
                .map(|o| OpSig {
                    params: o.params.clone(),
                    ret: o.ret.clone(),
                    errors: o.errors.iter().map(|e| symbol_hash(e)).collect(),
                })
                .collect();
            b = b.cap(&c.name, ops);
        }
        for e in &self.errors {
            b = b.error(&e.name, e.payload.clone());
        }
        for en in &self.enums {
            let variants = en
                .variants
                .iter()
                .map(|v| (v.name.as_str(), v.fields.clone()))
                .collect();
            b = b.enum_decl(&en.name, variants);
        }
        for s in &self.structs {
            b = b.struct_decl(&s.name, s.fields.clone(), s.linear);
        }
        for g in &self.globals {
            b = b.global(&g.name, g.ty.clone());
        }
        b.build()
    }
}
