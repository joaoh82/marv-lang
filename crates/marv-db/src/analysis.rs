//! The compiler pipeline as plain functions, and the salsa-friendly result it
//! produces.
//!
//! [`analyze_text`] runs the full phase chain the protocol exposes —
//! `parse → lower → typecheck → effects/errors` (`spec/03` §1) — over one file
//! and distils it into a [`FileAnalysis`]: a flat, owned, `serde`/`salsa::Update`
//! value holding everything the JSON-RPC method catalog needs (diagnostics,
//! per-definition signatures, Core + content hash, inferred effect/error rows,
//! call edges, canonical text). The salsa layer (`crate::lib`) memoizes this per
//! file, so editing one file recomputes only its analysis.
//!
//! The rich compiler types (`Diagnostic`, `Core`, `LoweredModule`) live in other
//! crates and do not implement `salsa::Update`; rather than couple those phase
//! crates to salsa, the query stores this distilled form. Per-*definition*
//! incrementality (one tracked struct per def) is a later refinement — today the
//! grain is the file, which already gives the milestone's "edit one file,
//! recompute only it" property.
//!
//! Lowering here is strictly **single-file** (`lower_module`): a file that
//! constructs or matches an enum imported from another module surfaces the
//! explicit `UnresolvedImportedEnum` lower error (MARV-18) rather than a wrong
//! lowering — resolving a snapshot's module *set* through these queries is
//! store work (MARV-14). The CLI's `check`/`run` path does resolve `import
//! std.*` to source and lowers the set together (`marv_cli::pipeline`).

use std::collections::HashMap;

use marv_core::ir::*;
use marv_core::{lower_module, DefEntry, LoweredModule};
use marv_syntax::{ast, format_module, parse_with_spans, Item, ItemSpan, Module};
use marv_types::{check_def, effect_row, Code, World};

use crate::corespec::CoreModuleSpec;

/// A real source span (MARV-12): a UTF-8 byte interval plus the 0-based
/// `{line, col}` rendering of each endpoint (`spec/03` §2). `col` is a byte
/// offset within its line, matching `start_byte`/`end_byte`.
///
/// These are *source* spans threaded from the lexer/parser through to the
/// distilled analysis; the Core IR itself stays span-free (it is the names-erased
/// content identity, `spec/02` §F). A diagnostic's span is its definition's
/// header — the grain real spans reach today (per-expression spans would require
/// a Core→source map that the identity model deliberately omits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::Update)]
pub struct SrcSpan {
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Maps byte offsets to 0-based `{line, col}` over one source file.
struct LineIndex {
    /// Byte offset at which each line starts (line 0 starts at byte 0).
    line_starts: Vec<u32>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// The 0-based `(line, col)` of a byte offset (`col` is the byte offset into
    /// the line).
    fn position(&self, byte: u32) -> (u32, u32) {
        // The line is the greatest start not exceeding `byte`.
        let line = match self.line_starts.binary_search(&byte) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        (line as u32, byte - self.line_starts[line])
    }

    /// Build a [`SrcSpan`] from a `(start_byte, end_byte)` pair.
    fn span(&self, (lo, hi): (u32, u32)) -> SrcSpan {
        let (start_line, start_col) = self.position(lo);
        let (end_line, end_col) = self.position(hi);
        SrcSpan {
            start_byte: lo,
            end_byte: hi,
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }
}

/// How a workspace file is ingested (`spec/03` §3.1). See [`crate::corespec`] for
/// why Core ingestion exists alongside source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::Update)]
pub enum SourceKind {
    /// marv `.mv` source: parsed and lowered through the front end.
    Source,
    /// A [`CoreModuleSpec`] in JSON: deserialized and checked directly.
    Core,
}

/// The distilled analysis of a single file — the salsa query's output and the
/// server's source of truth for every read-only method.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct FileAnalysis {
    /// Dotted module path, e.g. `"report"` (empty if the file declares none).
    pub module_path: String,
    /// Definitions in source order.
    pub defs: Vec<DefInfo>,
    /// Every diagnostic, in deterministic order, tagged with the def it belongs
    /// to.
    pub diagnostics: Vec<DiagInfo>,
    /// The whole file in canonical form (the formatter as data). For a Core file
    /// this is the re-serialized Core JSON.
    pub canonical: String,
    /// A parse/lower/deserialize error that prevented analysis, if any. When
    /// `Some`, `defs`/`diagnostics` are empty.
    pub parse_error: Option<String>,
}

/// Everything the protocol surfaces about one definition.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct DefInfo {
    /// Source name, e.g. `"load"`.
    pub name: String,
    /// Module-qualified name, e.g. `"report.load"` (how the protocol addresses
    /// it).
    pub qualified: String,
    /// `"fn"`, `"struct"`, ….
    pub kind: String,
    /// Content hash in wire form, e.g. `"b3:9f2c1a…"`.
    pub hash: String,
    /// The definition body's Core IR, serialized (`spec/03` §4.4). `"null"` for
    /// a bodyless def (a `struct`/abstract interface).
    pub core_json: String,
    /// Content hashes of the globals this body references — the Merkle-DAG edges
    /// (`deps` in `marv/core`), and the raw material for `callees`.
    pub callee_hashes: Vec<String>,
    /// Parameters (name + display type), for `marv/signature`.
    pub params: Vec<ParamInfo>,
    /// Return type, displayed.
    pub ret: String,
    /// Whether the signature is `pure` (empty declared effect row).
    pub pure: bool,
    /// Inferred capabilities the body exercises (`marv/effects`).
    pub effects: Vec<String>,
    /// Inferred error set (`marv/errorSet`).
    pub error_set: Vec<String>,
    /// Number of `requires` clauses (M1 lowers none yet).
    pub requires: usize,
    /// Number of `ensures` clauses.
    pub ensures: usize,
    /// `SAFETY:` justification for an `unsafe fn`, if this definition is an
    /// unsafe audit site. This is source metadata, not Core identity.
    pub unsafe_site: Option<String>,
    /// This definition alone, canonically formatted (`marv/canonical` def scope).
    pub canonical: String,
    /// Real source span of the definition's header — keyword(s) through name
    /// (MARV-12), for `marv/typeAt` and as the anchor for this def's diagnostics.
    /// `None` for Core-ingested files, which have no source text.
    pub span: Option<SrcSpan>,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct ParamInfo {
    pub name: String,
    pub ty: String,
}

/// A diagnostic distilled to its wire-relevant fields. For source files `span` is
/// the enclosing definition's header (MARV-12); it is `None` for Core-ingested
/// files (no source text). Related locations and fix edits carry resolved spans
/// where the front end can derive them.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct DiagInfo {
    pub code: String,
    pub severity: String,
    pub message: String,
    /// Qualified name of the definition this diagnostic was raised in.
    pub def: Option<String>,
    /// The diagnostic's source span (the def header), or `None` for Core files.
    pub span: Option<SrcSpan>,
    /// Messages of related locations.
    pub related: Vec<String>,
    pub fixes: Vec<FixInfo>,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct FixInfo {
    pub title: String,
    pub edits: Vec<EditInfo>,
    pub confidence: f32,
}

/// One edit of a [`FixInfo`]: the text to insert/replace and, where the front end
/// can resolve it, the source span it lands at (MARV-12). A `None` span means the
/// insertion point is not mechanically derivable (e.g. a Core-ingested file, or a
/// fix whose location the def-granular spans do not pin down); `new_text` is
/// always meaningful.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct EditInfo {
    pub span: Option<SrcSpan>,
    pub new_text: String,
}

/// Module-qualify a definition name.
pub fn qualify(module_path: &str, name: &str) -> String {
    if module_path.is_empty() || name.contains('.') {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}

/// Run the full pipeline over one file and distil it into a [`FileAnalysis`].
pub fn analyze_text(kind: SourceKind, text: &str) -> FileAnalysis {
    match kind {
        SourceKind::Source => analyze_source(text),
        SourceKind::Core => analyze_core(text),
    }
}

/// One definition with everything the Tier-2 verifier needs: its full Core
/// [`Def`] (carrying the `requires`/`ensures` contracts the distilled
/// [`DefInfo`] does not) and the parameter names (for labeling counterexamples).
#[derive(Debug, Clone)]
pub struct VerifyDef {
    pub name: String,
    pub qualified: String,
    pub def: Def,
    pub params: Vec<String>,
    /// Real source span of the definition's header (MARV-12), so `marv/verify`
    /// can report a `relatedSpan` pointing at the contract's definition. `None`
    /// for Core-ingested files.
    pub span: Option<SrcSpan>,
}

/// Recover the full Core definitions of a file for verification (`marv/verify`).
/// Unlike [`analyze_text`], which distils for the read-only queries, this keeps
/// the whole [`Def`] so contracts can be discharged, and returns the [`World`]
/// of struct/enum declarations so ADT-typed parameters can be havocked
/// (MARV-11; empty for Core-ingested files, which carry no declarations).
/// Returns a parse/lower/ingest error message on failure.
pub fn verify_inputs(
    kind: SourceKind,
    text: &str,
) -> Result<(String, Vec<VerifyDef>, World), String> {
    match kind {
        SourceKind::Source => {
            let (module, item_spans) =
                parse_with_spans(text).map_err(|e| format!("parse error: {e}"))?;
            let module_path = module.name.join(".");
            let lowered = lower_module(&module).map_err(|e| format!("lower error: {e}"))?;
            let world = World::from_module(&lowered);
            let line_index = LineIndex::new(text);
            let spans_by_name: HashMap<&str, &ItemSpan> =
                item_spans.iter().map(|s| (s.name.as_str(), s)).collect();
            let defs = lowered
                .defs
                .into_iter()
                .map(|e| {
                    let params = source_fn_params(&module, &e.name);
                    let qualified = qualify(&module_path, &e.name);
                    let span = spans_by_name
                        .get(e.name.as_str())
                        .map(|s| line_index.span(s.header));
                    VerifyDef {
                        name: e.name,
                        qualified,
                        def: e.def,
                        params,
                        span,
                    }
                })
                .collect();
            Ok((module_path, defs, world))
        }
        SourceKind::Core => {
            let spec: CoreModuleSpec =
                serde_json::from_str(text).map_err(|e| format!("core ingest error: {e}"))?;
            let module_path = spec.module.clone();
            let defs = spec
                .defs
                .into_iter()
                .map(|d| {
                    let qualified = qualify(&module_path, &d.name);
                    VerifyDef {
                        name: d.name,
                        qualified,
                        def: d.def,
                        params: d.params,
                        span: None,
                    }
                })
                .collect();
            Ok((module_path, defs, World::new()))
        }
    }
}

/// Parameter names of a named function in the AST (empty for non-functions).
fn source_fn_params(module: &Module, name: &str) -> Vec<String> {
    for item in &module.items {
        if let Item::Fn(f) = item {
            if f.name == name {
                return f.params.iter().map(|p| p.name.clone()).collect();
            }
        }
    }
    Vec::new()
}

// ---- source pipeline ----------------------------------------------------

fn analyze_source(text: &str) -> FileAnalysis {
    let (module, item_spans) = match parse_with_spans(text) {
        Ok(ms) => ms,
        Err(e) => return parse_failed(text.to_string(), format!("parse error: {e}")),
    };
    let module_path = module.name.join(".");
    let canonical = format_module(&module);

    let lowered = match lower_module(&module) {
        Ok(l) => l,
        Err(e) => return parse_failed(canonical, format!("lower error: {e}")),
    };
    // Build the declaration world with each function's *inferred* effect/error
    // row baked into its arrow, so a caller picks up its callees' errors through
    // `App` — full cross-call error-set inference (`spec/01` §6). The fixpoint
    // converges because rows only grow and are bounded by the program's errors.
    let world = world_with_propagated_effects(&lowered);

    // Real source spans (MARV-12): map each definition to its parsed item span,
    // and convert byte offsets to {line, col} via a line index over the file.
    let line_index = LineIndex::new(text);
    let spans_by_name: HashMap<&str, &ItemSpan> =
        item_spans.iter().map(|s| (s.name.as_str(), s)).collect();

    let mut defs = Vec::with_capacity(lowered.defs.len());
    let mut diagnostics = Vec::new();
    for entry in &lowered.defs {
        let qualified = qualify(&module_path, &entry.name);
        let item_span = spans_by_name.get(entry.name.as_str()).copied();
        let header = item_span.map(|s| line_index.span(s.header));
        let (params, ret, is_pure, def_canonical) = source_signature(&module, &entry.name);
        // The capability fix inserts a leading parameter just inside `(`; that
        // is a resolved zero-width insertion point for a `MissingCapability` fix.
        let cap_edit = item_span
            .and_then(|s| s.param_insert)
            .map(|p| line_index.span((p, p)));
        for d in check_def(&world, &entry.def, Some(&entry.name)) {
            let info = if d.code == Code::MissingCapability && is_pure {
                let pure_edit = item_span
                    .map(|s| s.header.0)
                    .and_then(|start| {
                        text.get(start as usize..start as usize + 5)
                            .map(|prefix| (start, prefix))
                    })
                    .and_then(|(start, prefix)| {
                        if prefix == "pure " {
                            Some(line_index.span((start, start + 5)))
                        } else {
                            None
                        }
                    });
                let mut info = diag_to_info(&d, Some(qualified.clone()), header, pure_edit);
                for fix in &mut info.fixes {
                    fix.title =
                        "remove `pure` marker so capability parameters declare the effect".into();
                    for edit in &mut fix.edits {
                        edit.new_text.clear();
                    }
                }
                info
            } else {
                let edit_span = if d.code == Code::MissingCapability {
                    cap_edit
                } else {
                    None
                };
                diag_to_info(&d, Some(qualified.clone()), header, edit_span)
            };
            diagnostics.push(info);
        }
        let row = effect_row(&world, &entry.def);
        let unsafe_site = source_unsafe_site(&module, &entry.name);
        defs.push(DefInfo {
            name: entry.name.clone(),
            qualified,
            kind: defkind_str(entry.def.kind).to_string(),
            hash: entry.hash.to_b3(),
            core_json: core_json(&entry.def.body),
            callee_hashes: body_globals(&entry.def.body),
            params,
            ret,
            pure: is_pure,
            effects: row.caps.iter().map(|h| world.cap_name(h)).collect(),
            error_set: row.errors.iter().map(|h| world.error_name(h)).collect(),
            requires: entry.def.requires.len(),
            ensures: entry.def.ensures.len(),
            unsafe_site,
            canonical: def_canonical,
            span: header,
        });
    }

    FileAnalysis {
        module_path,
        defs,
        diagnostics,
        canonical,
        parse_error: None,
    }
}

/// Extract a function's parameter names+types, return type, purity, single-def
/// canonical text, and source byte offset from the AST. Types are rendered in
/// their *surface* spelling (`Fs`, `str`, `&i32`) rather than the names-erased
/// Core form.
fn source_signature(module: &Module, name: &str) -> (Vec<ParamInfo>, String, bool, String) {
    for item in &module.items {
        match item {
            Item::Fn(f) if f.name == name => {
                let params = f
                    .params
                    .iter()
                    .map(|p| ParamInfo {
                        name: p.name.clone(),
                        ty: surface_ty(&p.ty),
                    })
                    .collect();
                let ret = f
                    .ret
                    .as_ref()
                    .map(surface_ty)
                    .unwrap_or_else(|| "()".into());
                return (params, ret, f.is_pure, one_def_canonical(module, item));
            }
            Item::Struct(s) if s.name == name => {
                let params = s
                    .fields
                    .iter()
                    .map(|fld| ParamInfo {
                        name: fld.name.clone(),
                        ty: surface_ty(&fld.ty),
                    })
                    .collect();
                return (
                    params,
                    name.to_string(),
                    true,
                    one_def_canonical(module, item),
                );
            }
            _ => {}
        }
    }
    (Vec::new(), "()".into(), false, String::new())
}

fn source_unsafe_site(module: &Module, name: &str) -> Option<String> {
    for item in &module.items {
        if let Item::Fn(f) = item {
            if f.name == name && f.is_unsafe {
                return f
                    .docs
                    .iter()
                    .find_map(|d| d.trim_start().strip_prefix("SAFETY:"))
                    .map(|s| s.trim().to_string());
            }
        }
    }
    None
}

/// Render an item by itself in canonical form (a module containing only it,
/// header stripped).
fn one_def_canonical(module: &Module, item: &Item) -> String {
    let solo = Module {
        name: module.name.clone(),
        imports: Vec::new(),
        items: vec![item.clone()],
    };
    let full = format_module(&solo);
    // Drop the `mod <name>\n\n` header so the def stands alone.
    match full.split_once("\n\n") {
        Some((_header, rest)) => rest.to_string(),
        None => full,
    }
}

fn surface_ty(t: &ast::Type) -> String {
    match t {
        ast::Type::Unit => "()".into(),
        ast::Type::Named(path) => path.join("."),
        ast::Type::Generic { path, args } => {
            let args: Vec<String> = args.iter().map(surface_ty).collect();
            format!("{}[{}]", path.join("."), args.join(", "))
        }
        ast::Type::Slice(inner) => format!("[]{}", surface_ty(inner)),
        ast::Type::Array { len, elem } => format!("[{len}]{}", surface_ty(elem)),
        ast::Type::Ref { mutable, inner } => {
            format!(
                "&{}{}",
                if *mutable { "mut " } else { "" },
                surface_ty(inner)
            )
        }
        ast::Type::ErrorUnion(Some(inner)) => format!("!{}", surface_ty(inner)),
        ast::Type::ErrorUnion(None) => "!".into(),
        ast::Type::Optional(inner) => format!("?{}", surface_ty(inner)),
    }
}

// ---- core pipeline ------------------------------------------------------

fn analyze_core(text: &str) -> FileAnalysis {
    let spec: CoreModuleSpec = match serde_json::from_str(text) {
        Ok(s) => s,
        Err(e) => return parse_failed(text.to_string(), format!("core ingest error: {e}")),
    };
    let world = spec.world.build();
    let module_path = spec.module.clone();
    let canonical = serde_json::to_string_pretty(&spec).unwrap_or_else(|_| text.to_string());

    let mut defs = Vec::with_capacity(spec.defs.len());
    let mut diagnostics = Vec::new();
    for d in &spec.defs {
        let qualified = qualify(&module_path, &d.name);
        for diag in check_def(&world, &d.def, Some(&d.name)) {
            // Core-ingested files have no source text, so no spans to attach.
            diagnostics.push(diag_to_info(&diag, Some(qualified.clone()), None, None));
        }
        let row = effect_row(&world, &d.def);
        let (param_tys, ret_ty, _eff) = peel_arrow(&d.def.ty);
        let params = param_tys
            .iter()
            .enumerate()
            .map(|(i, ty)| ParamInfo {
                name: d
                    .params
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("arg{i}")),
                ty: display_core_type(&world, ty),
            })
            .collect();
        defs.push(DefInfo {
            name: d.name.clone(),
            qualified,
            kind: defkind_str(d.def.kind).to_string(),
            hash: d.def.content_hash().to_b3(),
            core_json: core_json(&d.def.body),
            callee_hashes: body_globals(&d.def.body),
            params,
            ret: display_core_type(&world, &ret_ty),
            pure: peel_arrow(&d.def.ty).2.is_empty(),
            effects: row.caps.iter().map(|h| world.cap_name(h)).collect(),
            error_set: row.errors.iter().map(|h| world.error_name(h)).collect(),
            requires: d.def.requires.len(),
            ensures: d.def.ensures.len(),
            unsafe_site: None,
            canonical: String::new(),
            span: None,
        });
    }

    FileAnalysis {
        module_path,
        defs,
        diagnostics,
        canonical,
        parse_error: None,
    }
}

/// Peel a curried arrow into its parameter types, final return type, and the
/// innermost (declared) effect row.
fn peel_arrow(ty: &Type) -> (Vec<Type>, Type, EffectRow) {
    let mut params = Vec::new();
    let mut cur = ty;
    let mut eff = EffectRow::empty();
    while let Type::Arrow {
        param,
        ret,
        effects,
    } = cur
    {
        params.push((**param).clone());
        eff = effects.clone();
        cur = ret;
    }
    (params, cur.clone(), eff)
}

/// Display a Core type, resolving nominal hashes to their declared names via the
/// world where possible.
fn display_core_type(world: &World, t: &Type) -> String {
    match t {
        Type::Unit => "()".into(),
        Type::Bool => "bool".into(),
        Type::Int(i) => int_name(*i).into(),
        Type::Float(FloatTy::F32) => "f32".into(),
        Type::Float(FloatTy::F64) => "f64".into(),
        Type::Str => "str".into(),
        Type::Char => "char".into(),
        Type::Array(e, n) => format!("[{n}]{}", display_core_type(world, e)),
        Type::Slice(e) => format!("[]{}", display_core_type(world, e)),
        Type::Tuple(es) => {
            let inner: Vec<String> = es.iter().map(|e| display_core_type(world, e)).collect();
            format!("({})", inner.join(", "))
        }
        Type::Arrow { param, ret, .. } => format!(
            "fn({}) -> {}",
            display_core_type(world, param),
            display_core_type(world, ret)
        ),
        Type::Nominal { def, .. } => nominal_name(world, def),
        Type::Ref { mutable: true, of } => format!("&mut {}", display_core_type(world, of)),
        Type::Ref { mutable: false, of } => format!("&{}", display_core_type(world, of)),
        Type::Linear(inner) => format!("linear {}", display_core_type(world, inner)),
        Type::Var(i) => format!("T{i}"),
    }
}

fn nominal_name(world: &World, h: &Hash) -> String {
    if let Some(s) = world.struct_decl(h) {
        return s.name.clone();
    }
    if let Some(e) = world.enum_decl(h) {
        return e.name.clone();
    }
    if world.is_cap(h) {
        return world.cap_name(h);
    }
    format!("nominal#{}", &h.to_hex()[..8])
}

fn int_name(i: IntTy) -> &'static str {
    match i {
        IntTy::I8 => "i8",
        IntTy::I16 => "i16",
        IntTy::I32 => "i32",
        IntTy::I64 => "i64",
        IntTy::Isize => "isize",
        IntTy::U8 => "u8",
        IntTy::U16 => "u16",
        IntTy::U32 => "u32",
        IntTy::U64 => "u64",
        IntTy::Usize => "usize",
    }
}

// ---- shared helpers -----------------------------------------------------

fn parse_failed(canonical: String, err: String) -> FileAnalysis {
    FileAnalysis {
        module_path: String::new(),
        defs: Vec::new(),
        diagnostics: Vec::new(),
        canonical,
        parse_error: Some(err),
    }
}

fn defkind_str(k: DefKind) -> &'static str {
    match k {
        DefKind::Fn => "fn",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Interface => "interface",
        DefKind::Impl => "impl",
        DefKind::Const => "const",
        DefKind::Cap => "cap",
        DefKind::Error => "error",
    }
}

fn core_json(body: &Option<Core>) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "null".to_string())
}

/// Distil a checker [`Diagnostic`] to its wire fields. `span` is the enclosing
/// definition's header span (`None` for Core files); `edit_span` is the resolved
/// insertion point for this diagnostic's fix edits where one is derivable (the
/// `MissingCapability` parameter-list point), else `None`.
fn diag_to_info(
    d: &marv_types::Diagnostic,
    def: Option<String>,
    span: Option<SrcSpan>,
    edit_span: Option<SrcSpan>,
) -> DiagInfo {
    DiagInfo {
        code: d.code.as_str().to_string(),
        severity: d.severity.as_str().to_string(),
        message: d.message.clone(),
        def,
        span,
        related: d.related.iter().map(|r| r.message.clone()).collect(),
        fixes: d
            .fixes
            .iter()
            .map(|f| FixInfo {
                title: f.title.clone(),
                edits: f
                    .edits
                    .iter()
                    .map(|e| EditInfo {
                        span: edit_span,
                        new_text: e.new_text.clone(),
                    })
                    .collect(),
                confidence: f.confidence,
            })
            .collect(),
    }
}

/// The content hashes of every global a Core term references (deduplicated,
/// sorted), in wire form — the Merkle-DAG out-edges.
fn body_globals(body: &Option<Core>) -> Vec<String> {
    let mut hashes = Vec::new();
    if let Some(c) = body {
        collect_globals(c, &mut hashes);
    }
    hashes.sort();
    hashes.dedup();
    hashes.iter().map(|h| h.to_b3()).collect()
}

// ---- applyFix repair (Core snapshots) ----------------------------------

/// Apply the mechanical capability/error repair to a Core-ingested file, in
/// service of `marv/applyFix` (`spec/03` §4.1).
///
/// The `MissingCapability`/`MissingError` fixes the checker emits insert text
/// (`fs: Fs, `) into a *signature* — but a Core file has no source text to edit,
/// and the fix's span is unresolved. The faithful equivalent over Core is to
/// make the declaration honest: set the target definition's declared effect row
/// to include everything its body actually exercises ([`effect_row`]). After
/// this, the subsumption check passes and a re-`check` is clean.
///
/// `target` selects the definition by qualified or bare name; `None` repairs
/// every definition whose body out-runs its declaration. Returns the repaired
/// file text (canonical Core JSON), or `None` if the text is not a parseable
/// [`CoreModuleSpec`].
pub fn repair_core_text(text: &str, target: Option<&str>) -> Option<String> {
    let mut spec: CoreModuleSpec = serde_json::from_str(text).ok()?;
    let world = spec.world.build();
    let module_path = spec.module.clone();
    for d in &mut spec.defs {
        let qualified = qualify(&module_path, &d.name);
        if let Some(t) = target {
            if t != qualified && t != d.name {
                continue;
            }
        }
        let row = effect_row(&world, &d.def);
        if !row.is_empty() {
            repair_effect_row(&mut d.def, &row);
        }
    }
    serde_json::to_string_pretty(&spec).ok()
}

/// Build the declaration [`World`] for a lowered source module with cross-call
/// effect/error rows propagated to a fixpoint (`spec/01` §6 — error sets are
/// inferred, and a caller's set includes its callees').
///
/// The front end lowers every arrow with the empty row (inference is the
/// checker's job), so a freshly built world would let `App` propagate nothing.
/// Here each function's body is re-inferred ([`effect_row`]) and its arrow row
/// updated to include what it raises/performs *and* what its callees do; the
/// world is rebuilt and the pass repeats until no row grows. Rows only ever grow
/// and are bounded by the program's finite error/capability set, so this
/// terminates (recursion included).
fn world_with_propagated_effects(lowered: &LoweredModule) -> World {
    let mut work: Vec<DefEntry> = lowered.defs.clone();
    loop {
        let module = LoweredModule {
            module: lowered.module.clone(),
            defs: work.clone(),
            interfaces: lowered.interfaces.clone(),
            impls: lowered.impls.clone(),
            instantiations: lowered.instantiations.clone(),
        };
        let world = World::from_module(&module);
        let mut changed = false;
        for entry in &mut work {
            if entry.def.kind != DefKind::Fn {
                continue;
            }
            let row = effect_row(&world, &entry.def);
            if row.is_empty() {
                continue;
            }
            let before = innermost_arrow_eff(&entry.def.ty)
                .cloned()
                .unwrap_or_default();
            repair_effect_row(&mut entry.def, &row);
            let after = innermost_arrow_eff(&entry.def.ty)
                .cloned()
                .unwrap_or_default();
            if before != after {
                changed = true;
            }
        }
        if !changed {
            return world;
        }
    }
}

/// The innermost arrow's effect row of a (possibly curried) function type — the
/// row a call to the fully-applied function exercises. `None` for a non-arrow.
fn innermost_arrow_eff(t: &Type) -> Option<&EffectRow> {
    match t {
        Type::Arrow { ret, effects, .. } if matches!(**ret, Type::Arrow { .. }) => {
            innermost_arrow_eff(ret)
        }
        Type::Arrow { effects, .. } => Some(effects),
        _ => None,
    }
}

/// Set a definition's *declared* effect row (the innermost arrow's, and its
/// paired lambda's) to the union of what it already declares and `row`.
fn repair_effect_row(def: &mut Def, row: &EffectRow) {
    set_innermost_arrow_eff(&mut def.ty, row);
    if let Some(body) = &mut def.body {
        set_innermost_lam_eff(body, row);
    }
}

fn set_innermost_arrow_eff(t: &mut Type, row: &EffectRow) {
    if let Type::Arrow { ret, effects, .. } = t {
        if matches!(**ret, Type::Arrow { .. }) {
            set_innermost_arrow_eff(ret, row);
        } else {
            *effects = union_rows(effects, row);
        }
    }
}

fn set_innermost_lam_eff(c: &mut Core, row: &EffectRow) {
    if let Core::Lam { body, effects, .. } = c {
        if matches!(**body, Core::Lam { .. }) {
            set_innermost_lam_eff(body, row);
        } else {
            *effects = union_rows(effects, row);
        }
    }
}

fn union_rows(a: &EffectRow, b: &EffectRow) -> EffectRow {
    let mut out = a.clone();
    for c in &b.caps {
        if !out.caps.contains(c) {
            out.caps.push(*c);
        }
    }
    for e in &b.errors {
        if !out.errors.contains(e) {
            out.errors.push(*e);
        }
    }
    out
}

fn collect_globals(c: &Core, out: &mut Vec<Hash>) {
    let atom = |a: &Atom, out: &mut Vec<Hash>| {
        if let Atom::Global(h) = a {
            out.push(*h);
        }
    };
    match c {
        Core::Atom(a) => atom(a, out),
        Core::Let { value, body } => {
            collect_globals(value, out);
            collect_globals(body, out);
        }
        Core::Lam { body, .. } => collect_globals(body, out),
        Core::App { func, arg } => {
            atom(func, out);
            atom(arg, out);
        }
        Core::Ctor { fields, .. } => fields.iter().for_each(|a| atom(a, out)),
        Core::Array { items, .. } => items.iter().for_each(|a| atom(a, out)),
        Core::IndexSet { base, index, value } => {
            atom(base, out);
            atom(index, out);
            atom(value, out);
        }
        Core::ListNew {
            alloc, capacity, ..
        } => {
            atom(alloc, out);
            atom(capacity, out);
        }
        Core::ListPush { alloc, list, value } => {
            atom(alloc, out);
            atom(list, out);
            atom(value, out);
        }
        Core::ListPop { list } => atom(list, out),
        Core::ListSet { list, index, value } => {
            atom(list, out);
            atom(index, out);
            atom(value, out);
        }
        Core::Proj { base, .. } => atom(base, out),
        Core::Match {
            scrutinee,
            branches,
        } => {
            atom(scrutinee, out);
            branches.iter().for_each(|b| collect_globals(&b.body, out));
        }
        Core::Prim { args, .. } => args.iter().for_each(|a| atom(a, out)),
        Core::Cast { value, .. } => atom(value, out),
        Core::Ref { of, .. } => atom(of, out),
        Core::Perform { cap, args, .. } => {
            atom(cap, out);
            args.iter().for_each(|a| atom(a, out));
        }
        Core::Raise { args, .. } => args.iter().for_each(|a| atom(a, out)),
        Core::Return { value } => atom(value, out),
        Core::Loop {
            state, cond, body, ..
        } => {
            state.iter().for_each(|a| atom(a, out));
            collect_globals(cond, out);
            collect_globals(body, out);
        }
    }
}
