//! Surfaces du daemon en T1 (liste fermee du design Phase 1, section 2.3) :
//! - API lecture seule : GET /health, GET /boot, GET /state. Aucune route
//!   d'ecriture d'etat n'existe.
//! - Serveur MCP (lecture seule) : groupe `daemon` = daemon_status,
//!   boot_state, boot_receipt_last. Aucun outil mutant n'est enregistre —
//!   plus fort que le refus : l'absence (design section 0.5).
//! - Surface d'ancrage : POST /anchor ne fait qu'une chose — refuser, de
//!   facon typee, toute mutation presentee (design section 0.4). Elle
//!   n'ecrit aucun etat ; chaque presentation est consignee au journal.

use crate::envelope::EnvelopeService;
use crate::journal::Journal;
use crate::timefmt;
use crate::workgraph::WorkGraph;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub const MCP_TOOLS: [&str; 6] = [
    "daemon_status",
    "boot_state",
    "boot_receipt_last",
    "veille_status",
    "veille_last_beat",
    "veille_observations",
];

/// Etat de l'ancrage, lu EN DIRECT au store (Phase 2) : le mecanisme est
/// ABSENT jusqu'a l'acte d'activation, puis ACTIVE (design Phase 2, 0.1) ;
/// la politique reste le deny-by-default — rien n'est servi sans enveloppe
/// autorisee ; les capacites accordees sont l'etat reel des baux vivants.
pub fn anchor_json(mechanism: &str, capabilities_granted: i64) -> Value {
    json!({
        "mechanism": mechanism,
        "mechanism_note": if mechanism == "ACTIVE" {
            "service d'enveloppe en execution (Phase 2)"
        } else {
            "mecanisme non active — activation apres preuves H-P2 (Phase 2)"
        },
        "policy": "DENY_ALL",
        "capabilities_granted": capabilities_granted,
    })
}

/// La seule reponse de la surface d'ancrage brute : un refus type.
/// Toute reponse en forme de succes ou de bail simule serait une
/// violation de design (H-P1-8) — les mutations passent par le service
/// d'enveloppe (/envelope/*), jamais par cette surface.
pub fn anchor_refusal(mechanism: &str, capabilities_granted: i64, presented: &Value) -> Value {
    json!({
        "error": {
            "code": "ANCHOR_DENY_ALL",
            "type": "typed_refusal",
            "message": "ancrage d'autorite en deny-by-default : les mutations se presentent au service d'enveloppe (/envelope/effect), jamais a la surface brute",
            "anchor": anchor_json(mechanism, capabilities_granted),
            "presented_kind": presented.get("kind").cloned().unwrap_or(Value::Null),
        }
    })
}

pub struct DaemonState {
    pub boot_id: String,
    pub daemon_version: String,
    pub board_id: String,
    pub stage: String, // "T0" pendant le boot, "T1" apres declaration
    pub t0_unix: i64,
    pub stage_marks: Vec<(String, String)>, // (etape, iso8601)
    pub store_integrity: String,
    pub last_receipt: Option<Value>,
    pub anomalies: Vec<String>,
}

impl DaemonState {
    pub fn state_json(&self, anchor: &Value, watchdog: &Value) -> Value {
        json!({
            "daemon": "noyau",
            "daemon_version": self.daemon_version,
            "board_id": self.board_id,
            "stage": self.stage,
            "t2_ready_at": watchdog.get("t2_ready_at").cloned().unwrap_or(Value::Null),
            "watchdog": watchdog,
            "store_integrity": self.store_integrity,
            "anchor": anchor,
            "anomalies": self.anomalies,
        })
    }
    pub fn boot_json(&self) -> Value {
        let stages: Vec<Value> = self
            .stage_marks
            .iter()
            .map(|(s, at)| json!({"stage": s, "at": at}))
            .collect();
        json!({
            "boot_id": self.boot_id,
            "stage": self.stage,
            "stages": stages,
            "last_receipt": self.last_receipt,
        })
    }
}

pub struct ServerCtx {
    pub state: Arc<RwLock<DaemonState>>,
    pub journal: Arc<Journal>,
    pub envelope: Arc<EnvelopeService>,
    pub workgraph: Arc<WorkGraph>,
    pub veille: Arc<crate::veille::Veille>,
}

fn live_anchor(ctx: &ServerCtx) -> Value {
    let st = ctx.envelope.state_json();
    anchor_json(
        st.get("mechanism").and_then(|m| m.as_str()).unwrap_or("ABSENT"),
        st.get("capabilities_granted").and_then(|c| c.as_i64()).unwrap_or(0),
    )
}

/// Etat vivant du watchdog N2 (H-P4-9/H-P4-10) : stade du relais, version de
/// spec rapportee, t2_ready_at (null jusqu'a la bascule).
fn live_watchdog(ctx: &ServerCtx) -> Value {
    let t2: Value = ctx.veille.store_t2().map(Value::String).unwrap_or(Value::Null);
    let stade = ctx.veille.store_stage();
    json!({
        "service": "veille",
        "source_trace": crate::veille::TRACE_SOURCE,
        "spec_version": crate::veille::SPEC_VERSION,
        "stade": stade,
        "autoritaire": matches!(stade.as_str(), "C" | "D"),
        "t2_ready_at": t2,
    })
}

// ---------- transport HTTP minimal (std uniquement) ----------

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 1_048_576 {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    body.truncate(content_length);
    Some(Request { method, path, body })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn respond(stream: &mut TcpStream, status: u16, body: &Value) {
    let text = body.to_string();
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        text.len(),
        text
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

// ---------- API ----------

fn handle_api(ctx: &ServerCtx, req: &Request) -> (u16, Value) {
    let path_only = req.path.split('?').next().unwrap_or("");
    // --- service d'enveloppe (Phase 2) : point de controle unique
    if req.method == "POST" && (path_only == "/envelope/claim" || path_only == "/envelope/effect") {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let out = if path_only == "/envelope/claim" {
            ctx.envelope.claim(&body)
        } else {
            ctx.envelope.effect(&body)
        };
        return (out.status, out.body);
    }
    if req.method == "GET" && path_only == "/envelope/state" {
        return (200, ctx.envelope.state_json());
    }
    if req.method == "GET" && path_only == "/envelope/proof" {
        let query = req.path.split('?').nth(1).unwrap_or("");
        let mut mission = "";
        let mut operation = "";
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "mission_id" => mission = v,
                    "operation_id" => operation = v,
                    _ => {}
                }
            }
        }
        let out = ctx.envelope.proof(mission, operation);
        return (out.status, out.body);
    }
    // --- WorkGraph type (Phase 3) : transitions sous enveloppe et
    // surface d'interrogation bornee du PRD section 5
    if req.method == "POST" && (path_only == "/workgraph/transition" || path_only == "/workgraph/reconcile") {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let out = if path_only == "/workgraph/transition" {
            ctx.workgraph.transition(&body)
        } else {
            ctx.workgraph.reconcile(&body)
        };
        return (out.status, out.body);
    }
    if req.method == "GET" && path_only.starts_with("/workgraph/") {
        let query = req.path.split('?').nth(1).unwrap_or("");
        let mut mission = "";
        let mut hop = "";
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "mission_id" => mission = v,
                    "hop_id" => hop = v,
                    _ => {}
                }
            }
        }
        let out = match path_only {
            "/workgraph/mission" => ctx.workgraph.mission(mission),
            "/workgraph/assertion" => ctx.workgraph.assertion(mission, hop),
            "/workgraph/eligible" => ctx.workgraph.eligible(mission),
            "/workgraph/lease-holder" => ctx.workgraph.lease_holder(mission),
            "/workgraph/pending-reconciliations" => ctx.workgraph.pending_reconciliations(),
            _ => crate::envelope::Outcome {
                status: 404,
                body: json!({"error": {"code": "NOT_FOUND", "message": "surface WorkGraph inexistante — la surface d'interrogation est bornee (PRD Phase 3, section 5)"}}),
            },
        };
        return (out.status, out.body);
    }
    let state = ctx.state.read().unwrap();
    match (req.method.as_str(), path_only) {
        ("GET", "/health") => (
            200,
            json!({"status": "ok", "daemon": "noyau", "stage": state.stage, "boot_id": state.boot_id}),
        ),
        ("GET", "/boot") => (200, state.boot_json()),
        ("GET", "/state") => {
            let anchor = live_anchor(ctx);
            let watchdog = live_watchdog(ctx);
            (200, state.state_json(&anchor, &watchdog))
        }
        ("POST", "/anchor") => {
            // Surface d'ancrage brute : refus type, consigne au journal.
            let presented: Value =
                serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let anchor = live_anchor(ctx);
            let refusal = anchor_refusal(
                anchor.get("mechanism").and_then(|m| m.as_str()).unwrap_or("ABSENT"),
                anchor.get("capabilities_granted").and_then(|c| c.as_i64()).unwrap_or(0),
                &presented,
            );
            drop(state);
            ctx.journal.append(&json!({
                "kind": "anchor-refusal",
                "boot_id": ctx.state.read().unwrap().boot_id,
                "at": timefmt::iso8601_utc(timefmt::now_unix_secs()),
                "presented": presented,
                "decision": "DENIED",
                "code": "ANCHOR_DENY_ALL",
            }));
            (403, refusal)
        }
        ("GET", _) | ("POST", _) => (
            404,
            json!({"error": {"code": "NOT_FOUND", "message": "surface inexistante — les surfaces du daemon sont celles des designs signes (Phase 1 section 2.3, Phase 2 : service d'enveloppe)"}}),
        ),
        _ => (
            405,
            json!({"error": {"code": "METHOD_NOT_ALLOWED", "message": "API lecture seule hors service d'enveloppe ; la surface d'ancrage brute ne sert que des refus types"}}),
        ),
    }
}

// ---------- MCP (JSON-RPC 2.0 sur HTTP) ----------

fn mcp_tool_result(ctx: &ServerCtx, name: &str) -> Value {
    let state = ctx.state.read().unwrap();
    match name {
        "daemon_status" => state.state_json(&live_anchor(ctx), &live_watchdog(ctx)),
        "boot_state" => state.boot_json(),
        "boot_receipt_last" => state
            .last_receipt
            .clone()
            .unwrap_or(json!({"note": "aucun recu emis pour le boot courant pour l'instant"})),
        "veille_status" => ctx.veille.status_json(),
        "veille_last_beat" => ctx.veille.last_beat_json(),
        "veille_observations" => ctx.veille.observations_json(),
        _ => json!({"error": "outil inconnu"}),
    }
}

fn handle_mcp(ctx: &ServerCtx, req: &Request) -> (u16, Value) {
    if req.method != "POST" {
        return (
            405,
            json!({"error": {"code": "METHOD_NOT_ALLOWED", "message": "le serveur MCP noyau attend des requetes JSON-RPC en POST"}}),
        );
    }
    let rpc: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            return (
                200,
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
            )
        }
    };
    let id = rpc.get("id").cloned().unwrap_or(Value::Null);
    let method = rpc.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "noyau", "version": ctx.state.read().unwrap().daemon_version},
        }),
        "tools/list" => json!({
            "tools": [
                {"name": "daemon_status", "description": "Etat du daemon noyau (stade, store, ancrage, watchdog).", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "boot_state", "description": "Etapes du boot courant et dernier recu.", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "boot_receipt_last", "description": "Dernier recu de boot emis (R10).", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "veille_status", "description": "Etat du watchdog N2 : stade du relais, spec, autorite, t2_ready_at, concordance.", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "veille_last_beat", "description": "Dernier battement ecrit a la trace par N2.", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "veille_observations", "description": "Verdicts par item W1-W7 du dernier cycle de veille.", "inputSchema": {"type": "object", "properties": {}}},
            ]
        }),
        "tools/call" => {
            let name = rpc
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            if MCP_TOOLS.contains(&name) {
                let value = mcp_tool_result(ctx, name);
                json!({"content": [{"type": "text", "text": value.to_string()}], "isError": false})
            } else {
                json!({"content": [{"type": "text", "text": format!("outil non enregistre : {name} — en Phase 1, seuls les outils de lecture du groupe daemon existent ; aucun outil mutant n'est enregistre")}], "isError": true})
            }
        }
        "ping" => json!({}),
        _ => {
            return (
                200,
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("methode inconnue : {method}")}}),
            )
        }
    };
    (200, json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

// ---------- boucle d'acceptation ----------

pub fn serve(listener: TcpListener, ctx: Arc<ServerCtx>, is_mcp: bool) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
                    if let Some(req) = read_request(&mut s) {
                        let (status, body) = if is_mcp {
                            handle_mcp(&ctx, &req)
                        } else {
                            handle_api(&ctx, &req)
                        };
                        respond(&mut s, status, &body);
                    }
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

// ---------- client minimal pour les auto-sondes (S6) ----------

pub fn http_request(bind: &str, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
    let mut stream = TcpStream::connect(bind).map_err(|e| format!("connexion {bind}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("timeout: {e}"))?;
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {bind}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("envoi: {e}"))?;
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|e| format!("lecture: {e}"))?;
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "reponse HTTP illisible".to_string())?;
    let body_text = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    let value: Value = serde_json::from_str(body_text).unwrap_or(Value::Null);
    Ok((status, value))
}
