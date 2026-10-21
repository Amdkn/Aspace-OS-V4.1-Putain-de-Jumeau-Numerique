//! Service d'enveloppe d'autorite runtime (Phase 2 du Noyau A1).
//!
//! Le service est celui de `noyau` (design section 0.1) : point de controle
//! unique a la frontiere du daemon (design section 0.5). Il emet les baux
//! (autorite unique d'emission des fencing tokens, strictement croissants
//! par mission — PRD 2.2), verifie au point d'effet (bail vivant, fencing
//! courant, budget par effet, politique L05 lue au moment de la decision),
//! et produit sa trace AVANT l'effet comme avant le refus (design 0.2).
//! La politique est lue au store L05 et epinglee en version dans
//! l'enveloppe (policy_ref) ; ce service ne contient aucune regle en propre.

use crate::policy;
use crate::store::Store;
use crate::timefmt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct EnvelopeService {
    pub store: Arc<Mutex<Store>>,
    pub policy_dir: Option<PathBuf>,
}

pub struct Outcome {
    pub status: u16,
    pub body: Value,
}

/// Presentation autorisee par le point de controle unique : le bail
/// verifie, sa trace d'autorisation, et l'enveloppe telle qu'emise.
pub struct Authorized {
    pub mission: String,
    pub operation: String,
    pub token: i64,
    pub trace_id: i64,
    pub stored_envelope: Value,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
}

fn i(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64())
}

impl EnvelopeService {
    fn now(&self) -> i64 {
        timefmt::now_unix_secs()
    }

    /// Refus type, TRACE AVANT le retour (H-P2-9) : chaque refus porte
    /// l'identifiant de sa trace ; un refus sans trace est une violation.
    /// Variante a store deja tenu par l'appelant (le WorkGraph tient le
    /// verrou pendant ses gardes) ; traced_refusal() est la variante qui
    /// prend le verrou.
    pub(crate) fn refusal(
        &self,
        store: &Store,
        status: u16,
        code: &str,
        message: &str,
        mission: Option<&str>,
        operation: Option<&str>,
        token: Option<i64>,
        detail: Value,
    ) -> Outcome {
        let trace_id = store.trace(
            "REFUSE",
            mission,
            operation,
            token,
            &json!({"code": code, "message": message, "detail": detail}).to_string(),
            self.now(),
        );
        match trace_id {
            Ok(id) => {
                let mut error = json!({
                    "code": code,
                    "type": "typed_refusal",
                    "message": message,
                    "trace_id": id,
                    "evidence_ref": format!("trace:{id}"),
                });
                // La version de politique lue est epinglee aussi dans la
                // reponse : la decision est reproductible sans ouvrir le store.
                if let Some(pr) = detail.get("policy_ref_read") {
                    error["policy_ref_read"] = pr.clone();
                }
                Outcome {
                    status,
                    body: json!({"error": error}),
                }
            }
            Err(e) => Outcome {
                status: 500,
                body: json!({
                    "error": {
                        "code": "TRACE_WRITE_FAILED",
                        "type": "system_fault",
                        "message": format!("refus {code} non trace — fail-closed sur la preuve : {e}"),
                    }
                }),
            },
        }
    }

    fn load_policy(&self) -> Result<policy::PolicySnapshot, String> {
        match &self.policy_dir {
            None => Err("policy store non configure (NOYAU_POLICY_DIR absent)".to_string()),
            Some(dir) => policy::load_policy_store(dir),
        }
    }

    /// Emission d'un bail (mapping CLAIMED -> Lease, PRD section 2).
    pub fn claim(&self, req: &Value) -> Outcome {
        let store = self.store.lock().unwrap();
        let mission_hint = s(req, "mission_id");
        let operation_hint = s(req, "operation_id");
        // --- validation de forme, AVANT tout controle de bail (design 0.4)
        let fields = ["work_id", "mission_id", "operation_id", "correlation_id", "holder", "return_to"];
        for f in fields {
            if s(req, f).is_none() {
                return self.refusal(&store, 400, "ENVELOPE_MALFORMED",
                    &format!("champ requis absent ou vide : {f}"),
                    mission_hint.as_deref(), operation_hint.as_deref(), None, json!({}));
            }
        }
        let mission = s(req, "mission_id").unwrap();
        let operation = s(req, "operation_id").unwrap();
        let ttl = i(req, "ttl_s").filter(|t| *t > 0);
        if ttl.is_none() {
            return self.refusal(&store, 400, "ENVELOPE_MALFORMED",
                "ttl_s requis (entier > 0)", Some(&mission), Some(&operation), None, json!({}));
        }
        let ttl = ttl.unwrap();
        let caps = req.pointer("/capabilities/permanent").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        if caps.iter().any(|c| !c.is_string()) {
            return self.refusal(&store, 400, "ENVELOPE_MALFORMED",
                "capabilities.permanent doit etre une liste de chaines",
                Some(&mission), Some(&operation), None, json!({}));
        }
        let promos_in = req.pointer("/capabilities/promotions").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        for p in &promos_in {
            if s(p, "capability").is_none() || s(p, "granted_by").is_none() || i(p, "expires_at_unix").is_none() {
                return self.refusal(&store, 400, "ENVELOPE_MALFORMED",
                    "promotion incomplete : capability, granted_by et expires_at_unix sont requis",
                    Some(&mission), Some(&operation), None, json!({}));
            }
        }
        let budget_caps = req.pointer("/budget/per_effect_caps").and_then(|c| c.as_object()).cloned().unwrap_or_default();
        if budget_caps.values().any(|v| !v.is_i64() || v.as_i64().unwrap() < 0) {
            return self.refusal(&store, 400, "ENVELOPE_MALFORMED",
                "budget.per_effect_caps doit associer chaque classe a un plafond entier >= 0",
                Some(&mission), Some(&operation), None, json!({}));
        }
        // --- politique lue au moment de la decision (design 0.3)
        let snapshot = match self.load_policy() {
            Ok(snap) if snap.in_force() => snap,
            Ok(snap) => {
                return self.refusal(&store, 403, "POLICY_NOT_IN_FORCE",
                    "le policy store lu n'est pas en vigueur (une politique inscrite ne porte pas le jeton de vigueur du registre)",
                    Some(&mission), Some(&operation), None,
                    json!({"policy_ref_read": snap.policy_ref()}));
            }
            Err(e) => {
                return self.refusal(&store, 403, "POLICY_NOT_IN_FORCE",
                    &format!("policy store illisible : {e}"),
                    Some(&mission), Some(&operation), None, json!({}));
            }
        };
        let policy_ref = snapshot.policy_ref();
        // --- un seul bail vivant par portee de mission (PRD section 5)
        let now = self.now();
        let current = store.current_token(&mission);
        let successor = s(req, "mode").as_deref() == Some("successor");
        if current > 0 {
            if let Some((_, state, _, expires, _)) = store.get_lease(&mission, current) {
                if state == "live" && expires > now && !successor {
                    return self.refusal(&store, 409, "LEASE_HELD",
                        "un bail vivant existe deja pour cette mission ; la reprise passe par un bail successeur (fencing)",
                        Some(&mission), Some(&operation), Some(current), json!({}));
                }
            }
        }
        // --- emission : token strictement croissant, autorite unique
        let token = current + 1;
        let issued = now;
        let expires = now + ttl;
        let promotions: Vec<Value> = promos_in.iter().map(|p| json!({
            "capability": s(p, "capability").unwrap(),
            "granted_by": s(p, "granted_by").unwrap(),
            "granted_at_unix": issued,
            "expires_at_unix": i(p, "expires_at_unix").unwrap(),
        })).collect();
        let mut envelope = json!({
            "work_id": s(req, "work_id").unwrap(),
            "mission_id": mission,
            "operation_id": operation,
            "correlation_id": s(req, "correlation_id").unwrap(),
            "lease": {
                "holder": s(req, "holder").unwrap(),
                "fencing_token": token,
                "issued_at_unix": issued,
                "issued_at": timefmt::iso8601_utc(issued),
                "expires_at_unix": expires,
                "expires_at": timefmt::iso8601_utc(expires),
            },
            "budget": {"per_effect_caps": Value::Object(budget_caps)},
            "capabilities": {"permanent": caps, "promotions": promotions},
            "policy_ref": policy_ref,
            "evidence_ref": Value::Null,
            "return_to": s(req, "return_to").unwrap(),
        });
        if let Err(e) = store.insert_lease(&mission, token, &s(req, "holder").unwrap(), issued, expires, &envelope.to_string()) {
            return self.refusal(&store, 500, "STORE_ERROR", &e,
                Some(&mission), Some(&operation), Some(token), json!({}));
        }
        if successor && current > 0 {
            let _ = store.set_lease_state(&mission, current, "fenced");
        }
        // --- trace d'emission AVANT que l'enveloppe soit utilisable (0.2)
        let trace_id = match store.trace("ISSUE", Some(&mission), Some(&operation), Some(token),
            &json!({"holder": s(req, "holder").unwrap(), "policy_ref": envelope["policy_ref"], "mode": if successor { "successor" } else { "initial" }}).to_string(), now) {
            Ok(id) => id,
            Err(e) => {
                let _ = store.set_lease_state(&mission, token, "void");
                return self.refusal(&store, 500, "TRACE_WRITE_FAILED",
                    &format!("emission non tracee — bail annule (fail-closed sur la preuve) : {e}"),
                    Some(&mission), Some(&operation), Some(token), json!({}));
            }
        };
        envelope["evidence_ref"] = json!(format!("trace:{trace_id}"));
        // Reconsigner l'enveloppe finale au bail (evidence_ref desormais pose).
        let _ = store.update_lease_envelope(&mission, token, &envelope.to_string());
        Outcome {
            status: 200,
            body: json!({"envelope": envelope, "trace_id": trace_id}),
        }
    }

    /// Refus type et trace, pour les consommateurs du point de controle
    /// (WorkGraph, Phase 3) : meme discipline que les refus du service —
    /// aucun refus ne part sans sa trace.
    pub fn traced_refusal(&self, status: u16, code: &str, message: &str, mission: Option<&str>, operation: Option<&str>, token: Option<i64>, detail: Value) -> Outcome {
        let store = self.store.lock().unwrap();
        self.refusal(&store, status, code, message, mission, operation, token, detail)
    }

    /// Verification complete d'une presentation de mutation et trace
    /// d'autorisation AVANT l'effet : forme de l'enveloppe, bail emis et
    /// conforme, fencing courant, expiration au point d'effet, politique
    /// relue, capacite, budget. C'est LE point de controle unique
    /// (design Phase 2, 0.5) : /envelope/effect et le WorkGraph (Phase 3)
    /// passent par ici, aucun autre chemin n'autorise.
    pub fn authorize(&self, env: &Value, effect_class: &str, head_sha: &str, cost: i64, required_capability: Option<&str>) -> Result<Authorized, Outcome> {
        let store = self.store.lock().unwrap();
        let mission_hint = s(env, "mission_id");
        let operation_hint = s(env, "operation_id");
        let token_hint = env.pointer("/lease/fencing_token").and_then(|t| t.as_i64());
        let form_ok = ["work_id", "mission_id", "operation_id", "correlation_id", "return_to"]
            .iter().all(|f| s(env, f).is_some())
            && env.pointer("/lease/holder").and_then(|h| h.as_str()).is_some()
            && token_hint.is_some()
            && env.pointer("/lease/issued_at_unix").and_then(|x| x.as_i64()).is_some()
            && env.pointer("/lease/expires_at_unix").and_then(|x| x.as_i64()).is_some();
        if !form_ok {
            return Err(self.refusal(&store, 400, "ENVELOPE_MALFORMED",
                "enveloppe malforme : identifiants, return_to et bail complet sont requis",
                mission_hint.as_deref(), operation_hint.as_deref(), token_hint, json!({})));
        }
        let mission = mission_hint.unwrap();
        let operation = operation_hint.unwrap();
        let token = token_hint.unwrap();
        let now = self.now();
        // --- le bail doit avoir ete emis par ce service
        let Some((holder, state, _issued, expires, stored_env_json)) = store.get_lease(&mission, token) else {
            return Err(self.refusal(&store, 403, "LEASE_UNKNOWN",
                "aucun bail emis sous cette mission et ce token — un claim sans bail ne produit aucun effet (claim orphelin)",
                Some(&mission), Some(&operation), Some(token), json!({})));
        };
        // --- l'enveloppe presentee doit etre celle qui a ete emise
        let stored_env: Value = serde_json::from_str(&stored_env_json).unwrap_or(Value::Null);
        let subset_ok = s(&stored_env, "work_id") == s(&env, "work_id")
            && s(&stored_env, "correlation_id") == s(&env, "correlation_id")
            && stored_env.pointer("/lease/holder").and_then(|h| h.as_str()) == env.pointer("/lease/holder").and_then(|h| h.as_str())
            && stored_env.pointer("/lease/holder").and_then(|h| h.as_str()) == Some(holder.as_str());
        if !subset_ok {
            return Err(self.refusal(&store, 403, "ENVELOPE_MISMATCH",
                "l'enveloppe presentee n'est pas celle qui a ete emise sous ce token",
                Some(&mission), Some(&operation), Some(token), json!({})));
        }
        // --- fencing : le token presente doit etre le token courant
        let current = store.current_token(&mission);
        if state == "fenced" || token != current {
            return Err(self.refusal(&store, 403, "FENCED",
                "bail fence : un bail successeur a token superieur gouverne cette mission ; l'ancien holder ne peut plus agir",
                Some(&mission), Some(&operation), Some(token), json!({"current_token": current})));
        }
        // --- expiration du bail, au point d'effet
        if now > expires {
            let _ = store.set_lease_state(&mission, token, "expired");
            return Err(self.refusal(&store, 403, "LEASE_EXPIRED",
                "bail perime refuse au point d'effet — aucun effet produit",
                Some(&mission), Some(&operation), Some(token),
                json!({"expires_at": timefmt::iso8601_utc(expires)})));
        }
        // --- politique relue au moment de la decision
        match self.load_policy() {
            Ok(snap) if snap.in_force() => {}
            Ok(snap) => {
                return Err(self.refusal(&store, 403, "POLICY_NOT_IN_FORCE",
                    "le policy store relu n'est pas en vigueur",
                    Some(&mission), Some(&operation), Some(token),
                    json!({"policy_ref_read": snap.policy_ref()})));
            }
            Err(e) => {
                return Err(self.refusal(&store, 403, "POLICY_NOT_IN_FORCE",
                    &format!("policy store illisible : {e}"),
                    Some(&mission), Some(&operation), Some(token), json!({})));
            }
        }
        // --- capacite requise : permanente, ou promotion vivante
        if let Some(required) = required_capability {
            let permanent = stored_env.pointer("/capabilities/permanent").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            if !permanent.iter().any(|c| c.as_str() == Some(required)) {
                let promos = stored_env.pointer("/capabilities/promotions").and_then(|c| c.as_array()).cloned().unwrap_or_default();
                match promos.iter().find(|p| s(p, "capability").as_deref() == Some(required)) {
                    Some(p) if i(p, "expires_at_unix").map(|e| e > now).unwrap_or(false) => {}
                    Some(p) => {
                        return Err(self.refusal(&store, 403, "PROMOTION_EXPIRED",
                            "promotion expiree effectivement retiree : la capacite n'est plus accordee",
                            Some(&mission), Some(&operation), Some(token),
                            json!({"capability": required, "promotion_expires_at_unix": i(p, "expires_at_unix")})));
                    }
                    None => {
                        return Err(self.refusal(&store, 403, "CAPABILITY_ABSENT",
                            "capacite requise absente de l'enveloppe (ni permanente, ni promotion)",
                            Some(&mission), Some(&operation), Some(token),
                            json!({"capability": required})));
                    }
                }
            }
        }
        // --- budget par effet (PRD section 5) ; la classe paid_fallback est
        // plafonnee a 0 en Phase 2 par mecanique (design 0.6 : le garde
        // « pas de fallback payant » devient un plafond, pas une consigne).
        let caps = stored_env.pointer("/budget/per_effect_caps").and_then(|c| c.as_object()).cloned().unwrap_or_default();
        let cap = if effect_class == "paid_fallback" {
            0
        } else {
            caps.get(effect_class).and_then(|v| v.as_i64()).unwrap_or(0)
        };
        let spent = store.spent_for(&mission, token, &effect_class);
        if spent + cost > cap {
            return Err(self.refusal(&store, 403, "BUDGET_EXCEEDED",
                "plafond par effet depasse pour cette classe — refus consigne",
                Some(&mission), Some(&operation), Some(token),
                json!({"effect_class": effect_class, "cap": cap, "spent": spent, "cost": cost})));
        }
        // --- trace d'autorisation AVANT l'effet (design 0.2)
        let trace_id = match store.trace("AUTHORIZE", Some(&mission), Some(&operation), Some(token),
            &json!({"effect_class": effect_class, "head_sha": head_sha, "cost": cost}).to_string(), now) {
            Ok(id) => id,
            Err(e) => {
                return Err(Outcome { status: 500, body: json!({"error": {"code": "TRACE_WRITE_FAILED", "type": "system_fault", "message": format!("autorisation non tracee — effet refuse (fail-closed sur la preuve) : {e}")}}) });
            }
        };
        Ok(Authorized {
            mission,
            operation,
            token,
            trace_id,
            stored_envelope: stored_env,
        })
    }

    /// Presentation d'une mutation au point d'effet : verification par
    /// authorize() (point de controle unique), liaison de preuve, puis
    /// l'effet porte au store — qui refuse lui-meme un token fence entre
    /// l'autorisation et l'ecriture (couplage ecrit des deux cotes).
    pub fn effect(&self, req: &Value) -> Outcome {
        let env = req.get("envelope").cloned().unwrap_or(Value::Null);
        let eff = req.get("effect").cloned().unwrap_or(Value::Null);
        if s(&eff, "effect_class").is_none() || s(&eff, "head_sha").is_none() {
            return self.traced_refusal(400, "ENVELOPE_MALFORMED",
                "effet malforme : effect_class et head_sha sont requis",
                s(&env, "mission_id").as_deref(), s(&env, "operation_id").as_deref(),
                env.pointer("/lease/fencing_token").and_then(|t| t.as_i64()), json!({}));
        }
        let effect_class = s(&eff, "effect_class").unwrap();
        let head_sha = s(&eff, "head_sha").unwrap();
        let cost = i(&eff, "cost").unwrap_or(1).max(1);
        let required = s(&eff, "required_capability");
        let auth = match self.authorize(&env, &effect_class, &head_sha, cost, required.as_deref()) {
            Ok(a) => a,
            Err(outcome) => return outcome,
        };
        let mission = auth.mission.clone();
        let operation = auth.operation.clone();
        let token = auth.token;
        let trace_id = auth.trace_id;
        let now = self.now();
        let store = self.store.lock().unwrap();
        // --- liaison de preuve : une nouvelle tete rend l'ancienne preuve
        // obsolete — jamais portee en silence (PRD 3.1, H-P2-7).
        for (eff_id, old_head, proof_state, _) in store.effects_for(&mission, &operation) {
            if proof_state == "CURRENT" {
                if let Some(old) = &old_head {
                    if *old != head_sha {
                        let _ = store.mark_effect_obsolete(eff_id);
                        let _ = store.trace("OBSOLETE", Some(&mission), Some(&operation), Some(token),
                            &json!({"effect_id": eff_id, "superseded_head": old, "new_head": head_sha}).to_string(), now);
                    }
                }
            }
        }
        // --- l'effet, porte au store (transition estampillee du token)
        match store.insert_effect(&mission, &operation, s(&env, "correlation_id").as_deref(),
            &effect_class, Some(&head_sha), cost, token, trace_id, now) {
            Ok(effect_id) => Outcome {
                status: 200,
                body: json!({
                    "authorized": true,
                    "effect_id": effect_id,
                    "trace_id": trace_id,
                    "evidence_ref": format!("trace:{trace_id}"),
                    "head_sha": head_sha,
                    "proof_state": "CURRENT",
                    "fencing_token": token,
                }),
            },
            Err(e) => Outcome {
                status: 500,
                body: json!({"error": {"code": "EFFECT_STORE_REFUSED", "type": "system_fault", "message": e, "trace_id": trace_id}}),
            },
        }
    }

    /// Preuve d'une (mission, operation) : tete courante et tetes obsoletes.
    pub fn proof(&self, mission: &str, operation: &str) -> Outcome {
        let store = self.store.lock().unwrap();
        let effects = store.effects_for(mission, operation);
        if effects.is_empty() {
            return Outcome { status: 200, body: json!({"found": false, "mission_id": mission, "operation_id": operation}) };
        }
        let (id, head, state, trace_id) = &effects[0];
        let obsolete: Vec<Value> = effects.iter().skip(1)
            .filter(|(_, _, st, _)| st == "OBSOLETE")
            .map(|(i, h, _, t)| json!({"effect_id": i, "head_sha": h, "trace_id": t}))
            .collect();
        Outcome {
            status: 200,
            body: json!({
                "found": true,
                "mission_id": mission,
                "operation_id": operation,
                "effect_id": id,
                "head_sha": head,
                "proof_state": state,
                "trace_id": trace_id,
                "evidence_ref": format!("trace:{trace_id}"),
                "obsolete": obsolete,
            }),
        }
    }

    pub fn state_json(&self) -> Value {
        let store = self.store.lock().unwrap();
        let now = self.now();
        json!({
            "mechanism": store.anchor_mechanism(),
            "live_leases": store.conn.query_row(
                "SELECT COUNT(*) FROM leases WHERE state='live' AND expires_unix > ?1",
                [now], |r| r.get::<_, i64>(0)).unwrap_or(0),
            "capabilities_granted": store.live_granted_count(now),
            "policy_store": match self.load_policy() {
                Ok(snap) => json!({"loaded": true, "policy_ref": snap.policy_ref(), "in_force": snap.in_force()}),
                Err(e) => json!({"loaded": false, "error": e}),
            },
        })
    }
}
