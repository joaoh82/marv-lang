//! # marv-mcp — Model Context Protocol server for the marv toolchain
//!
//! Exposes `marv-server`'s JSON-RPC agent protocol (`spec/03`) as MCP **tools**,
//! so any MCP-capable harness (Claude Code/Desktop, Codex, …) can drive the
//! generate→check→repair loop with typed tool calls instead of shelling out.
//!
//! Transport is newline-delimited JSON-RPC 2.0 over stdio (the MCP stdio
//! transport). A single in-process [`marv_server::Server`] is held for the
//! session, so a `marv_open_snapshot` call and the queries against it share
//! state — exactly as an interactive agent expects. Each tool simply forwards
//! its `arguments` to the matching `marv/*` method and returns the result as
//! both text and structured content. See `docs/agents.md` for client wiring.

use std::io::{self, BufRead, Write};

use marv_server::Server;
use serde_json::{json, Map, Value};

/// MCP protocol revision this server speaks.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// One MCP tool, forwarding to a `marv/*` protocol method.
struct Tool {
    /// MCP tool name (snake_case).
    name: &'static str,
    /// The `marv/*` method it forwards to.
    method: &'static str,
    description: &'static str,
    /// `(key, json-schema-type, required)` for the input schema; arguments are
    /// forwarded verbatim, so this is advisory (additionalProperties allowed).
    params: &'static [(&'static str, &'static str, bool)],
}

const SNAP: (&str, &str, bool) = ("snapshotId", "string", true);
const DEF: (&str, &str, bool) = ("def", "string", true);

const TOOLS: &[Tool] = &[
    Tool {
        name: "marv_open_snapshot",
        method: "marv/openSnapshot",
        description: "Open a workspace snapshot from a set of files; returns a snapshotId. \
                      Each file is {path, text} for source, or {path, core} for a Core-IR snapshot.",
        params: &[("files", "array", true)],
    },
    Tool {
        name: "marv_open_package",
        method: "marv/openPackage",
        description: "Open a manifest-backed package from disk (`marv.toml`) as a source snapshot; \
                      `path` may be the package root or any source file inside it.",
        params: &[("path", "string", true)],
    },
    Tool {
        name: "marv_check",
        method: "marv/check",
        description: "Type / effect / capability / error-set / reference / linearity check; \
                      returns diagnostics (stable codes E0001…) carrying machine-applicable fixes.",
        params: &[SNAP],
    },
    Tool {
        name: "marv_signature",
        method: "marv/signature",
        description: "The full signature of a definition (params, return, purity, effects, error set) without its body.",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_error_set",
        method: "marv/errorSet",
        description: "The inferred error set of a definition.",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_effects",
        method: "marv/effects",
        description: "The capabilities a definition's body exercises (its inferred effect row).",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_unsafe_sites",
        method: "marv/unsafeSites",
        description: "List unsafe functions/blocks in a snapshot with SAFETY justifications for audit.",
        params: &[SNAP],
    },
    Tool {
        name: "marv_callers",
        method: "marv/callers",
        description: "Definitions that call the given definition.",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_callees",
        method: "marv/callees",
        description: "Definitions the given definition calls.",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_canonical",
        method: "marv/canonical",
        description: "Canonical (formatted) source of the snapshot or a single definition.",
        params: &[SNAP, ("def", "string", false)],
    },
    Tool {
        name: "marv_core",
        method: "marv/core",
        description: "The Core IR of a definition plus its content hash and dependency edges.",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_hash",
        method: "marv/hash",
        description: "The content hash of a definition.",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_type_at",
        method: "marv/typeAt",
        description: "The type at a byte offset in a file.",
        params: &[SNAP, ("file", "string", true), ("byte", "integer", true)],
    },
    Tool {
        name: "marv_apply_fix",
        method: "marv/applyFix",
        description: "Apply a fix carried by a diagnostic; returns a new snapshot and a re-check.",
        params: &[SNAP, ("def", "string", false)],
    },
    Tool {
        name: "marv_format",
        method: "marv/format",
        description: "Normalize the whole snapshot to canonical form; returns a new snapshot.",
        params: &[SNAP],
    },
    Tool {
        name: "marv_verify",
        method: "marv/verify",
        description: "Discharge a definition's contracts via SMT (Tier 2): proved / failed (with a \
                      counterexample) / unsupported (falls back to runtime checks).",
        params: &[SNAP, DEF],
    },
    Tool {
        name: "marv_commit",
        method: "marv/commit",
        description: "Freeze a snapshot's definitions into the content-addressed store; returns the \
                      lockfile delta (new vs. already-reviewed hashes).",
        params: &[SNAP],
    },
];

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut out = io::stdout();
    let mut server = Server::new();
    let mut internal_id: u64 = 0;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(
                    &mut out,
                    error_envelope(Value::Null, -32700, &format!("parse error: {e}")),
                )?;
                continue;
            }
        };

        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let outcome = dispatch(&mut server, method, params, &mut internal_id);

        // Notifications (no `id`) get no response.
        let Some(id) = id else { continue };
        let envelope = match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, msg)) => error_envelope(id, code, &msg),
        };
        send(&mut out, envelope)?;
    }
    Ok(())
}

fn dispatch(
    server: &mut Server,
    method: &str,
    params: Value,
    internal_id: &mut u64,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "marv-mcp", "version": env!("CARGO_PKG_VERSION") },
        })),
        // Notifications — acknowledged, response suppressed by the caller.
        m if m.starts_with("notifications/") => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => tools_call(server, &params, internal_id),
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

fn tool_list() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": input_schema(t),
            })
        })
        .collect()
}

fn input_schema(t: &Tool) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for (key, ty, req) in t.params {
        props.insert((*key).to_string(), json!({ "type": ty }));
        if *req {
            required.push(json!(key));
        }
    }
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": required,
        "additionalProperties": true,
    })
}

fn tools_call(
    server: &mut Server,
    params: &Value,
    internal_id: &mut u64,
) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires `name`".to_string()))?;
    let tool = TOOLS
        .iter()
        .find(|t| t.name == name)
        .ok_or((-32602, format!("unknown tool `{name}`")))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Forward to the marv protocol method.
    *internal_id += 1;
    let marv_req = json!({
        "jsonrpc": "2.0",
        "id": *internal_id,
        "method": tool.method,
        "params": arguments,
    });
    let resp = server.handle_request(marv_req);

    if let Some(err) = resp.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error");
        // A protocol-level error is reported as a tool error, not a transport error,
        // so the agent sees it as a result it can react to.
        return Ok(json!({
            "content": [{ "type": "text", "text": format!("marv error: {msg}") }],
            "isError": true,
        }));
    }
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": result,
    }))
}

fn error_envelope(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn send(out: &mut impl Write, value: Value) -> io::Result<()> {
    writeln!(out, "{}", serde_json::to_string(&value)?)?;
    out.flush()
}
