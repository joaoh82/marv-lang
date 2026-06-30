//! M3 acceptance gate: drive the JSON-RPC server with the worked-example
//! requests of `spec/03` §4 and assert the example responses.
//!
//! Three flows from the spec:
//!
//! 1. **§4.1 — the missing-`Fs` fix flow.** `check` surfaces a missing-capability
//!    diagnostic carrying the fix `add capability parameter \`fs: Fs\``;
//!    `applyFix` applies it and a re-check is clean.
//! 2. **§4.2 — `signature`.** The full signature of a def without its body.
//! 3. **§4.4 — `core` + `hash`.** The Core IR and content identity of a def.
//!
//! ## Two honest departures from the literal spec text
//!
//! - **The original Fs flow is driven over ingested Core.** Source-level
//!   capability misuse is now expressible too, and has its own `applyFix`
//!   regression below. The Core-snapshot path remains because the protocol
//!   accepts both `.mv` source and hand-authored Core (`spec/03` §3.1,
//!   `marv_db::corespec`).
//! - **The error code is `E0110`, not the prose example's `E0307`.** §6 fixes the
//!   real numbering by check family (capabilities are `E011x`); the M2 checker
//!   and `spec/03` §6 are the source of truth, and the `E0307` in the §4.1 prose
//!   is an older illustrative number. Spans over *Core-ingested* files are `null`
//!   (Core has no source text); over `.mv` source they are real spans, including
//!   source-fix edit spans where the front end can derive them.

use marv_core::ir::*;
use marv_core::symbol_hash;
use marv_db::{CapSpec, CoreDefSpec, CoreModuleSpec, OpSpec, WorldSpec};
use marv_server::{serve, Server};
use serde_json::{json, Value};
use std::io::Cursor;

/// Send a request and unwrap its `result`, asserting there was no `error`.
fn call(server: &mut Server, method: &str, params: Value) -> Value {
    let resp = server.handle_request(json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params
    }));
    assert!(
        resp.get("error").is_none(),
        "{method} returned an error: {resp:#}"
    );
    resp.get("result").cloned().unwrap_or(Value::Null)
}

/// The `report` module ingested as Core: `load(fs: Fs, path: str)` performs an
/// `Fs` operation, but its declared effect row is empty — exactly the §4.1 bug.
fn report_core_module() -> Value {
    let fs_ty = Type::Nominal {
        def: symbol_hash("Fs"),
        args: Vec::new(),
    };
    // ty: Fs -> (str -> ()), every arrow row empty (the missing `Fs` is the bug).
    let ty = Type::Arrow {
        param: Box::new(fs_ty.clone()),
        ret: Box::new(Type::Arrow {
            param: Box::new(Type::Str),
            ret: Box::new(Type::Unit),
            effects: EffectRow::empty(),
        }),
        effects: EffectRow::empty(),
    };
    // body: λfs. λpath. perform(fs, read, path)
    //   de Bruijn at depth 2: fs = Var(1), path = Var(0).
    let body = Core::Lam {
        param: fs_ty,
        effects: EffectRow::empty(),
        body: Box::new(Core::Lam {
            param: Type::Str,
            effects: EffectRow::empty(),
            body: Box::new(Core::Perform {
                cap: Atom::Var(1),
                op: OpId(0),
                args: vec![Atom::Var(0)],
            }),
        }),
    };
    let def = Def {
        kind: DefKind::Fn,
        ty,
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(body),
    };
    let spec = CoreModuleSpec {
        module: "report".into(),
        world: WorldSpec {
            caps: vec![CapSpec {
                name: "Fs".into(),
                ops: vec![OpSpec {
                    consumes_receiver: true,
                    params: vec![Type::Str],
                    ret: Type::Unit,
                    errors: Vec::new(),
                }],
            }],
            ..Default::default()
        },
        defs: vec![CoreDefSpec {
            name: "load".into(),
            params: vec!["fs".into(), "path".into()],
            def,
        }],
    };
    serde_json::to_value(spec).unwrap()
}

#[test]
fn missing_fs_fix_flow() {
    let mut server = Server::new();

    // openSnapshot(files) -> s1
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "report.mv", "core": report_core_module() } ] }),
    );
    let s1 = open["snapshotId"].as_str().unwrap().to_string();
    assert_eq!(s1, "s1");

    // marv/check { snapshotId, scope: { def: "report.load" } }  (spec §4.1)
    let checked = call(
        &mut server,
        "marv/check",
        json!({ "snapshotId": s1, "scope": { "def": "report.load" } }),
    );
    let diags = checked["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {checked:#}");
    let d = &diags[0];
    assert_eq!(d["code"], "E0110"); // MissingCapability (spec §6 family E011x)
    assert_eq!(d["severity"], "error");
    assert!(
        d["span"].is_null(),
        "Core-ingested files have no source span"
    );
    assert!(d["message"].as_str().unwrap().contains("Fs"));

    // The diagnostic carries the mechanical fix, exactly as in §4.1.
    let fix = &d["fixes"][0];
    assert_eq!(fix["title"], "add capability parameter `fs: Fs`");
    assert_eq!(fix["edits"][0]["newText"], "fs: Fs, ");
    assert!(fix["confidence"].as_f64().unwrap() >= 0.8);

    // marv/applyFix { snapshotId, diagnosticCode, def } -> s2, clean.
    let fixed = call(
        &mut server,
        "marv/applyFix",
        json!({ "snapshotId": s1, "diagnosticCode": "E0110", "def": "report.load" }),
    );
    let s2 = fixed["snapshotId"].as_str().unwrap().to_string();
    assert_ne!(s2, s1, "applyFix returns a new snapshot");
    assert_eq!(
        fixed["diagnostics"].as_array().unwrap().len(),
        0,
        "the repaired snapshot is clean: {fixed:#}"
    );

    // And the repair is real: `report.load` now declares the `Fs` effect.
    let eff = call(
        &mut server,
        "marv/effects",
        json!({ "snapshotId": s2, "def": "report.load" }),
    );
    assert_eq!(eff["effects"], json!(["Fs"]));
}

#[test]
fn source_missing_capability_apply_fix_removes_pure_marker() {
    let mut server = Server::new();
    let src = "\
mod demo

interface Fs {
    fn read(fs: &Fs, path: str) -> ![]u8
}

pure fn read_file(fs: Fs, path: str) -> ![]u8 {
    fs.read(path)
}
";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "demo.mv", "text": src } ] }),
    );
    let s1 = open["snapshotId"].as_str().unwrap().to_string();

    let checked = call(&mut server, "marv/check", json!({ "snapshotId": s1 }));
    let diags = checked["diagnostics"].as_array().unwrap();
    assert_eq!(
        diags.len(),
        1,
        "expected one MissingCapability: {checked:#}"
    );
    let d = &diags[0];
    assert_eq!(d["code"], "E0110");
    let fix = &d["fixes"][0];
    assert_eq!(
        fix["title"],
        "remove `pure` marker so capability parameters declare the effect"
    );
    let span = &fix["edits"][0]["span"];
    assert_eq!(span["file"], "demo.mv");
    let (lo, hi) = (
        span["startByte"].as_u64().unwrap() as usize,
        span["endByte"].as_u64().unwrap() as usize,
    );
    assert_eq!(&src[lo..hi], "pure ");
    assert_eq!(fix["edits"][0]["newText"], "");

    let fixed = call(
        &mut server,
        "marv/applyFix",
        json!({ "snapshotId": s1, "diagnosticCode": "E0110", "def": "demo.read_file" }),
    );
    let s2 = fixed["snapshotId"].as_str().unwrap().to_string();
    assert_ne!(s2, s1, "applyFix returns a new snapshot");
    assert_eq!(
        fixed["diagnostics"].as_array().unwrap().len(),
        0,
        "source repair re-checks clean: {fixed:#}"
    );

    let sig = call(
        &mut server,
        "marv/signature",
        json!({ "snapshotId": s2, "def": "demo.read_file" }),
    );
    assert_eq!(sig["pure"], false);
    assert_eq!(sig["effects"], json!(["Fs"]));
}

#[test]
fn unsafe_sites_lists_source_safety_justifications() {
    let mut server = Server::new();
    let src = "\
mod demo

/// SAFETY: the host symbol follows the marv i64 ABI.
unsafe extern fn host_add_one(x: i64) -> i64

/// SAFETY: host boundary validates the raw value before calling this function.
unsafe fn raw_value(x: i64) -> i64 {
    x
}

pure fn safe_value(x: i64) -> i64 {
    x
}
";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "demo.mv", "text": src } ] }),
    );
    let snapshot = open["snapshotId"].as_str().unwrap();

    let unsafe_sites = call(
        &mut server,
        "marv/unsafeSites",
        json!({ "snapshotId": snapshot }),
    );
    let sites = unsafe_sites["sites"].as_array().unwrap();
    assert_eq!(
        sites.len(),
        2,
        "expected two unsafe sites: {unsafe_sites:#}"
    );
    assert_eq!(sites[0]["file"], "demo.mv");
    assert_eq!(sites[0]["def"], "demo.host_add_one");
    assert!(sites[0]["hash"].as_str().unwrap().starts_with("b3:"));
    assert_eq!(
        sites[0]["justification"],
        "the host symbol follows the marv i64 ABI."
    );
    assert_eq!(sites[0]["span"]["file"], "demo.mv");
    assert_eq!(sites[1]["file"], "demo.mv");
    assert_eq!(sites[1]["def"], "demo.raw_value");
    assert!(sites[1]["hash"].as_str().unwrap().starts_with("b3:"));
    assert_eq!(
        sites[1]["justification"],
        "host boundary validates the raw value before calling this function."
    );
    assert_eq!(sites[1]["span"]["file"], "demo.mv");
}

#[test]
fn signature_without_body() {
    // §4.2: the full signature of a def. Driven from real `.mv` source — the
    // front end gives real parameter names and surface-spelled types.
    let mut server = Server::new();
    let src = "mod report\n\nfn load(fs: Fs, path: str) -> Config {\n    path\n}\n";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "report.mv", "text": src } ] }),
    );
    let s = open["snapshotId"].as_str().unwrap().to_string();

    let sig = call(
        &mut server,
        "marv/signature",
        json!({ "snapshotId": s, "def": "report.load" }),
    );
    assert_eq!(sig["name"], "report.load");
    assert_eq!(
        sig["params"],
        json!([
            { "name": "fs", "type": "Fs" },
            { "name": "path", "type": "str" }
        ])
    );
    assert_eq!(sig["ret"], "Config");
    assert_eq!(sig["pure"], false);
    // effects/errorSet are empty from source (the front end infers none yet —
    // they populate over ingested Core, see the Fs flow).
    assert_eq!(sig["effects"], json!([]));
    assert_eq!(sig["errorSet"], json!([]));
    assert!(sig["hash"].as_str().unwrap().starts_with("b3:"));
}

#[test]
fn stdio_ndjson_transport() {
    // The real wire path: line-delimited JSON-RPC over a reader/writer pair
    // (stdio in the binary). Two requests in, two response lines out.
    let mut server = Server::new();
    let requests = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"marv/openSnapshot","params":{"files":[{"path":"m.mv","text":"mod m\n\npure fn f(x: i32) -> i32 {\n    (x + 1)\n}\n"}]}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"marv/check","params":{"snapshotId":"s1"}}"#,
        "\n",
    );
    let mut out: Vec<u8> = Vec::new();
    serve(&mut server, Cursor::new(requests.as_bytes()), &mut out).unwrap();

    let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
    assert_eq!(lines.len(), 2, "one response per request");

    let r1: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(r1["id"], 1);
    assert_eq!(r1["result"]["snapshotId"], "s1");

    let r2: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(r2["id"], 2);
    // A well-formed function: no diagnostics.
    assert_eq!(r2["result"]["diagnostics"].as_array().unwrap().len(), 0);
}

#[test]
fn source_snapshot_checks_as_a_module_set() {
    let mut server = Server::new();
    let app = "mod app\nimport math (double)\n\npure fn main() -> i64 {\n    double(21)\n}\n";
    let math = "mod math\n\npure fn double(x: i64) -> i64 {\n    (x * 2)\n}\n";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({
            "files": [
                { "path": "app.mv", "text": app },
                { "path": "math.mv", "text": math }
            ]
        }),
    );
    let s = open["snapshotId"].as_str().unwrap().to_string();

    let checked = call(&mut server, "marv/check", json!({ "snapshotId": s }));
    assert_eq!(
        checked["diagnostics"].as_array().unwrap().len(),
        0,
        "two-file package snapshot checks cleanly: {checked:#}"
    );
}

#[test]
fn unknown_method_is_jsonrpc_error() {
    let mut server = Server::new();
    let resp = server.handle_request(json!({
        "jsonrpc": "2.0", "id": 7, "method": "marv/teleport", "params": {}
    }));
    assert_eq!(resp["id"], 7);
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn core_and_hash() {
    // §4.4: the Core IR and content identity of a def. `clamp` is expressible in
    // M0 source and lowers (via `if`/`else`) to a nested `Match`.
    let mut server = Server::new();
    let src = "mod math\n\npure fn clamp(x: i32, lo: i32, hi: i32) -> i32 {\n    if (x < lo) {\n        lo\n    } else {\n        if (x > hi) {\n            hi\n        } else {\n            x\n        }\n    }\n}\n";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "math.mv", "text": src } ] }),
    );
    let s = open["snapshotId"].as_str().unwrap().to_string();

    // marv/core { snapshotId, def } -> { hash, core, deps, alphaCanonical }
    let core = call(
        &mut server,
        "marv/core",
        json!({ "snapshotId": s, "def": "math.clamp" }),
    );
    let hash = core["hash"].as_str().unwrap().to_string();
    assert!(hash.starts_with("b3:"));
    assert_eq!(core["alphaCanonical"], true);
    // The Core term is a lambda (curried params), exactly the §4.4 shape
    // `{ "Lam": { "param": {"Int":"I32"}, "effects": {...}, "body": {...} } }`.
    let lam = &core["core"]["Lam"];
    assert!(lam.is_object(), "core root is a Lam: {core:#}");
    assert_eq!(lam["param"], json!({ "Int": "I32" }));
    assert_eq!(lam["effects"], json!({ "caps": [], "errors": [] }));

    // marv/hash agrees with marv/core's hash.
    let h = call(
        &mut server,
        "marv/hash",
        json!({ "snapshotId": s, "def": "math.clamp" }),
    );
    assert_eq!(h["hash"], core["hash"]);

    // M1 identity property surfaced over the protocol: an alpha-equivalent
    // program (renamed locals, reflowed) hashes identically.
    let mut server2 = Server::new();
    let alpha = "mod math\n\npure fn clamp(a: i32, b: i32, c: i32) -> i32 {\n    if (a < b) { b } else { if (a > c) { c } else { a } }\n}\n";
    let open2 = call(
        &mut server2,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "math.mv", "text": alpha } ] }),
    );
    let s2 = open2["snapshotId"].as_str().unwrap().to_string();
    let core2 = call(
        &mut server2,
        "marv/core",
        json!({ "snapshotId": s2, "def": "math.clamp" }),
    );
    assert_eq!(core2["hash"], core["hash"], "alpha-equivalent ⇒ same hash");
}

/// §4.3 — `verify`: a correct `clamp` proves; a buggy one fails with a concrete
/// counterexample. Needs a z3 binary; when absent the server reports
/// `unsupported` (the Tier-1 fallback) and the test skips rather than fails.
const CLAMP_PAIR: &str = "\
mod math

pure fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures result >= lo and result <= hi
{
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

pure fn clampbug(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures result >= lo and result <= hi
{
    if x > hi {
        hi
    } else {
        x
    }
}
";

#[test]
fn verify_proves_and_counterexamples_over_the_protocol() {
    let mut server = Server::new();
    let opened = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [{ "path": "math.mv", "text": CLAMP_PAIR }] }),
    );
    let snap = opened.get("snapshotId").cloned().unwrap();

    let ok = call(
        &mut server,
        "marv/verify",
        json!({ "snapshotId": snap, "def": "math.clamp" }),
    );
    // Skip if no solver is available in this environment.
    if ok.get("status").and_then(Value::as_str) == Some("unsupported")
        && ok
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|r| r.contains("z3"))
    {
        eprintln!("skipping: no z3 solver available");
        return;
    }

    assert_eq!(ok["status"], "proved", "correct clamp should prove: {ok:#}");
    // The result carries a `relatedSpan` pointing at the verified definition's
    // header in source (MARV-12).
    let rs = &ok["relatedSpan"];
    assert_eq!(
        rs["file"], "math.mv",
        "relatedSpan names the source file: {ok:#}"
    );
    let (lo, hi) = (
        rs["startByte"].as_u64().unwrap() as usize,
        rs["endByte"].as_u64().unwrap() as usize,
    );
    assert_eq!(
        &CLAMP_PAIR[lo..hi],
        "pure fn clamp",
        "span covers the header"
    );

    let bug = call(
        &mut server,
        "marv/verify",
        json!({ "snapshotId": snap, "def": "math.clampbug" }),
    );
    assert_eq!(bug["status"], "failed", "buggy clamp should fail: {bug:#}");
    let cx = &bug["counterexample"];
    assert!(cx.is_object(), "counterexample is an object: {bug:#}");
    // The model assigns the parameters and the result as JSON numbers.
    assert!(cx.get("x").is_some() && cx.get("result").is_some());
    assert!(
        bug["obligation"].as_str().unwrap().contains("result"),
        "obligation names the violated clause: {bug:#}"
    );
}

/// MARV-12 — over `.mv` source, a diagnostic carries the real span of the
/// definition's header (keyword(s) through name), and doc comments preceding the
/// definition are excluded from that header span.
#[test]
fn source_diagnostic_carries_real_header_span() {
    let mut server = Server::new();
    // A doc-commented function whose body type does not match its return type:
    // the M2 checker raises `TypeMismatch` (E0101) from real source.
    let src = "mod m\n\n/// A documented but wrong function.\n/// Second line.\nfn f() -> i32 {\n    true\n}\n";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "m.mv", "text": src } ] }),
    );
    let s = open["snapshotId"].as_str().unwrap().to_string();

    let checked = call(&mut server, "marv/check", json!({ "snapshotId": s }));
    let diags = checked["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "one type-mismatch diagnostic: {checked:#}");
    let span = &diags[0]["span"];
    assert!(
        !span.is_null(),
        "source diagnostics carry a real span: {checked:#}"
    );
    assert_eq!(span["file"], "m.mv");
    let (lo, hi) = (
        span["startByte"].as_u64().unwrap() as usize,
        span["endByte"].as_u64().unwrap() as usize,
    );
    // The header span is the `fn f` declaration — NOT the `///` doc lines above it.
    assert_eq!(&src[lo..hi], "fn f");
    // Line/col are 0-based: `fn f` is on line 4 (after `mod m`, blank, two docs).
    assert_eq!(span["start"]["line"], 4);
    assert_eq!(span["start"]["col"], 0);
}

/// MARV-12 — `typeAt` resolves an offset to its enclosing definition and returns
/// that definition's real header span.
#[test]
fn type_at_returns_real_span() {
    let mut server = Server::new();
    let src = "mod m\n\npure fn add(a: i32, b: i32) -> i32 {\n    (a + b)\n}\n";
    let open = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [ { "path": "m.mv", "text": src } ] }),
    );
    let s = open["snapshotId"].as_str().unwrap().to_string();

    // An offset inside the body resolves to `m.add`.
    let byte = src.find("(a + b)").unwrap() as u64;
    let ta = call(
        &mut server,
        "marv/typeAt",
        json!({ "snapshotId": s, "file": "m.mv", "byte": byte }),
    );
    assert_eq!(ta["def"], "m.add");
    assert_eq!(ta["type"], "fn(i32, i32) -> i32");
    let span = &ta["span"];
    let (lo, hi) = (
        span["startByte"].as_u64().unwrap() as usize,
        span["endByte"].as_u64().unwrap() as usize,
    );
    assert_eq!(&src[lo..hi], "pure fn add");
}

/// §3.4 — `commit`: freezing a snapshot's definitions into the content-addressed
/// store is reported as a delta, and is idempotent (re-committing the same
/// source adds nothing and reports the defs as already reviewed).
#[test]
fn commit_freezes_and_is_idempotent() {
    let src = "\
mod demo
pure fn factorial(n: i64) -> i64 {
    if n < 2 {
        1
    } else {
        n * factorial(n - 1)
    }
}
";
    let mut server = Server::new();
    let opened = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [{ "path": "demo.mv", "text": src }] }),
    );
    let snap = opened.get("snapshotId").cloned().unwrap();

    let first = call(&mut server, "marv/commit", json!({ "snapshotId": snap }));
    assert_eq!(first["added"], 1, "first commit adds the def: {first:#}");
    assert_eq!(first["alreadyReviewed"], 0);
    let hash = first["committed"][0]["hash"].as_str().unwrap().to_string();
    assert!(hash.starts_with("b3:"));

    let second = call(&mut server, "marv/commit", json!({ "snapshotId": snap }));
    assert_eq!(second["added"], 0, "re-commit adds nothing: {second:#}");
    assert_eq!(second["alreadyReviewed"], 1);
    assert_eq!(second["committed"][0]["hash"].as_str().unwrap(), hash);
    assert_eq!(second["committed"][0]["reviewed"], true);
    assert_eq!(second["storeSize"], 1);
}

/// MARV-22 — `verify` discharges loop invariants over the protocol: a loop
/// with a strong enough invariant proves; a wrong invariant fails with the
/// carried state (positional `s{j}` labels, primed post-iteration values) in
/// the counterexample. Skips without a z3 binary, like the clamp test above.
const LOOP_PAIR: &str = "\
mod loops

pure fn count_down_sum(n: i64) -> i64
    requires n >= 0 and n <= 1000000000
    ensures result >= 0
{
    var total: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
        invariant (i <= n)
        invariant (total >= 0)
        invariant (total <= (n - i))
    {
        total = (total + 1)
        i = (i - 1)
    }
    total
}

pure fn badloop(n: i64) -> i64
    requires n >= 0
{
    var i: i64 = 0
    while (i < n)
        invariant (i <= 0)
    {
        i = (i + 1)
    }
    i
}
";

#[test]
fn verify_discharges_loop_invariants_over_the_protocol() {
    let mut server = Server::new();
    let opened = call(
        &mut server,
        "marv/openSnapshot",
        json!({ "files": [{ "path": "loops.mv", "text": LOOP_PAIR }] }),
    );
    let snap = opened.get("snapshotId").cloned().unwrap();

    let ok = call(
        &mut server,
        "marv/verify",
        json!({ "snapshotId": snap, "def": "loops.count_down_sum" }),
    );
    if ok.get("status").and_then(Value::as_str) == Some("unsupported")
        && ok
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|r| r.contains("z3"))
    {
        eprintln!("skipping: no z3 solver available");
        return;
    }
    assert_eq!(
        ok["status"], "proved",
        "the bounded loop's invariant should prove: {ok:#}"
    );

    let bad = call(
        &mut server,
        "marv/verify",
        json!({ "snapshotId": snap, "def": "loops.badloop" }),
    );
    assert_eq!(
        bad["status"], "failed",
        "a wrong invariant should fail: {bad:#}"
    );
    assert!(
        bad["message"]
            .as_str()
            .is_some_and(|m| m.contains("not preserved")),
        "the consecution failure is named: {bad:#}"
    );
    let cx = &bad["counterexample"];
    assert!(
        cx.get("s0").is_some() && cx.get("s0'").is_some(),
        "carried state (pre and post) in the counterexample: {bad:#}"
    );
}
