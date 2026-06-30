//! `marv` — the command-line front end for the marv toolchain.
//!
//! Subcommands mirror the agent protocol (`spec/03-compiler-protocol.md`):
//!
//! - `fmt`    — canonicalize source (wired to `marv-syntax::format`).
//! - `check`  — type / effect / capability checking (`marv-types`, M2).
//! - `build`  — compile a target (`marv-codegen-cl`, M4).
//! - `run`    — interpret an entry point with an explicit capability grant set
//!   (`marv-interp`, M4; `spec/03` §4.5).
//! - `verify` — discharge contracts via SMT (`marv-verify`, M6).
//! - `commit` — freeze definitions into the content-addressed store
//!   (`marv-store`, M7; `spec/03` §3.4).
//!
//! Argument parsing is hand-rolled to keep the front end small.

mod pipeline;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use marv_codegen_cl as codegen;
use marv_codegen_llvm as llvm;
use marv_codegen_wasm as wasm;
use marv_core::ir::{Def, DefKind, Hash, Type};
use marv_interp::Program;
use marv_store::{commit_with_meta, resolve, CommitStatus, DefMeta, Store, StoreDir, StoredDef};
use marv_types::{OpSig, World, WorldBuilder};
use marv_verify::{verify_def, VerifyOutcome};

use pipeline::{any_errors, load, print_diags, Loaded};

const USAGE: &str = "\
marv — the marv language toolchain

USAGE:
    marv <command> [args]

COMMANDS:
    fmt [--write|--check] [files...]
                               Canonicalize marv source. With no files, reads
                               stdin and writes canonical form to stdout. With
                               files and no flag, prints each file's canonical
                               form to stdout. With --write, rewrites each file
                               in place. With --check, writes nothing and exits
                               non-zero if any input is not already canonical.
    check <file>               Type / effect / capability check a `.mv` source
                               file or a `.core.json` Core-IR snapshot.
    build [--target T] [--run] [--release] [--emit object|exe] [--out PATH] [--entry NAME] <file> [args...]
                               Compile. Refuses to build a file that fails
                               `check`. Targets: `native-cranelift` (default;
                               --run JIT-executes the entry and prints its
                               integer result; --out writes a linked native
                               executable; --emit object writes a relocatable
                               object), `native-llvm` (uses clang/LLVM for
                               optimized release-style native run/exe output),
                               and `wasm-component` (writes a
                               .wasm module to --out, default <file>.wasm, and
                               reports the host imports = capabilities it needs).
                               Only definitions reachable from the entry
                               (--entry, else `main`, else the sole function)
                               are compiled, so an unreferenced sibling the
                               backend can't lower yet doesn't block the build;
                               without a resolvable entry the whole module is
                               compiled. Debug builds (the default) carry the
                               Tier-1 bounds check on runtime element
                               reads/stores; --release omits it.
                               With --store DIR, resolve imports through DIR's
                               lockfile and build from fetched pinned hashes.
    run [--grant CAP,CAP] [--entry NAME] <file> [args...]
                               Interpret an entry point (the semantics oracle).
                               Capabilities enter only through --grant; the
                               entry's value parameters are filled from [args...]
                               in order.
    verify [--def NAME] <file> Discharge `requires`/`ensures` contracts via SMT.
    resolve-impl <file>        Report each generic instantiation and which
                               coherent `impl` its bounded type arguments select
                               (the `marv/resolveImpl` report, `spec/01` §3.4).
    commit [--store DIR] <file>
                               Freeze a file's definitions into the content-
                               addressed store (default .marv/), update the
                               lockfile, and report the delta (new vs. already
                               reviewed). Identity is the content (dag) hash, so
                               re-committing is idempotent and renames are free.
    store audit [--store DIR]  Print reviewed/reachable provenance for blobs.
    store gc [--store DIR]     Remove blobs unreachable from the lockfile.

    -h, --help                 Print this help.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let rest = &args[1..];

    match command.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "fmt" => cmd_fmt(rest),
        "check" => cmd_check(rest),
        "build" => cmd_build(rest),
        "run" => cmd_run(rest),
        "resolve-impl" => cmd_resolve_impl(rest),
        "verify" => cmd_verify(rest),
        "commit" => cmd_commit(rest),
        "store" => cmd_store(rest),
        other => {
            eprintln!("marv: unknown command `{other}`\n");
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

// ---- resolve-impl -------------------------------------------------------

/// `marv resolve-impl <file>` — the `marv/resolveImpl` report (`spec/01` §3.4):
/// for every generic instantiation the program requests, print which coherent
/// `impl` each of its bounded type arguments resolves to, and which method
/// definition each interface method dispatches to. Also surfaces any unsatisfied
/// bound / coherence diagnostics.
fn cmd_resolve_impl(args: &[String]) -> ExitCode {
    let Some(file) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("marv resolve-impl: expected a file");
        return ExitCode::FAILURE;
    };
    if !file.ends_with(".mv") {
        eprintln!("marv resolve-impl: expected a `.mv` source file");
        return ExitCode::FAILURE;
    }
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("marv resolve-impl: {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let module = match marv_syntax::parse(&src) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("marv resolve-impl: parse error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let lowered = match marv_core::lower_module(&module) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("marv resolve-impl: lower error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let modules = std::slice::from_ref(&lowered);

    let resolutions = marv_types::resolve_impls(modules);
    if resolutions.is_empty() {
        println!("{file}: no generic instantiations");
    }
    for r in &resolutions {
        println!("{} (instantiates `{}`)", r.instance, r.generic);
        if r.selections.is_empty() {
            println!("    (no bounded type parameters)");
        }
        for sel in &r.selections {
            println!(
                "    {}: {} = {}  ->  impl {}[{}]",
                sel.param, sel.interface, sel.type_key, sel.interface, sel.type_key
            );
            for (meth, def) in &sel.methods {
                println!("        {meth} -> {def}");
            }
        }
    }

    // Report (but do not stop on) unsatisfied bounds / coherence violations.
    let mut bad = false;
    for (_, d) in marv_types::check_bounds(modules) {
        bad = true;
        eprintln!("{}[{}] {}", d.severity.as_str(), d.code.as_str(), d.message);
    }
    if bad {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ---- verify -------------------------------------------------------------

/// `marv verify [--def NAME] <file>` — discharge each function's contracts via
/// the SMT backend (Tier 2), printing `proved` / `failed` (with a
/// counterexample) / `unsupported` (`spec/03` §3.3, §4.3). Exits non-zero only
/// when a contract is provably *violated*.
fn cmd_verify(args: &[String]) -> ExitCode {
    let mut def_filter: Option<String> = None;
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--def" => {
                i += 1;
                match args.get(i) {
                    Some(v) => def_filter = Some(v.clone()),
                    None => {
                        eprintln!("marv verify: --def requires a value");
                        return ExitCode::FAILURE;
                    }
                }
            }
            f if f.starts_with("--") => {
                eprintln!("marv verify: unknown flag `{f}`");
                return ExitCode::FAILURE;
            }
            _ if file.is_none() => file = Some(args[i].clone()),
            _ => {}
        }
        i += 1;
    }
    let Some(file) = file else {
        eprintln!("marv verify: expected a file");
        return ExitCode::FAILURE;
    };

    let loaded = match load(&file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("marv verify: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut any_failed = false;
    let mut verified_any = false;
    for (i, (name, def)) in loaded.defs.iter().enumerate() {
        if def.kind != marv_core::DefKind::Fn {
            continue;
        }
        let qualified = if loaded.module_path.is_empty() {
            name.clone()
        } else {
            format!("{}.{}", loaded.module_path, name)
        };
        if let Some(t) = &def_filter {
            if t != name && t != &qualified {
                continue;
            }
        }
        // Only functions that carry contracts are worth reporting. A loop
        // `invariant` is a contract too (MARV-22).
        if def.requires.is_empty()
            && def.ensures.is_empty()
            && !marv_verify::has_loop_invariant(def)
        {
            continue;
        }
        verified_any = true;
        let names = loaded.param_names.get(i).cloned().unwrap_or_default();
        let outcome = verify_def(def, &names, &loaded.world);
        if matches!(outcome, VerifyOutcome::Failed { .. }) {
            any_failed = true;
        }
        print_verify(&qualified, &outcome);
    }

    if !verified_any {
        eprintln!("marv verify: {file}: no contracts to verify");
    }
    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print one definition's verification result (`spec/03` §4.3 shape, in prose).
fn print_verify(name: &str, outcome: &VerifyOutcome) {
    match outcome {
        VerifyOutcome::Proved => println!("proved   {name}  (Tier 2: holds for all inputs)"),
        VerifyOutcome::Failed {
            obligation,
            counterexample,
            message,
        } => {
            println!("FAILED   {name}  — {message}");
            println!("    obligation: {obligation}");
            let assigns: Vec<String> = counterexample
                .iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect();
            println!("    counterexample: {{ {} }}", assigns.join(", "));
        }
        VerifyOutcome::Unsupported { reason } => {
            println!("unsupported {name}  — {reason}");
            println!("    fallback: runtime-checked (Tier 1)");
        }
        VerifyOutcome::SolverUnavailable { reason } => {
            println!("unsupported {name}  — {reason}");
            println!("    fallback: runtime-checked (Tier 1)");
        }
    }
}

// ---- commit -------------------------------------------------------------

/// `marv commit [--store DIR] <file>` — freeze a file's definitions into the
/// content-addressed store and report the lockfile delta (`spec/03` §3.4).
fn cmd_commit(args: &[String]) -> ExitCode {
    let mut store_dir = ".marv".to_string();
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--store" => {
                i += 1;
                match args.get(i) {
                    Some(v) => store_dir = v.clone(),
                    None => {
                        eprintln!("marv commit: --store requires a value");
                        return ExitCode::FAILURE;
                    }
                }
            }
            f if f.starts_with("--") => {
                eprintln!("marv commit: unknown flag `{f}`");
                return ExitCode::FAILURE;
            }
            _ if file.is_none() => file = Some(args[i].clone()),
            _ => {}
        }
        i += 1;
    }
    let Some(file) = file else {
        eprintln!("marv commit: expected a file");
        return ExitCode::FAILURE;
    };

    let loaded = match load(&file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("marv commit: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A commit freezes checked code; refuse if it does not check.
    let diags = loaded.check();
    print_diags(&diags);
    if any_errors(&diags) {
        eprintln!("marv commit: {file}: refusing to commit (checker reported errors)");
        return ExitCode::FAILURE;
    }

    let dir = StoreDir::new(&store_dir);
    let (mut store, mut lock) = match dir.load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("marv commit: {store_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let entries = loaded.store_entries();
    let report = commit_with_meta(&mut store, &mut lock, &loaded.module_path, &entries);

    for e in &report.entries {
        let short = &e.hash[..e.hash.len().min(15)];
        match e.status {
            CommitStatus::New => {
                println!("  + {}  {}…  (new — frozen & reviewed)", e.qualified, short)
            }
            CommitStatus::Existing { reviewed } => {
                let tag = if reviewed {
                    "already in store — already reviewed"
                } else {
                    "already in store"
                };
                println!("  = {}  {}…  ({tag})", e.qualified, short);
            }
        }
    }
    for name in &report.rebound {
        println!("  ~ {name}  (name rebound to a new hash)");
    }

    if let Err(e) = dir.save(&store, &lock) {
        eprintln!("marv commit: {store_dir}: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "marv commit: {file}: {} new, {} already in store ({} defs total in {store_dir})",
        report.added(),
        report.deduped(),
        store.len()
    );
    ExitCode::SUCCESS
}

fn cmd_store(args: &[String]) -> ExitCode {
    let Some(action) = args.first() else {
        eprintln!("marv store: expected `audit` or `gc`");
        return ExitCode::FAILURE;
    };
    let mut store_dir = ".marv".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--store" => {
                i += 1;
                match args.get(i) {
                    Some(v) => store_dir = v.clone(),
                    None => {
                        eprintln!("marv store {action}: --store requires a value");
                        return ExitCode::FAILURE;
                    }
                }
            }
            f if f.starts_with("--") => {
                eprintln!("marv store {action}: unknown flag `{f}`");
                return ExitCode::FAILURE;
            }
            extra => {
                eprintln!("marv store {action}: unexpected argument `{extra}`");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let dir = StoreDir::new(&store_dir);
    let (mut store, lock) = match dir.load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("marv store {action}: {store_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };

    match action.as_str() {
        "audit" => {
            let report = store.audit(&lock);
            for e in &report.entries {
                let reach = if e.reachable {
                    "reachable"
                } else {
                    "unreachable"
                };
                let review = if e.reviewed { "reviewed" } else { "unreviewed" };
                println!(
                    "{}  {}  {review}  {reach}  deps:{}  unsafe:{}",
                    e.hash,
                    e.name,
                    e.deps.len(),
                    e.unsafe_sites.len()
                );
            }
            eprintln!(
                "marv store audit: {} blob(s), {} lock binding(s) in {store_dir}",
                report.entries.len(),
                lock.bindings.len()
            );
            ExitCode::SUCCESS
        }
        "gc" => {
            let report = store.gc(&lock);
            for hash in &report.removed {
                println!("  - {hash}");
            }
            if let Err(e) = dir.save(&store, &lock) {
                eprintln!("marv store gc: {store_dir}: {e}");
                return ExitCode::FAILURE;
            }
            eprintln!(
                "marv store gc: removed {} blob(s), retained {} in {store_dir}",
                report.removed.len(),
                report.retained
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("marv store: unknown action `{other}` (expected `audit` or `gc`)");
            ExitCode::FAILURE
        }
    }
}

// ---- check --------------------------------------------------------------

/// `marv check <file>` — run the M2 checker and print every diagnostic.
fn cmd_check(args: &[String]) -> ExitCode {
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [file] = files.as_slice() else {
        eprintln!("marv check: expected exactly one file");
        return ExitCode::FAILURE;
    };
    let loaded = match load(file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("marv check: {e}");
            return ExitCode::FAILURE;
        }
    };
    let diags = loaded.check();
    print_diags(&diags);
    if any_errors(&diags) {
        ExitCode::FAILURE
    } else {
        eprintln!(
            "marv check: {file}: ok ({} definition(s))",
            loaded.defs.len()
        );
        ExitCode::SUCCESS
    }
}

impl Loaded {
    fn store_entries(&self) -> Vec<(String, Def, DefMeta)> {
        self.defs
            .iter()
            .zip(self.store_meta.iter())
            .map(|((name, def), meta)| (name.clone(), def.clone(), meta.clone()))
            .collect()
    }
}

#[derive(Clone)]
struct PinnedDef {
    hash: Hash,
    name: String,
    def: Def,
    meta: DefMeta,
}

struct PinnedProgram {
    defs: Vec<PinnedDef>,
    aliases: Vec<(String, Hash)>,
    world: World,
}

impl PinnedProgram {
    fn defs_for_backend(&self) -> Vec<(Hash, String, Def)> {
        self.defs
            .iter()
            .map(|d| (d.hash, d.name.clone(), d.def.clone()))
            .collect()
    }
}

fn pin_loaded(loaded: &Loaded, store_dir: &str, entry: &str) -> Result<PinnedProgram, String> {
    let dir = StoreDir::new(store_dir);
    let (store, lock) = dir.load().map_err(|e| format!("{store_dir}: {e}"))?;
    let external = lock.external_index();
    let resolved = resolve(&loaded.module_path, &loaded.defs, &external);

    let mut local: BTreeMap<String, StoredDef> = BTreeMap::new();
    let mut aliases = Vec::new();
    for (i, (name, _)) in loaded.defs.iter().enumerate() {
        let hash = resolved.dag_hashes[i].to_b3();
        let qualified = qualify_name(&loaded.module_path, name);
        aliases.push((name.clone(), resolved.dag_hashes[i]));
        aliases.push((qualified.clone(), resolved.dag_hashes[i]));
        let deps = resolved.deps[i].iter().map(|h| h.to_b3()).collect();
        local.insert(
            hash.clone(),
            StoredDef {
                hash,
                name: qualified,
                def: resolved.resolved_defs[i].clone(),
                meta: loaded.store_meta[i].clone(),
                deps,
                reviewed: false,
            },
        );
    }

    let roots = root_hashes(loaded, &resolved.dag_hashes, entry);
    let mut seen = BTreeSet::new();
    let mut defs = Vec::new();
    for root in roots {
        visit_pinned(&root.to_b3(), &local, &store, &mut seen, &mut defs)?;
    }
    let world = world_for_pinned(&defs);
    Ok(PinnedProgram {
        defs,
        aliases,
        world,
    })
}

fn visit_pinned(
    hash: &str,
    local: &BTreeMap<String, StoredDef>,
    store: &Store,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<PinnedDef>,
) -> Result<(), String> {
    if !seen.insert(hash.to_string()) {
        return Ok(());
    }
    let blob = local
        .get(hash)
        .cloned()
        .or_else(|| store.get(hash).cloned())
        .ok_or_else(|| format!("store is missing pinned dependency `{hash}`"))?;
    let parsed =
        Hash::from_b3(&blob.hash).ok_or_else(|| format!("invalid hash `{}`", blob.hash))?;
    out.push(PinnedDef {
        hash: parsed,
        name: blob.name.clone(),
        def: blob.def.clone(),
        meta: blob.meta.clone(),
    });
    for dep in &blob.deps {
        visit_pinned(dep, local, store, seen, out)?;
    }
    Ok(())
}

fn root_hashes(loaded: &Loaded, hashes: &[Hash], entry: &str) -> Vec<Hash> {
    let concrete_fn = |def: &Def| def.kind == DefKind::Fn && !def.ty.is_polymorphic();
    if !entry.is_empty() {
        if let Some((idx, _)) = loaded.defs.iter().enumerate().find(|(_, (name, def))| {
            concrete_fn(def) && (name == entry || qualify_name(&loaded.module_path, name) == entry)
        }) {
            return vec![hashes[idx]];
        }
        return hashes.to_vec();
    }
    if let Some((idx, _)) = loaded
        .defs
        .iter()
        .enumerate()
        .find(|(_, (name, def))| name == "main" && concrete_fn(def))
    {
        return vec![hashes[idx]];
    }
    let fns: Vec<_> = loaded
        .defs
        .iter()
        .enumerate()
        .filter(|(_, (_, def))| concrete_fn(def))
        .map(|(idx, _)| hashes[idx])
        .collect();
    match fns.as_slice() {
        [h] => vec![*h],
        _ => hashes.to_vec(),
    }
}

fn world_for_pinned(defs: &[PinnedDef]) -> World {
    let mut b = WorldBuilder::new();
    for d in defs {
        match d.def.kind {
            DefKind::Struct => {
                let (fields, linear) = struct_fields(&d.def.ty);
                b = b.struct_hash(d.hash, d.name.clone(), fields, linear);
            }
            DefKind::Enum => {
                let variants = d
                    .meta
                    .enum_variants
                    .iter()
                    .map(|v| (v.name.clone(), v.fields.clone()))
                    .collect();
                b = b.enum_hash(d.hash, d.name.clone(), variants);
            }
            DefKind::Error => {
                let variants = d
                    .meta
                    .enum_variants
                    .iter()
                    .map(|v| (v.name.clone(), v.fields.clone()))
                    .collect();
                b = b.error_hash(d.hash, d.name.clone(), Vec::new()).enum_hash(
                    d.hash,
                    d.name.clone(),
                    variants,
                );
            }
            DefKind::Interface if !d.meta.capability_ops.is_empty() => {
                let ops: Vec<OpSig> = d
                    .meta
                    .capability_ops
                    .iter()
                    .map(|op| OpSig {
                        consumes_receiver: op.consumes_receiver,
                        params: op.params.clone(),
                        ret: op.ret.clone(),
                        errors: op.errors.clone(),
                    })
                    .collect();
                let bare = d.name.rsplit('.').next().unwrap_or(&d.name).to_string();
                b = b
                    .cap_hash(d.hash, bare.clone(), ops.clone())
                    .cap(&bare, ops);
            }
            DefKind::Interface => {}
            _ => {
                b = b.global_hash(d.hash, d.def.ty.clone());
            }
        }
    }
    b.build()
}

fn struct_fields(ty: &Type) -> (Vec<Type>, bool) {
    match ty {
        Type::Linear(inner) => {
            let (fields, _) = struct_fields(inner);
            (fields, true)
        }
        Type::Tuple(fields) => (fields.clone(), false),
        other => (vec![other.clone()], false),
    }
}

fn qualify_name(module_path: &str, name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}

// ---- build --------------------------------------------------------------

/// `marv build [--target T] [--run] [--emit object|exe] [--out PATH]
/// [--entry NAME] <file>` —
/// check, then compile with the selected backend.
///
/// Targets: `native-cranelift` (Cranelift JIT; `--run` executes it; `--out` /
/// `--emit` writes AOT artifacts), `native-llvm` (LLVM IR via `clang` for the
/// release slice), and `wasm-component` (a WebAssembly module written to `--out`,
/// default `<file>.wasm`). All refuse code that fails `check`, and all compile
/// only the definitions reachable from the entry (MARV-8) — `commit`/audit flows
/// keep operating on every definition.
fn cmd_build(args: &[String]) -> ExitCode {
    let inv = match parse_invocation(args) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(file) = inv.file.clone() else {
        eprintln!("marv build: expected a file");
        return ExitCode::FAILURE;
    };

    let loaded = match load(&file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A build refuses code that does not check — this is where a program using a
    // capability absent from its effect row fails to compile (`spec/03` §5).
    let diags = loaded.check();
    print_diags(&diags);
    if any_errors(&diags) {
        eprintln!("marv build: {file}: refusing to compile (checker reported errors)");
        return ExitCode::FAILURE;
    }

    if let Some(store_dir) = &inv.store_dir {
        let pinned = match pin_loaded(&loaded, store_dir, &inv.entry) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("marv build: {e}");
                return ExitCode::FAILURE;
            }
        };
        return match inv.target.as_str() {
            "native-cranelift" => build_native_pinned(&inv, &file, &pinned),
            "native-llvm" | "llvm" => build_llvm_pinned(&inv, &file, &pinned),
            "wasm-component" | "wasm" => build_wasm_pinned(&inv, &file, &pinned),
            other => {
                eprintln!(
                    "marv build: unsupported target `{other}` (have `native-cranelift`, \
                     `native-llvm`, `wasm-component`)"
                );
                ExitCode::FAILURE
            }
        };
    }

    match inv.target.as_str() {
        "native-cranelift" => build_native(&inv, &file, &loaded),
        "native-llvm" | "llvm" => build_llvm(&inv, &file, &loaded),
        // `wasm-component` is the spec's name for the WASM target; today the
        // artifact is a core module (the component's substrate), with
        // capabilities as host imports per the component model (`spec/01` §9).
        "wasm-component" | "wasm" => build_wasm(&inv, &file, &loaded),
        other => {
            eprintln!(
                "marv build: unsupported target `{other}` (have `native-cranelift`, \
                 `native-llvm`, `wasm-component`)"
            );
            ExitCode::FAILURE
        }
    }
}

fn build_native_pinned(inv: &Invocation, file: &str, pinned: &PinnedProgram) -> ExitCode {
    let opts = codegen::Options {
        bounds_checks: !inv.release,
    };
    let emit_kind = match native_emit_kind(inv) {
        Ok(kind) => kind,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(kind) = emit_kind {
        let artifact = match codegen::emit_hashed_object_reachable(
            &pinned.defs_for_backend(),
            &pinned.aliases,
            &pinned.world,
            &opts,
            &inv.entry,
        ) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("marv build: {e}");
                return ExitCode::FAILURE;
            }
        };
        return emit_native_artifact(inv, file, &artifact, kind, true);
    }
    let jit = match codegen::compile_hashed_reachable(
        &pinned.defs_for_backend(),
        &pinned.aliases,
        &pinned.world,
        &opts,
        &inv.entry,
    ) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    let arity = match jit.entry_arity(&inv.entry) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !inv.run {
        eprintln!(
            "marv build: {file}: compiled from pinned hashes via native-cranelift \
             (entry takes {arity} word argument(s))"
        );
        return ExitCode::SUCCESS;
    }
    let ints = match parse_int_args(&inv.args, arity) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    match jit.run_i64(&inv.entry, &ints) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("marv build: {e}");
            ExitCode::FAILURE
        }
    }
}

fn build_wasm_pinned(inv: &Invocation, file: &str, pinned: &PinnedProgram) -> ExitCode {
    let opts = wasm::Options {
        bounds_checks: !inv.release,
    };
    let artifact = match wasm::compile_hashed_reachable(
        &pinned.defs_for_backend(),
        &pinned.aliases,
        &pinned.world,
        &opts,
        &inv.entry,
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out = inv.out.clone().unwrap_or_else(|| default_wasm_out(file));
    if let Err(e) = std::fs::write(&out, &artifact.bytes) {
        eprintln!("marv build: {out}: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!(
        "marv build: {file}: wrote {out} ({} bytes) from pinned hashes via wasm-component",
        artifact.bytes.len()
    );
    if artifact.imports.is_empty() {
        eprintln!("  capabilities required: none (pure — imports nothing)");
    } else {
        eprintln!("  capabilities required (host imports):");
        for imp in &artifact.imports {
            eprintln!("    {}::op{} ({} arg(s))", imp.cap, imp.op, imp.params);
        }
    }
    eprintln!("  exports: {}", join_exports(&artifact.exports));
    ExitCode::SUCCESS
}

fn build_llvm_pinned(inv: &Invocation, file: &str, pinned: &PinnedProgram) -> ExitCode {
    build_llvm_inner(
        inv,
        file,
        &pinned.defs_for_backend(),
        &pinned.aliases,
        &pinned.world,
        true,
    )
}

/// Cranelift backend: JIT-compile/run by default, or emit AOT artifacts when
/// `--out`/`--emit` requests them.
fn build_native(inv: &Invocation, file: &str, loaded: &Loaded) -> ExitCode {
    let opts = codegen::Options {
        bounds_checks: !inv.release,
    };
    let emit_kind = match native_emit_kind(inv) {
        Ok(kind) => kind,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(kind) = emit_kind {
        let artifact = match codegen::emit_hashed_object_reachable(
            &loaded.runtime_defs,
            &loaded.runtime_aliases,
            &loaded.world,
            &opts,
            &inv.entry,
        ) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("marv build: {e}");
                return ExitCode::FAILURE;
            }
        };
        return emit_native_artifact(inv, file, &artifact, kind, false);
    }
    // Compile only what the entry reaches (MARV-8): a sibling definition the
    // backend cannot lower yet must not block a build that never calls it.
    // Whole-module compilation remains the audit path (`compile_with`).
    let jit = match codegen::compile_hashed_reachable(
        &loaded.runtime_defs,
        &loaded.runtime_aliases,
        &loaded.world,
        &opts,
        &inv.entry,
    ) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    let arity = match jit.entry_arity(&inv.entry) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !inv.run {
        eprintln!(
            "marv build: {file}: compiled via native-cranelift (entry takes {arity} word \
             argument(s))"
        );
        return ExitCode::SUCCESS;
    }
    let ints = match parse_int_args(&inv.args, arity) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    match jit.run_i64(&inv.entry, &ints) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("marv build: {e}");
            ExitCode::FAILURE
        }
    }
}

/// LLVM release backend: emit deterministic LLVM IR and use `clang` to run or
/// link optimized native executables for the supported Core subset.
fn build_llvm(inv: &Invocation, file: &str, loaded: &Loaded) -> ExitCode {
    build_llvm_inner(
        inv,
        file,
        &loaded.runtime_defs,
        &loaded.runtime_aliases,
        &loaded.world,
        false,
    )
}

fn build_llvm_inner(
    inv: &Invocation,
    file: &str,
    defs: &[(marv_core::Hash, String, marv_core::ir::Def)],
    aliases: &[(String, marv_core::Hash)],
    world: &marv_types::World,
    pinned: bool,
) -> ExitCode {
    if inv.emit.is_some() {
        eprintln!("marv build: native-llvm does not support --emit yet (use --run or --out)");
        return ExitCode::FAILURE;
    }
    if inv.run && inv.out.is_some() {
        eprintln!("marv build: --run cannot be combined with native-llvm --out");
        return ExitCode::FAILURE;
    }
    let opts = llvm::Options {
        bounds_checks: !inv.release,
        optimize: true,
    };
    let program = match llvm::compile_hashed_reachable(defs, aliases, world, &opts, &inv.entry) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source = if pinned { " from pinned hashes" } else { "" };
    if let Some(out) = &inv.out {
        if let Err(e) = program.link_executable(out) {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!(
            "marv build: {file}: wrote {out}{source} via native-llvm executable \
             (entry takes {} word argument(s))",
            program.entry_arity()
        );
        return ExitCode::SUCCESS;
    }
    if !inv.run {
        eprintln!(
            "marv build: {file}: compiled{source} via native-llvm (entry takes {} word \
             argument(s))",
            program.entry_arity()
        );
        return ExitCode::SUCCESS;
    }
    let ints = match parse_int_args(&inv.args, program.entry_arity()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };
    match program.run_i64(&ints) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("marv build: {e}");
            ExitCode::FAILURE
        }
    }
}

/// WebAssembly backend: emit a `.wasm` module and report its capability
/// manifest (the host imports it requires; a pure module requires none).
fn build_wasm(inv: &Invocation, file: &str, loaded: &Loaded) -> ExitCode {
    let opts = wasm::Options {
        bounds_checks: !inv.release,
    };
    // Same reachability pruning as the native path (MARV-8): only the entry's
    // closure is compiled and exported.
    let artifact = match wasm::compile_hashed_reachable(
        &loaded.runtime_defs,
        &loaded.runtime_aliases,
        &loaded.world,
        &opts,
        &inv.entry,
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("marv build: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out = inv.out.clone().unwrap_or_else(|| default_wasm_out(file));
    if let Err(e) = std::fs::write(&out, &artifact.bytes) {
        eprintln!("marv build: {out}: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "marv build: {file}: wrote {out} ({} bytes) via wasm-component",
        artifact.bytes.len()
    );
    if artifact.imports.is_empty() {
        eprintln!("  capabilities required: none (pure — imports nothing)");
    } else {
        eprintln!("  capabilities required (host imports):");
        for imp in &artifact.imports {
            eprintln!("    {}::op{} ({} arg(s))", imp.cap, imp.op, imp.params);
        }
    }
    eprintln!("  exports: {}", join_exports(&artifact.exports));
    ExitCode::SUCCESS
}

#[derive(Clone, Copy)]
enum NativeEmit {
    Object,
    Executable,
}

fn native_emit_kind(inv: &Invocation) -> Result<Option<NativeEmit>, String> {
    if inv.run && (inv.out.is_some() || inv.emit.is_some()) {
        return Err("--run cannot be combined with native --out/--emit AOT output".into());
    }
    match inv.emit.as_deref() {
        Some("object") => Ok(Some(NativeEmit::Object)),
        Some("exe") | Some("executable") => Ok(Some(NativeEmit::Executable)),
        Some(other) => Err(format!(
            "unsupported native --emit `{other}` (have `object`, `exe`)"
        )),
        None if inv.out.is_some() => Ok(Some(NativeEmit::Executable)),
        None => Ok(None),
    }
}

fn emit_native_artifact(
    inv: &Invocation,
    file: &str,
    artifact: &codegen::AotObject,
    kind: NativeEmit,
    pinned: bool,
) -> ExitCode {
    match kind {
        NativeEmit::Object => {
            let out = inv.out.clone().unwrap_or_else(|| default_object_out(file));
            if let Err(e) = std::fs::write(&out, &artifact.bytes) {
                eprintln!("marv build: {out}: {e}");
                return ExitCode::FAILURE;
            }
            let source = if pinned { " from pinned hashes" } else { "" };
            eprintln!(
                "marv build: {file}: wrote {out} ({} bytes){source} via native-cranelift object",
                artifact.bytes.len()
            );
            ExitCode::SUCCESS
        }
        NativeEmit::Executable => {
            let out = inv.out.clone().unwrap_or_else(|| default_exe_out(file));
            match link_native_executable(&out, artifact) {
                Ok(()) => {
                    let source = if pinned { " from pinned hashes" } else { "" };
                    eprintln!(
                        "marv build: {file}: wrote {out}{source} via native-cranelift executable \
                         (entry takes {} word argument(s))",
                        artifact.entry_arity
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("marv build: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn link_native_executable(out: &str, artifact: &codegen::AotObject) -> Result<(), String> {
    if artifact.entry_arity > 4 {
        return Err(format!(
            "native executable wrapper supports up to four entry arguments, got {}",
            artifact.entry_arity
        ));
    }
    let tmp = unique_temp_dir("marv-aot")?;
    let obj = tmp.join("module.o");
    let runtime = tmp.join("runtime.c");
    std::fs::write(&obj, &artifact.bytes).map_err(|e| format!("{}: {e}", obj.display()))?;
    std::fs::write(&runtime, native_runtime_c(artifact))
        .map_err(|e| format!("{}: {e}", runtime.display()))?;
    let status = Command::new("cc")
        .arg(&obj)
        .arg(&runtime)
        .arg("-o")
        .arg(out)
        .status()
        .map_err(|e| format!("failed to invoke cc for native executable link: {e}"))?;
    let cleanup = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err(format!(
            "cc failed while linking native executable `{out}` with status {status}"
        ));
    }
    cleanup.map_err(|e| format!("cleanup {}: {e}", tmp.display()))?;
    Ok(())
}

fn native_runtime_c(artifact: &codegen::AotObject) -> String {
    let entry = &artifact.entry_symbol;
    let params = (0..artifact.entry_arity)
        .map(|i| format!("int64_t a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let args = (0..artifact.entry_arity)
        .map(|i| format!("argv_i64[{i}]"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static int64_t **marv_heap = 0;
static int64_t marv_heap_len = 0;
static int64_t marv_heap_cap = 0;

int64_t marv_rt_alloc(int64_t n_words) {{
    if (n_words < 0) n_words = 0;
    int64_t *buf = (int64_t *)calloc((size_t)n_words, sizeof(int64_t));
    if (!buf) {{
        fprintf(stderr, "marv: allocation failed\n");
        abort();
    }}
    if (marv_heap_len == marv_heap_cap) {{
        int64_t next = marv_heap_cap ? marv_heap_cap * 2 : 64;
        int64_t **grown = (int64_t **)realloc(marv_heap, (size_t)next * sizeof(int64_t *));
        if (!grown) {{
            fprintf(stderr, "marv: runtime heap tracking failed\n");
            abort();
        }}
        marv_heap = grown;
        marv_heap_cap = next;
    }}
    marv_heap[marv_heap_len++] = buf;
    return (int64_t)(intptr_t)buf;
}}

int64_t marv_rt_heap_mark(void) {{
    return marv_heap_len;
}}

void marv_rt_heap_reset(int64_t mark) {{
    if (mark < 0) mark = 0;
    if (mark > marv_heap_len) mark = marv_heap_len;
    for (int64_t i = mark; i < marv_heap_len; i++) {{
        free(marv_heap[i]);
    }}
    marv_heap_len = mark;
}}

void marv_rt_bounds_fail(int64_t index, int64_t len) {{
    fprintf(stderr, "marv: bounds check failed: index %lld out of range for length %lld (Tier 1)\n",
            (long long)index, (long long)len);
    abort();
}}

extern int64_t {entry}({params});

static int parse_i64(const char *s, int64_t *out) {{
    errno = 0;
    char *end = 0;
    long long v = strtoll(s, &end, 10);
    if (errno || !end || *end != 0) return 0;
    *out = (int64_t)v;
    return 1;
}}

int main(int argc, char **argv) {{
    if (argc - 1 != {arity}) {{
        fprintf(stderr, "marv: entry expects {arity} integer argument(s), got %d\n", argc - 1);
        return 2;
    }}
    int64_t argv_i64[4] = {{0, 0, 0, 0}};
    for (int i = 0; i < argc - 1; i++) {{
        if (!parse_i64(argv[i + 1], &argv_i64[i])) {{
            fprintf(stderr, "marv: argument %d `%s` is not an integer\n", i, argv[i + 1]);
            return 2;
        }}
    }}
    int64_t result = {entry}({args});
    marv_rt_heap_reset(0);
    printf("%lld\n", (long long)result);
    return 0;
}}
"#,
        arity = artifact.entry_arity,
    )
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before epoch: {e}"))?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Default `.wasm` output path: the input file with its extension replaced.
fn default_wasm_out(file: &str) -> String {
    let base = input_base(file);
    format!("{base}.wasm")
}

fn default_object_out(file: &str) -> String {
    let base = input_base(file);
    format!("{base}.o")
}

fn default_exe_out(file: &str) -> String {
    let base = input_base(file);
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn input_base(file: &str) -> &str {
    Path::new(file)
        .to_str()
        .and_then(|p| {
            p.strip_suffix(".core.json")
                .or_else(|| p.strip_suffix(".mv"))
        })
        .unwrap_or(file)
}

fn join_exports(exports: &[wasm::ExportInfo]) -> String {
    exports
        .iter()
        .map(|e| format!("{}/{}", e.name, e.arity))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- run ----------------------------------------------------------------

/// `marv run [--grant CAP,CAP] [--entry NAME] <file> [args...]` — interpret the
/// entry point with an explicit capability grant set (`spec/03` §4.5).
fn cmd_run(args: &[String]) -> ExitCode {
    let inv = match parse_invocation(args) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("marv run: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(file) = &inv.file else {
        eprintln!("marv run: expected a file");
        return ExitCode::FAILURE;
    };

    // The interpreter is the debug runner: Tier-1 checks (contracts, bounds)
    // always run. Say so rather than silently ignoring the flag — `--release`
    // changes observable semantics under `build`, so silence here could be
    // misread as a backend disagreement.
    if inv.release {
        eprintln!(
            "marv run: note: --release is ignored — the interpreter is the debug runner and \
             always performs Tier-1 checks (use `marv build --release` for unchecked codegen)"
        );
    }

    let loaded = match load(file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("marv run: {e}");
            return ExitCode::FAILURE;
        }
    };

    let diags = loaded.check();
    print_diags(&diags);
    if any_errors(&diags) {
        eprintln!("marv run: {file}: refusing to run (checker reported errors)");
        return ExitCode::FAILURE;
    }

    let program = if let Some(store_dir) = &inv.store_dir {
        let pinned = match pin_loaded(&loaded, store_dir, &inv.entry) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("marv run: {e}");
                return ExitCode::FAILURE;
            }
        };
        Program::new_hashed(pinned.defs_for_backend(), pinned.aliases, pinned.world)
    } else {
        let Loaded {
            runtime_defs,
            runtime_aliases,
            world,
            ..
        } = loaded;
        Program::new_hashed(runtime_defs, runtime_aliases, world)
    };
    match program.run(&inv.entry, &inv.grant, &inv.args) {
        Ok(outcome) => {
            println!("{}", outcome.value.render());
            for e in &outcome.effects {
                let rendered: Vec<String> = e.args.iter().map(|a| a.render()).collect();
                eprintln!("effect: {} op#{} [{}]", e.cap, e.op, rendered.join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("marv run: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---- shared invocation parsing -----------------------------------------

/// The flags and operands shared by `build` and `run`.
struct Invocation {
    target: String,
    run: bool,
    /// `--release`: omit the Tier-1 debug checks (today: the runtime bounds
    /// check, MARV-34) from the compiled artifact.
    release: bool,
    entry: String,
    grant: Vec<String>,
    /// Output path for `build` artifacts (`--out`/`-o`); defaults per target.
    out: Option<String>,
    /// Native artifact kind (`object` or `exe`) requested by `build`.
    emit: Option<String>,
    /// Content store to resolve imports/build dependencies from.
    store_dir: Option<String>,
    file: Option<String>,
    args: Vec<String>,
}

/// Parse `[--target T] [--run] [--emit KIND] [--entry NAME] [--grant LIST]
/// <file> [args...]`.
/// The first non-flag operand is the file; everything after it is passed
/// through as program arguments.
fn parse_invocation(args: &[String]) -> Result<Invocation, String> {
    let mut inv = Invocation {
        target: "native-cranelift".to_string(),
        run: false,
        release: false,
        entry: String::new(),
        grant: Vec::new(),
        out: None,
        emit: None,
        store_dir: None,
        file: None,
        args: Vec::new(),
    };
    let mut i = 0;
    let mut only_args = false; // set once a literal `--` is seen
    while i < args.len() {
        let a = &args[i];
        if only_args {
            inv.args.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            // A literal `--` ends flag parsing; the rest are program arguments
            // even if they look like flags.
            "--" => only_args = true,
            "--run" => inv.run = true,
            "--release" => inv.release = true,
            "--target" => inv.target = take_value(args, &mut i, "--target")?,
            "--emit" => inv.emit = Some(take_value(args, &mut i, "--emit")?),
            "--out" | "-o" => inv.out = Some(take_value(args, &mut i, "--out")?),
            "--store" => inv.store_dir = Some(take_value(args, &mut i, "--store")?),
            "--entry" => inv.entry = take_value(args, &mut i, "--entry")?,
            "--grant" => {
                let list = take_value(args, &mut i, "--grant")?;
                inv.grant.extend(
                    list.split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                );
            }
            // Flags are recognized in any position; the first bare operand is the
            // file, and every bare operand after it is a program argument.
            flag if flag.starts_with("--") => return Err(format!("unknown flag `{flag}`")),
            _ if inv.file.is_none() => inv.file = Some(a.clone()),
            _ => inv.args.push(a.clone()),
        }
        i += 1;
    }
    Ok(inv)
}

/// Consume the value following a `--flag` token, advancing the cursor past it.
fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

/// Parse exactly `arity` command-line integers for a Cranelift `--run`.
fn parse_int_args(args: &[String], arity: usize) -> Result<Vec<i64>, String> {
    if args.len() != arity {
        return Err(format!(
            "entry expects {arity} integer argument(s), got {}",
            args.len()
        ));
    }
    args.iter()
        .enumerate()
        .map(|(i, s)| {
            s.parse::<i64>()
                .map_err(|_| format!("argument {i} `{s}` is not an integer"))
        })
        .collect()
}

// ---- fmt ----------------------------------------------------------------

/// `marv fmt` — canonicalize source via `marv-syntax::format`.
fn cmd_fmt(args: &[String]) -> ExitCode {
    let check_only = args.iter().any(|a| a == "--check");
    let write = args.iter().any(|a| a == "--write");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    if check_only && write {
        eprintln!("marv fmt: --check and --write are mutually exclusive");
        return ExitCode::FAILURE;
    }

    // No files: act as a stdin -> stdout filter.
    if files.is_empty() {
        let mut src = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut src) {
            eprintln!("marv fmt: reading stdin: {e}");
            return ExitCode::FAILURE;
        }
        let formatted = marv_syntax::format(&src);
        if check_only {
            return if formatted == src {
                ExitCode::SUCCESS
            } else {
                eprintln!("marv fmt: <stdin> is not in canonical form");
                ExitCode::FAILURE
            };
        }
        if let Err(e) = io::stdout().write_all(formatted.as_bytes()) {
            eprintln!("marv fmt: writing stdout: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    // Files: print to stdout by default, rewrite in place with --write, or
    // report non-canonical files with --check.
    let mut had_error = false;
    let mut needs_formatting = false;

    for path in files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("marv fmt: {path}: {e}");
                had_error = true;
                continue;
            }
        };
        let formatted = marv_syntax::format(&src);

        if check_only {
            if formatted != src {
                eprintln!("marv fmt: {path} is not in canonical form");
                needs_formatting = true;
            }
            continue;
        }

        if write {
            if formatted != src {
                if let Err(e) = std::fs::write(path, &formatted) {
                    eprintln!("marv fmt: {path}: {e}");
                    had_error = true;
                }
            }
        } else if let Err(e) = io::stdout().write_all(formatted.as_bytes()) {
            eprintln!("marv fmt: writing stdout: {e}");
            return ExitCode::FAILURE;
        }
    }

    if had_error || needs_formatting {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
