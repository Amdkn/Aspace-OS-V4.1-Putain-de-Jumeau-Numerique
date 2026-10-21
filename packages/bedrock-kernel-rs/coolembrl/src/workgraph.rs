//! WorkGraph type d'execution (Phase 3 du Noyau A1, D-N039).
//!
//! Composant du daemon `noyau` (PRD Phase 3 du 13e, design signe de
//! Clara) : il tient l'etat d'execution des missions — le registre
//! Nardole reste le registre des missions ; les deux ne se concurrencent
//! pas (PRD section 0). Le modele est celui du PRD section 1, a liste de
//! champs fermee : aucun champ de dispatch n'existe ici (design 0.1).
//!
//! Regles portees par ce code :
//! - toute mutation passe par le point de controle unique de l'enveloppe
//!   (EnvelopeService::authorize) et par le store, qui refuse un token
//!   fence a l'ecriture (double mur, comme en Phase 2) ;
//! - `recorded_at` n'est jamais un champ d'entree (design 0.4) : le
//!   store le pose, l'antidatage est impossible par construction ;
//! - un effet tente non observe est ATTEMPTED_UNKNOWN ; il ne se resout
//!   QUE par une reconciliation a source typee (design 0.2) — timeout,
//!   hypothese, recit d'agent et succession de bail sont refuses comme
//!   modes de resolution (PRD section 3) ;
//! - le garde d'eligibilite vit a la transition, pas seulement a la
//!   liste (design 0.3) ;
//! - l'exactement-une-fois est un resultat de composition (idempotence
//!   + blocage UNKNOWN + reconciliation + fencing), jamais une propriete
//!   du journal seul (PRD section 4).

use crate::envelope::{EnvelopeService, Outcome};
use crate::store::Store;
use crate::timefmt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub const HOP_KINDS: [&str; 8] = [
    "picked",
    "worktree-created",
    "agent-running",
    "qa-evidence",
    "pr-open",
    "merged",
    "issue-closed",
    "refilled",
];

pub struct WorkGraph {
    pub store: Arc<Mutex<Store>>,
    pub envelope: Arc<EnvelopeService>,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
}

fn has_recorded_at(v: &Value) -> bool {
    // Controle SUPERFICIEL (cles du niveau requete + niveau evenement) :
    // les charges utiles d'assertion sont du JSON opaque — une cle
    // « recorded_at » dans une assertion est une donnee, pas un champ
    // d'entree de l'API (design 0.4 vise les champs de l'API).
    match v.get("event") {
        Some(ev) => {
            v.as_object().map(|m| m.contains_key("recorded_at")).unwrap_or(false)
                || ev.as_object().map(|m| m.contains_key("recorded_at")).unwrap_or(false)
        }
        None => v.as_object().map(|m| m.contains_key("recorded_at")).unwrap_or(false),
    }
}

impl WorkGraph {
    fn now(&self) -> i64 {
        timefmt::now_unix_secs()
    }

    /// Refus du WorkGraph. `store` = Some quand l'appelant tient deja le
    /// verrou du store (les gardes d'etat) : la variante a store tenu est
    /// utilisee — repasser par le verrou serait un interblocage.
    fn refusal(&self, store: Option<&Store>, status: u16, code: &str, message: &str, mission: Option<&str>, token: Option<i64>, detail: Value) -> Outcome {
        match store {
            Some(st) => self.envelope.refusal(st, status, code, message, mission, None, token, detail),
            None => self.envelope.traced_refusal(status, code, message, mission, None, token, detail),
        }
    }

    /// Le garde des dependances, a la transition (design 0.3) : dans la
    /// chaine des hops d'une mission, un predecesseur dont l'effet requis
    /// est ATTEMPTED_UNKNOWN bloque ; un predecesseur REFUTED bloque
    /// aussi (la mission attend sur la verite, pas sur un premisse
    /// refute). Renvoie le refus a produire, ou None si la voie est libre.
    fn dependency_guard(
        &self,
        store: &Store,
        mission: &str,
        hop_id: &str,
        token: i64,
    ) -> Option<Outcome> {
        let hops = store.wg_hops(mission);
        let pos = hops.iter().position(|h| h.hop_id == hop_id)?;
        for pred in &hops[..pos] {
            if pred.effect_state == "ATTEMPTED_UNKNOWN" {
                return Some(self.refusal(Some(store), 409, "DEPENDENCY_UNKNOWN",
                    "dependance en ATTEMPTED_UNKNOWN : le hop dependant n'est pas eligible tant que la reconciliation n'a pas tranche",
                    Some(mission), Some(token), json!({"blocked_by": pred.hop_id})));
            }
            if pred.effect_state == "REFUTED" {
                return Some(self.refusal(Some(store), 409, "DEPENDENCY_REFUTED",
                    "dependance REFUTED : l'effet requis n'a pas eu lieu — la mission se replanifie, elle ne s'appuie pas sur un premisse refute",
                    Some(mission), Some(token), json!({"blocked_by": pred.hop_id})));
            }
        }
        None
    }

    /// Une transition du WorkGraph : (etat, evenement, enveloppe) -> etat'.
    pub fn transition(&self, req: &Value) -> Outcome {
        let env = req.get("envelope").cloned().unwrap_or(Value::Null);
        let event = req.get("event").cloned().unwrap_or(Value::Null);
        let mission_hint = s(&env, "mission_id");
        // recorded_at n'est pas un champ d'entree (design 0.4)
        if has_recorded_at(req) {
            return self.refusal(None, 400, "RECORDED_AT_NOT_AN_INPUT",
                "recorded_at est pose par le store, jamais fourni par l'appelant — antidatage impossible par construction",
                mission_hint.as_deref(), None, json!({}));
        }
        let (Some(hop_id), Some(transition_key), Some(event_type)) = (
            s(&event, "hop_id"),
            s(&event, "transition_key"),
            s(&event, "type"),
        ) else {
            return self.refusal(None, 400, "WG_MALFORMED",
                "evenement malforme : hop_id, transition_key et type sont requis",
                mission_hint.as_deref(), None, json!({}));
        };
        if event_type == "create_hop" {
            match s(&event, "kind") {
                Some(k) if HOP_KINDS.contains(&k.as_str()) => {}
                other => {
                    return self.refusal(None, 400, "KIND_NOT_IN_ENUM",
                        "kind de hop hors de l'enumeration fermee du PRD (un nouveau kind = amendement de design signe)",
                        mission_hint.as_deref(), None, json!({"kind": other}));
                }
            }
        }
        // --- idempotence : une transition deja appliquee, redelivree,
        // est un no-op detecte, jamais un second effet (PRD section 2).
        {
            let store = self.store.lock().unwrap();
            if let Some(m) = &mission_hint {
                if let Some(id) = store.wg_transition_id(m, &hop_id, &transition_key) {
                    return Outcome {
                        status: 200,
                        body: json!({"applied": false, "noop": true, "transition_id": id,
                                      "reason": "transition deja appliquee — no-op detecte, aucun second effet"}),
                    };
                }
            }
        }
        // --- point de controle unique de l'enveloppe
        let auth = match self.envelope.authorize(&env, "workgraph.transition", &hop_id, 1, None) {
            Ok(a) => a,
            Err(outcome) => return outcome,
        };
        let mission = auth.mission.clone();
        let token = auth.token;
        let now = self.now();
        let now_iso = timefmt::iso8601_utc(now);
        let store = self.store.lock().unwrap();
        // --- gardes d'etat (apres autorisation, avant ecriture)
        match event_type.as_str() {
            "create_hop" => {
                if store.wg_get_hop(&mission, &hop_id).is_some() {
                    return self.refusal(Some(&store), 409, "HOP_EXISTS",
                        "hop deja cree sous cette mission (une recreation passe par une nouvelle mission ou un nouveau hop_id)",
                        Some(&mission), Some(token), json!({"hop_id": hop_id}));
                }
            }
            "set_status" | "effect_attempt" => {
                if store.wg_get_hop(&mission, &hop_id).is_none() {
                    return self.refusal(Some(&store), 404, "HOP_UNKNOWN",
                        "hop inconnu sous cette mission", Some(&mission), Some(token),
                        json!({"hop_id": hop_id}));
                }
                if let Some(r) = self.dependency_guard(&store, &mission, &hop_id, token) {
                    return r;
                }
                if event_type == "effect_attempt" {
                    let hop = store.wg_get_hop(&mission, &hop_id).unwrap();
                    match hop.effect_state.as_str() {
                        "NOT_ATTEMPTED" => {}
                        "ATTEMPTED_UNKNOWN" => {
                            return self.refusal(Some(&store), 409, "EFFECT_ALREADY_UNKNOWN",
                                "effet deja tente et non observe : pas de retry aveugle — la reconciliation tranche, elle seule",
                                Some(&mission), Some(token), json!({"hop_id": hop_id}));
                        }
                        other => {
                            return self.refusal(Some(&store), 409, "EFFECT_ALREADY_RESOLVED",
                                "effet deja tranche par reconciliation — aucun second effet",
                                Some(&mission), Some(token), json!({"hop_id": hop_id, "effect_state": other}));
                        }
                    }
                }
            }
            "envelope_update" => {
                if store.wg_get_mission(&mission).is_none() {
                    return self.refusal(Some(&store), 404, "WG_MISSION_UNKNOWN",
                        "mission inconnue du WorkGraph", Some(&mission), Some(token), json!({}));
                }
            }
            other => {
                return self.refusal(Some(&store), 400, "WG_EVENT_UNKNOWN",
                    "type d'evenement inconnu du WorkGraph type",
                    Some(&mission), Some(token), json!({"type": other}));
            }
        }
        // --- ecriture, en transaction : la transition (mur du store sur
        // le token) et le changement d'etat sont atomiques.
        let applied = (|| -> Result<i64, String> {
            store.conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
            let tid = store.wg_insert_transition(
                &mission, &hop_id, &transition_key, &event_type, token, auth.trace_id,
                &event.to_string(), now, &now_iso,
            )?;
            match event_type.as_str() {
                "create_hop" => {
                    let work_id = s(&auth.stored_envelope, "work_id").unwrap_or_default();
                    store.wg_upsert_mission(&mission, &work_id, s(&env, "evidence_ref").as_deref(), &now_iso)?;
                    let seq = store.wg_hops(&mission).len() as i64;
                    let assertion = event.get("assertion").map(|a| a.to_string());
                    let holder = auth.stored_envelope.pointer("/lease/holder").and_then(|h| h.as_str()).unwrap_or("").to_string();
                    store.wg_insert_hop(
                        &mission, &hop_id, seq,
                        &s(&event, "kind").unwrap(),
                        s(&event, "status").as_deref().unwrap_or("recorded"),
                        &holder,
                        token,
                        assertion.as_deref(),
                        &now_iso,
                    )?;
                }
                "set_status" => {
                    let hop = store.wg_get_hop(&mission, &hop_id).unwrap();
                    let holder = auth.stored_envelope.pointer("/lease/holder").and_then(|h| h.as_str()).unwrap_or("").to_string();
                    store.wg_update_hop(
                        &mission, &hop_id,
                        s(&event, "status").as_deref().unwrap_or(&hop.status),
                        &hop.effect_state,
                        hop.effect_attempted_unix,
                        &holder, token,
                        &hop.evidence_refs_json,
                        &now_iso,
                    )?;
                }
                "effect_attempt" => {
                    let hop = store.wg_get_hop(&mission, &hop_id).unwrap();
                    let observed = event.get("observed").and_then(|o| o.as_bool()).unwrap_or(false);
                    let mut refs: Vec<Value> = serde_json::from_str(&hop.evidence_refs_json).unwrap_or_default();
                    refs.push(json!(format!("trace:{}", auth.trace_id)));
                    let holder = auth.stored_envelope.pointer("/lease/holder").and_then(|h| h.as_str()).unwrap_or("").to_string();
                    store.wg_update_hop(
                        &mission, &hop_id, &hop.status,
                        if observed { "CONFIRMED" } else { "ATTEMPTED_UNKNOWN" },
                        Some(now), &holder, token,
                        &serde_json::to_string(&refs).unwrap(),
                        &now_iso,
                    )?;
                }
                "envelope_update" => {
                    // La succession se prouve dans le graphe (design 0.7) :
                    // la transition est presentee sous le bail successeur
                    // (authorize l'a verifie) et l'envelope_ref devient le
                    // sien.
                    store.wg_set_mission_envelope(
                        &mission,
                        s(&env, "evidence_ref").as_deref().unwrap_or_default(),
                        &now_iso,
                    )?;
                }
                _ => unreachable!(),
            }
            store.conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(tid)
        })();
        match applied {
            Ok(tid) => Outcome {
                status: 200,
                body: json!({"applied": true, "transition_id": tid, "trace_id": auth.trace_id,
                             "fencing_token": token, "mission_id": mission, "hop_id": hop_id,
                             "event_type": event_type}),
            },
            Err(e) => {
                let _ = store.conn.execute_batch("ROLLBACK");
                Outcome {
                    status: 500,
                    body: json!({"error": {"code": "WG_STORE_REFUSED", "type": "system_fault", "message": e, "trace_id": auth.trace_id}}),
                }
            }
        }
    }

    /// L'acte qui resout un UNKNOWN — lui-meme une mutation sous
    /// enveloppe, avec sa preuve (PRD section 3).
    pub fn reconcile(&self, req: &Value) -> Outcome {
        let env = req.get("envelope").cloned().unwrap_or(Value::Null);
        let mission_hint = s(&env, "mission_id");
        if has_recorded_at(req) {
            return self.refusal(None, 400, "RECORDED_AT_NOT_AN_INPUT",
                "recorded_at est pose par le store, jamais fourni par l'appelant",
                mission_hint.as_deref(), None, json!({}));
        }
        // --- la reconciliation a une FORME (design 0.2) : source_read est
        // une reference typee {systeme, localisateur, valeur observee} ;
        // un texte libre (recit) est invalide par la forme.
        let sr = req.get("source_read").cloned().unwrap_or(Value::Null);
        let (Some(system), Some(locator), Some(observed)) = (
            s(&sr, "system"),
            s(&sr, "locator"),
            s(&sr, "observed_value"),
        ) else {
            return self.refusal(None, 400, "RECONCILIATION_FORM",
                "source_read doit etre une reference typee {system, locator, observed_value} — un recit n'est pas une source",
                mission_hint.as_deref(), None, json!({}));
        };
        let (Some(hop_id), Some(resolved_to), Some(read_at)) = (
            s(req, "hop_id"),
            s(req, "resolved_to").filter(|r| r == "CONFIRMED" || r == "REFUTED"),
            s(req, "read_at"),
        ) else {
            return self.refusal(None, 400, "WG_MALFORMED",
                "reconciliation malformee : hop_id, resolved_to (CONFIRMED|REFUTED) et read_at sont requis",
                mission_hint.as_deref(), None, json!({}));
        };
        let auth = match self.envelope.authorize(&env, "workgraph.reconcile", &hop_id, 1, None) {
            Ok(a) => a,
            Err(outcome) => return outcome,
        };
        let mission = auth.mission.clone();
        let token = auth.token;
        let now_iso = timefmt::iso8601_utc(self.now());
        let store = self.store.lock().unwrap();
        let Some(hop) = store.wg_get_hop(&mission, &hop_id) else {
            return self.refusal(Some(&store), 404, "HOP_UNKNOWN",
                "hop inconnu sous cette mission", Some(&mission), Some(token),
                json!({"hop_id": hop_id}));
        };
        if hop.effect_state != "ATTEMPTED_UNKNOWN" {
            return self.refusal(Some(&store), 409, "NOTHING_TO_RECONCILE",
                "seul un effet ATTEMPTED_UNKNOWN se reconcilie",
                Some(&mission), Some(token),
                json!({"hop_id": hop_id, "effect_state": hop.effect_state}));
        }
        let evidence_ref = format!("trace:{}", auth.trace_id);
        let done = (|| -> Result<i64, String> {
            store.conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
            let rid = store.wg_insert_reconciliation(
                &mission, &hop_id, &resolved_to, &system, &locator, &observed,
                &read_at, &now_iso, &evidence_ref,
            )?;
            let mut refs: Vec<Value> = serde_json::from_str(&hop.evidence_refs_json).unwrap_or_default();
            refs.push(json!(evidence_ref.clone()));
            let holder = auth.stored_envelope.pointer("/lease/holder").and_then(|h| h.as_str()).unwrap_or("").to_string();
            store.wg_update_hop(
                &mission, &hop_id, &hop.status, &resolved_to,
                hop.effect_attempted_unix, &holder, token,
                &serde_json::to_string(&refs).unwrap(), &now_iso,
            )?;
            store.wg_set_mission_observed(&mission, &read_at, &now_iso)?;
            store.conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(rid)
        })();
        match done {
            Ok(rid) => Outcome {
                status: 200,
                body: json!({"reconciled": true, "reconciliation_id": rid, "mission_id": mission,
                             "hop_id": hop_id, "resolved_to": resolved_to, "trace_id": auth.trace_id,
                             "evidence_ref": evidence_ref}),
            },
            Err(e) => {
                let _ = store.conn.execute_batch("ROLLBACK");
                Outcome { status: 500, body: json!({"error": {"code": "WG_STORE_REFUSED", "type": "system_fault", "message": e, "trace_id": auth.trace_id}}) }
            }
        }
    }

    // ---------- surface d'interrogation (PRD section 5, lecture seule) ----------

    pub fn mission(&self, mission_id: &str) -> Outcome {
        let store = self.store.lock().unwrap();
        let Some((work_id, envelope_ref, observed_at, recorded_at)) = store.wg_get_mission(mission_id) else {
            return Outcome { status: 404, body: json!({"error": {"code": "WG_MISSION_UNKNOWN", "message": "mission inconnue du WorkGraph"}}) };
        };
        let hops: Vec<Value> = store.wg_hops(mission_id).iter().map(|h| {
            let refs: Value = serde_json::from_str(&h.evidence_refs_json).unwrap_or(json!([]));
            json!({
                "hop_id": h.hop_id,
                "kind": h.kind,
                "status": h.status,
                "lease_holder": h.lease_holder,
                "fencing_token": h.fencing_token,
                "evidence_refs": refs,
                "effect": h.effect_state,
            })
        }).collect();
        Outcome {
            status: 200,
            body: json!({
                "work_id": work_id,
                "mission_id": mission_id,
                "envelope_ref": envelope_ref,
                "hops": hops,
                "observed_at": observed_at,
                "recorded_at": recorded_at,
            }),
        }
    }

    /// Charge utile d'assertion d'un hop, relue a l'identique (le store
    /// respecte le contrat #64 par construction : JSON opaque, aucun
    /// rejet de type a ce niveau — PRD section 2).
    pub fn assertion(&self, mission_id: &str, hop_id: &str) -> Outcome {
        let store = self.store.lock().unwrap();
        match store.wg_get_hop(mission_id, hop_id) {
            None => Outcome { status: 404, body: json!({"error": {"code": "HOP_UNKNOWN", "message": "hop inconnu"}}) },
            Some(h) => Outcome {
                status: 200,
                body: json!({
                    "found": h.assertion_json.is_some(),
                    "assertion": h.assertion_json.and_then(|a| serde_json::from_str::<Value>(&a).ok()),
                }),
            },
        }
    }

    pub fn eligible(&self, mission_id: &str) -> Outcome {
        let store = self.store.lock().unwrap();
        if store.wg_get_mission(mission_id).is_none() {
            return Outcome { status: 404, body: json!({"error": {"code": "WG_MISSION_UNKNOWN", "message": "mission inconnue du WorkGraph"}}) };
        }
        let hops = store.wg_hops(mission_id);
        let mut eligible = Vec::new();
        let mut prefix_confirmed = true;
        for h in &hops {
            if prefix_confirmed && h.effect_state != "CONFIRMED" {
                eligible.push(h.hop_id.clone());
            }
            if h.effect_state != "CONFIRMED" {
                prefix_confirmed = false;
            }
        }
        let token = store.current_token(mission_id);
        let lease = if token > 0 {
            store.get_lease(mission_id, token).map(|(holder, state, _, expires, _)| {
                json!({"holder": holder, "fencing_token": token, "expires_at": timefmt::iso8601_utc(expires),
                       "live": state == "live" && expires > self.now()})
            })
        } else {
            None
        };
        Outcome { status: 200, body: json!({"mission_id": mission_id, "eligible": eligible, "current_lease": lease}) }
    }

    pub fn lease_holder(&self, mission_id: &str) -> Outcome {
        let store = self.store.lock().unwrap();
        let token = store.current_token(mission_id);
        if token == 0 {
            return Outcome { status: 200, body: json!({"found": false, "mission_id": mission_id}) };
        }
        match store.get_lease(mission_id, token) {
            Some((holder, state, issued, expires, _)) => Outcome {
                status: 200,
                body: json!({"found": true, "mission_id": mission_id, "holder": holder,
                             "fencing_token": token, "issued_at": timefmt::iso8601_utc(issued),
                             "expires_at": timefmt::iso8601_utc(expires),
                             "live": state == "live" && expires > self.now()}),
            },
            None => Outcome { status: 200, body: json!({"found": false, "mission_id": mission_id}) },
        }
    }

    /// La file des ATTEMPTED_UNKNOWN en attente, avec leur age — surface
    /// d'observation : aucun seuil, aucune notification (design 0.5 ;
    /// l'exploitation appartient a la Phase 4).
    pub fn pending_reconciliations(&self) -> Outcome {
        let store = self.store.lock().unwrap();
        let now = self.now();
        let items: Vec<Value> = store.wg_pending_unknown().iter().map(|(m, h, attempted)| {
            json!({
                "mission_id": m,
                "hop_id": h,
                "effect_attempted_at": attempted.map(timefmt::iso8601_utc),
                "age_secs": attempted.map(|a| now - a),
            })
        }).collect();
        Outcome { status: 200, body: json!({"pending": items, "count": items.len()}) }
    }
}
