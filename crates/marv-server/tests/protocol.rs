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
//! - **The Fs flow is driven over ingested Core, not source.** The M0 front end
//!   emits no `perform`, so a capability misuse cannot be *written* in `.mv`
//!   source yet (documented in `marv_types::check`). The protocol's Core-snapshot
//!   ingestion (`spec/03` §3.1, `marv_db::corespec`) lets the *real* checker run
//!   the flow end to end. The `check`/`applyFix` *requests* are exactly as in the
//!   spec.
//! - **The error code is `E0110`, not the prose example's `E0307`.** §6 fixes the
//!   real numbering by check family (capabilities are `E011x`); the M2 checker
//!   and `spec/03` §6 are the source of truth, and the `E0307` in the §4.1 prose
//!   is an older illustrative number. Likewise spans are `null` (not threaded
//!   through the front end yet — §2 span scope-honesty).

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
    assert!(d["span"].is_null(), "spans not threaded yet (§2)");
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
