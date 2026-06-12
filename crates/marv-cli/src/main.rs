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

use std::io::{self, Read, Write};
use std::process::ExitCode;

use marv_codegen_cl as codegen;
use marv_codegen_wasm as wasm;
use marv_interp::Program;
use marv_store::{commit, CommitStatus, StoreDir};
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
    build [--target T] [--run] [--release] [--out PATH] [--entry NAME] <file> [args...]
                               Compile. Refuses to build a file that fails
                               `check`. Targets: `native-cranelift` (default;
                               --run JIT-executes the entry and prints its
                               integer result) and `wasm-component` (writes a
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

    let report = commit(&mut store, &mut lock, &loaded.module_path, &loaded.defs);

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

// ---- build --------------------------------------------------------------

/// `marv build [--target T] [--run] [--out PATH] [--entry NAME] <file>` —
/// check, then compile with the selected backend.
///
/// Targets: `native-cranelift` (Cranelift JIT; `--run` executes it) and
/// `wasm-component` (a WebAssembly module written to `--out`, default
/// `<file>.wasm`). Both refuse code that fails `check`, and both compile only
/// the definitions reachable from the entry (MARV-8) — `commit`/audit flows
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

    match inv.target.as_str() {
        "native-cranelift" => build_native(&inv, &file, &loaded),
        // `wasm-component` is the spec's name for the WASM target; today the
        // artifact is a core module (the component's substrate), with
        // capabilities as host imports per the component model (`spec/01` §9).
        "wasm-component" | "wasm" => build_wasm(&inv, &file, &loaded),
        other => {
            eprintln!(
                "marv build: unsupported target `{other}` (have `native-cranelift`, \
                 `wasm-component`; LLVM is a later milestone)"
            );
            ExitCode::FAILURE
        }
    }
}

/// Cranelift backend: JIT-compile, and with `--run` execute the entry point.
fn build_native(inv: &Invocation, file: &str, loaded: &Loaded) -> ExitCode {
    let opts = codegen::Options {
        bounds_checks: !inv.release,
    };
    // Compile only what the entry reaches (MARV-8): a sibling definition the
    // backend cannot lower yet must not block a build that never calls it.
    // Whole-module compilation remains the audit path (`compile_with`).
    let jit = match codegen::compile_reachable(
        &loaded.module_path,
        &loaded.defs,
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

/// WebAssembly backend: emit a `.wasm` module and report its capability
/// manifest (the host imports it requires; a pure module requires none).
fn build_wasm(inv: &Invocation, file: &str, loaded: &Loaded) -> ExitCode {
    let opts = wasm::Options {
        bounds_checks: !inv.release,
    };
    // Same reachability pruning as the native path (MARV-8): only the entry's
    // closure is compiled and exported.
    let artifact = match wasm::compile_reachable(
        &loaded.module_path,
        &loaded.defs,
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

/// Default `.wasm` output path: the input file with its extension replaced.
fn default_wasm_out(file: &str) -> String {
    let base = file
        .strip_suffix(".core.json")
        .or_else(|| file.strip_suffix(".mv"))
        .unwrap_or(file);
    format!("{base}.wasm")
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

    let Loaded {
        module_path,
        defs,
        world,
        ..
    } = loaded;
    let program = Program::new(&module_path, defs, world);
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
    file: Option<String>,
    args: Vec<String>,
}

/// Parse `[--target T] [--run] [--entry NAME] [--grant LIST] <file> [args...]`.
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
            "--out" | "-o" => inv.out = Some(take_value(args, &mut i, "--out")?),
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
