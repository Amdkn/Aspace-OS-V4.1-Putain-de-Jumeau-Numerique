//! Store durable du daemon : SQLite en mode WAL sur les volumes du daemon
//! (voie qualifiee en Phase 0 du programme — ce module la consomme, il ne la
//! re-choisit pas). La Phase 1 ouvre le store et rejoue ce qu'il contient ;
//! le WorkGraph type qui definira les missions est la Phase 3.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub const STORE_SCHEMA_VERSION: i64 = 2;

pub struct Store {
    pub conn: Connection,
}

pub struct ReplayReport {
    pub hops_replayed: i64,
    pub effects_unknown_pending: i64,
}

impl Store {
    /// Ouvre (ou cree) le store, applique le schema, verifie l'integrite.
    /// Retourne Err(detail) si le controle d'integrite echoue.
    pub fn open(path: &Path) -> Result<Store, String> {
        let conn = Connection::open(path).map_err(|e| format!("ouverture du store: {e}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("activation WAL: {e}"))?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(|e| format!("synchronous FULL: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS boots (
                 boot_id TEXT PRIMARY KEY,
                 receipt_json TEXT NOT NULL,
                 recorded_unix INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS state_entries (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind TEXT NOT NULL,
                 correlation_id TEXT,
                 effect_status TEXT NOT NULL DEFAULT 'NONE',
                 payload_json TEXT
             );
             -- Phase 2 (D-N038) : service d'enveloppe d'autorite.
             CREATE TABLE IF NOT EXISTS leases (
                 mission_id TEXT NOT NULL,
                 fencing_token INTEGER NOT NULL,
                 holder TEXT NOT NULL,
                 issued_unix INTEGER NOT NULL,
                 expires_unix INTEGER NOT NULL,
                 envelope_json TEXT NOT NULL,
                 state TEXT NOT NULL DEFAULT 'live',
                 PRIMARY KEY (mission_id, fencing_token)
             );
             CREATE TABLE IF NOT EXISTS authority_traces (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 at_unix INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 mission_id TEXT,
                 operation_id TEXT,
                 fencing_token INTEGER,
                 detail_json TEXT
             );
             CREATE TABLE IF NOT EXISTS effects (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 mission_id TEXT NOT NULL,
                 operation_id TEXT NOT NULL,
                 correlation_id TEXT,
                 effect_class TEXT NOT NULL,
                 head_sha TEXT,
                 cost INTEGER NOT NULL DEFAULT 1,
                 fencing_token INTEGER NOT NULL,
                 trace_id INTEGER NOT NULL,
                 proof_state TEXT NOT NULL DEFAULT 'CURRENT',
                 at_unix INTEGER NOT NULL
             );",
        )
        .map_err(|e| format!("schema: {e}"))?;
        let version: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key='store_schema_version'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| format!("lecture version: {e}"))?;
        match version {
            None => {
                conn.execute(
                    "INSERT INTO meta(key, value) VALUES('store_schema_version', ?1)",
                    [STORE_SCHEMA_VERSION.to_string()],
                )
                .map_err(|e| format!("ecriture version: {e}"))?;
            }
            Some(v) => {
                let v: i64 = v.parse().map_err(|_| format!("version de schema illisible: {v}"))?;
                if v == 1 {
                    // Migration additive v1 -> v2 : les tables Phase 2 sont
                    // nouvelles (CREATE IF NOT EXISTS ci-dessus) ; aucune
                    // donnee v1 n'est touchee.
                    conn.execute(
                        "UPDATE meta SET value=?1 WHERE key='store_schema_version'",
                        [STORE_SCHEMA_VERSION.to_string()],
                    )
                    .map_err(|e| format!("migration schema v1->v2: {e}"))?;
                } else if v != STORE_SCHEMA_VERSION {
                    return Err(format!(
                        "version de schema du store {v} != attendue {STORE_SCHEMA_VERSION}"
                    ));
                }
            }
        }
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| format!("integrity_check: {e}"))?;
        if integrity != "ok" {
            return Err(format!("integrity_check: {integrity}"));
        }
        Ok(Store { conn })
    }

    /// Rejeu de l'etat canonique consigne : compte, ne resout rien par
    /// hypothese. Les effets en etat ATTEMPTED_UNKNOWN sont COMPTES au recu ;
    /// leur resolution releve de la reconciliation (Phase 3).
    pub fn replay(&self) -> Result<ReplayReport, String> {
        let hops: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM state_entries", [], |r| r.get(0))
            .map_err(|e| format!("rejeu (comptage): {e}"))?;
        let unknown: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM state_entries WHERE effect_status='ATTEMPTED_UNKNOWN'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("rejeu (inconnus): {e}"))?;
        Ok(ReplayReport {
            hops_replayed: hops,
            effects_unknown_pending: unknown,
        })
    }

    /// Etage 1 du recu de boot : l'entree est versee au store.
    pub fn record_boot_receipt(&self, boot_id: &str, receipt_json: &str, recorded_unix: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO boots(boot_id, receipt_json, recorded_unix) VALUES(?1, ?2, ?3)",
                rusqlite::params![boot_id, receipt_json, recorded_unix],
            )
            .map_err(|e| format!("versement du recu au store: {e}"))?;
        Ok(())
    }

    pub fn last_boot_receipt(&self) -> Option<String> {
        self.conn
            .query_row(
                "SELECT receipt_json FROM boots ORDER BY recorded_unix DESC, rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    // ---------- Phase 2 : service d'enveloppe ----------

    /// Trace d'autorite append-only. La trace est ecrite AVANT l'effet ou le
    /// retour du refus (design section 0.2) : son identifiant sert
    /// d'evidence_ref.
    pub fn trace(
        &self,
        kind: &str,
        mission_id: Option<&str>,
        operation_id: Option<&str>,
        fencing_token: Option<i64>,
        detail_json: &str,
        at_unix: i64,
    ) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO authority_traces(at_unix, kind, mission_id, operation_id, fencing_token, detail_json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![at_unix, kind, mission_id, operation_id, fencing_token, detail_json],
            )
            .map_err(|e| format!("ecriture de la trace {kind}: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Token courant d'une mission = le plus grand token emis (0 si aucun).
    /// Le service d'enveloppe est la SEULE autorite d'emission (PRD 2.2).
    pub fn current_token(&self, mission_id: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(fencing_token), 0) FROM leases WHERE mission_id=?1",
                [mission_id],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    pub fn insert_lease(
        &self,
        mission_id: &str,
        fencing_token: i64,
        holder: &str,
        issued_unix: i64,
        expires_unix: i64,
        envelope_json: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO leases(mission_id, fencing_token, holder, issued_unix, expires_unix, envelope_json, state)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'live')",
                rusqlite::params![mission_id, fencing_token, holder, issued_unix, expires_unix, envelope_json],
            )
            .map_err(|e| format!("insertion du bail: {e}"))?;
        Ok(())
    }

    pub fn get_lease(&self, mission_id: &str, fencing_token: i64) -> Option<(String, String, i64, i64, String)> {
        // (holder, state, issued_unix, expires_unix, envelope_json)
        self.conn
            .query_row(
                "SELECT holder, state, issued_unix, expires_unix, envelope_json FROM leases
                 WHERE mission_id=?1 AND fencing_token=?2",
                rusqlite::params![mission_id, fencing_token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_lease_state(&self, mission_id: &str, fencing_token: i64, state: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE leases SET state=?3 WHERE mission_id=?1 AND fencing_token=?2",
                rusqlite::params![mission_id, fencing_token, state],
            )
            .map_err(|e| format!("etat du bail: {e}"))?;
        Ok(())
    }

    pub fn update_lease_envelope(&self, mission_id: &str, fencing_token: i64, envelope_json: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE leases SET envelope_json=?3 WHERE mission_id=?1 AND fencing_token=?2",
                rusqlite::params![mission_id, fencing_token, envelope_json],
            )
            .map_err(|e| format!("enveloppe du bail: {e}"))?;
        Ok(())
    }

    /// Somme des couts des effets deja portes par un bail, par classe.
    pub fn spent_for(&self, mission_id: &str, fencing_token: i64, effect_class: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(cost), 0) FROM effects
                 WHERE mission_id=?1 AND fencing_token=?2 AND effect_class=?3",
                rusqlite::params![mission_id, fencing_token, effect_class],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// Transition d'effet AU STORE : le store refuse une transition presentee
    /// sous un bail fence (couplage PRD section 6, ecrit des deux cotes) —
    /// le token presente doit etre le token courant de la mission.
    pub fn insert_effect(
        &self,
        mission_id: &str,
        operation_id: &str,
        correlation_id: Option<&str>,
        effect_class: &str,
        head_sha: Option<&str>,
        cost: i64,
        fencing_token: i64,
        trace_id: i64,
        at_unix: i64,
    ) -> Result<i64, String> {
        let current = self.current_token(mission_id);
        if fencing_token != current {
            return Err(format!(
                "transition refusee au store : token {fencing_token} != token courant {current} (bail fence)"
            ));
        }
        self.conn
            .execute(
                "INSERT INTO effects(mission_id, operation_id, correlation_id, effect_class, head_sha, cost, fencing_token, trace_id, at_unix)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![mission_id, operation_id, correlation_id, effect_class, head_sha, cost, fencing_token, trace_id, at_unix],
            )
            .map_err(|e| format!("insertion de l'effet: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Effets d'une (mission, operation), du plus recent au plus ancien.
    pub fn effects_for(&self, mission_id: &str, operation_id: &str) -> Vec<(i64, Option<String>, String, i64)> {
        // (id, head_sha, proof_state, trace_id)
        let mut stmt = match self.conn.prepare(
            "SELECT id, head_sha, proof_state, trace_id FROM effects
             WHERE mission_id=?1 AND operation_id=?2 ORDER BY id DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(rusqlite::params![mission_id, operation_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn mark_effect_obsolete(&self, effect_id: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE effects SET proof_state='OBSOLETE' WHERE id=?1",
                [effect_id],
            )
            .map_err(|e| format!("obsolescence de la preuve: {e}"))?;
        Ok(())
    }

    /// Mecanisme de l'ancrage : ABSENT jusqu'a l'acte d'activation, puis
    /// ACTIVE (design section 0.1 — on ne declare pas le mecanisme avant
    /// ses preuves).
    pub fn anchor_mechanism(&self) -> String {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key='anchor_mechanism'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or_else(|| "ABSENT".to_string())
    }

    pub fn set_anchor_mechanism(&self, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES('anchor_mechanism', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [value],
            )
            .map_err(|e| format!("mecanisme de l'ancrage: {e}"))?;
        Ok(())
    }

    /// Capacites actuellement accordees = somme des capacites des baux
    /// vivants (etat reel, jamais declare).
    pub fn live_granted_count(&self, now_unix: i64) -> i64 {
        let mut stmt = match self.conn.prepare(
            "SELECT envelope_json FROM leases WHERE state='live' AND expires_unix > ?1",
        ) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let rows = stmt.query_map([now_unix], |r| r.get::<_, String>(0));
        let mut total = 0i64;
        if let Ok(it) = rows {
            for env_json in it.flatten() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&env_json) {
                    if let Some(caps) = v.pointer("/capabilities/permanent").and_then(|c| c.as_array()) {
                        total += caps.len() as i64;
                    }
                    if let Some(promos) = v.pointer("/capabilities/promotions").and_then(|c| c.as_array()) {
                        total += promos
                            .iter()
                            .filter(|p| {
                                p.get("expires_at_unix")
                                    .and_then(|e| e.as_i64())
                                    .map(|e| e > now_unix)
                                    .unwrap_or(false)
                            })
                            .count() as i64;
                    }
                }
            }
        }
        total
    }
}

