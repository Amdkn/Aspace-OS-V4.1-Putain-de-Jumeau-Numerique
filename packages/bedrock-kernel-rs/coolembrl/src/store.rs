//! Store durable du daemon : SQLite en mode WAL sur les volumes du daemon
//! (voie qualifiee en Phase 0 du programme — ce module la consomme, il ne la
//! re-choisit pas). La Phase 1 ouvre le store et rejoue ce qu'il contient ;
//! le WorkGraph type qui definira les missions est la Phase 3.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub const STORE_SCHEMA_VERSION: i64 = 3;

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
             );
             -- Phase 3 (D-N039) : WorkGraph type d'execution.
             CREATE TABLE IF NOT EXISTS wg_missions (
                 mission_id TEXT PRIMARY KEY,
                 work_id TEXT NOT NULL,
                 envelope_ref TEXT,
                 observed_at TEXT,
                 recorded_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS wg_hops (
                 mission_id TEXT NOT NULL,
                 hop_id TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 status TEXT NOT NULL,
                 lease_holder TEXT,
                 fencing_token INTEGER,
                 effect_state TEXT NOT NULL DEFAULT 'NOT_ATTEMPTED',
                 effect_attempted_unix INTEGER,
                 assertion_json TEXT,
                 evidence_refs_json TEXT NOT NULL DEFAULT '[]',
                 recorded_at TEXT NOT NULL,
                 PRIMARY KEY (mission_id, hop_id)
             );
             CREATE TABLE IF NOT EXISTS wg_transitions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 mission_id TEXT NOT NULL,
                 hop_id TEXT NOT NULL,
                 transition_key TEXT NOT NULL,
                 event_type TEXT NOT NULL,
                 fencing_token INTEGER NOT NULL,
                 trace_id INTEGER NOT NULL,
                 detail_json TEXT,
                 recorded_unix INTEGER NOT NULL,
                 recorded_at TEXT NOT NULL,
                 UNIQUE (mission_id, hop_id, transition_key)
             );
             CREATE TABLE IF NOT EXISTS wg_reconciliations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 mission_id TEXT NOT NULL,
                 hop_id TEXT NOT NULL,
                 resolved_to TEXT NOT NULL,
                 source_system TEXT NOT NULL,
                 source_locator TEXT NOT NULL,
                 source_observed TEXT NOT NULL,
                 read_at TEXT NOT NULL,
                 recorded_at TEXT NOT NULL,
                 evidence_ref TEXT
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
                if v < STORE_SCHEMA_VERSION {
                    // Migrations additives (v1 -> v2 : enveloppe ; v2 -> v3 :
                    // WorkGraph) : les nouvelles tables sont creees par les
                    // CREATE IF NOT EXISTS ci-dessus ; aucune donnee
                    // existante n'est touchee.
                    conn.execute(
                        "UPDATE meta SET value=?1 WHERE key='store_schema_version'",
                        [STORE_SCHEMA_VERSION.to_string()],
                    )
                    .map_err(|e| format!("migration schema v{v} -> v{STORE_SCHEMA_VERSION}: {e}"))?;
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

    // ---------- Phase 4 : veille N2 ----------

    /// Lecture generique de la table meta (cle -> valeur, ou None).
    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .ok()
    }

    pub fn meta_set_pub(&self, key: &str, value: &str) -> Result<(), String> {
        self.meta_set(key, value, key)
    }

    fn meta_set(&self, key: &str, value: &str, ctx: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [key, value],
            )
            .map_err(|e| format!("{ctx}: {e}"))?;
        Ok(())
    }

    /// t2_ready_at : champ a ecriture unique, mecaniquement.
    /// - refuse toute ecriture pre-bascule : le seul ecrivain legitime est
    ///   l'acte de bascule (design section 0.5) ; la commande operateur
    ///   `veille-switchover` verifie la fenetre de concordance avant d'appeler.
    /// - refuse toute seconde ecriture : la valeur unique est l'horodatage
    ///   de l'acte de bascule (H-P4-12).
    pub fn set_t2_ready_at(&self, iso: &str) -> Result<(), String> {
        if self.meta_get("t2_ready_at").is_some() {
            return Err("T2_ALREADY_SET: t2_ready_at est a ecriture unique (H-P4-12)".into());
        }
        self.meta_set("t2_ready_at", iso, "t2_ready_at")
    }

    pub fn t2_ready_at(&self) -> Option<String> {
        self.meta_get("t2_ready_at")
    }

    /// Stade du protocole de relais : A (ombre), B (concordance),
    /// C (bascule consignée), D (veille du veilleur). Defaut A.
    pub fn veille_stage(&self) -> String {
        self.meta_get("veille_stage").unwrap_or_else(|| "A".to_string())
    }

    pub fn set_veille_stage(&self, stage: &str) -> Result<(), String> {
        if !["A", "B", "C", "D"].contains(&stage) {
            return Err(format!("stade de veille inconnu: {stage}"));
        }
        self.meta_set("veille_stage", stage, "veille_stage")
    }

    /// Version de spec implementee par le service veille (rapporte a
    /// chaque cycle ; un ecart avec la version courante = anomalie de
    /// maintenance, H-P4-10).
    pub fn veille_spec_version(&self) -> Option<String> {
        self.meta_get("veille_spec_version")
    }

    pub fn set_veille_spec_version(&self, v: &str) -> Result<(), String> {
        self.meta_set("veille_spec_version", v, "veille_spec_version")
    }

    /// Compteur de cycles concordants consecutifs (stade B) et debut de
    /// la fenetre courante. Toute discordance remet le compteur a zero
    /// (design section 0.3).
    pub fn veille_concordant(&self) -> i64 {
        self.meta_get("veille_concordant")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    pub fn set_veille_concordant(&self, n: i64) -> Result<(), String> {
        self.meta_set("veille_concordant", &n.to_string(), "veille_concordant")
    }

    pub fn veille_window_start(&self) -> Option<i64> {
        self.meta_get("veille_window_start")
            .and_then(|v| v.parse().ok())
    }

    pub fn set_veille_window_start(&self, unix: i64) -> Result<(), String> {
        self.meta_set("veille_window_start", &unix.to_string(), "veille_window_start")
    }

    /// Instantane des baux vivants : (mission_id, holder, expires_unix, issued_unix).
    pub fn live_leases_snapshot(&self) -> Vec<(String, String, i64, i64)> {
        let mut out = Vec::new();
        let mut stmt = match self.conn.prepare(
            "SELECT mission_id, holder, expires_unix, issued_unix FROM leases WHERE state='live'",
        ) {
            Ok(s) => s,
            Err(_) => return out,
        };
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        }) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
        out
    }

    /// Derniere transition par mission (recorded_unix max) — mesure la
    /// progression pour le motif "cale sans crash".
    pub fn mission_last_transition(&self) -> std::collections::HashMap<String, i64> {
        let mut out = std::collections::HashMap::new();
        let mut stmt = match self
            .conn
            .prepare("SELECT mission_id, MAX(recorded_unix) FROM wg_transitions GROUP BY mission_id")
        {
            Ok(s) => s,
            Err(_) => return out,
        };
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            for row in rows.flatten() {
                out.insert(row.0, row.1);
            }
        }
        out
    }

    /// Capacites actuellement accordees = somme des capacites des baux
    /// vivants (etat reel, jamais declare).
    pub fn live_granted_count(&self, now_unix: i64) -> i64 {        let mut stmt = match self.conn.prepare(
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

    // ---------- Phase 3 : WorkGraph type ----------

    pub fn wg_upsert_mission(
        &self,
        mission_id: &str,
        work_id: &str,
        envelope_ref: Option<&str>,
        recorded_at: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO wg_missions(mission_id, work_id, envelope_ref, recorded_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(mission_id) DO NOTHING",
                rusqlite::params![mission_id, work_id, envelope_ref, recorded_at],
            )
            .map_err(|e| format!("mission du WorkGraph: {e}"))?;
        Ok(())
    }

    /// (work_id, envelope_ref, observed_at, recorded_at)
    pub fn wg_get_mission(&self, mission_id: &str) -> Option<(String, Option<String>, Option<String>, String)> {
        self.conn
            .query_row(
                "SELECT work_id, envelope_ref, observed_at, recorded_at FROM wg_missions WHERE mission_id=?1",
                [mission_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn wg_set_mission_envelope(&self, mission_id: &str, envelope_ref: &str, recorded_at: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE wg_missions SET envelope_ref=?2, recorded_at=?3 WHERE mission_id=?1",
                rusqlite::params![mission_id, envelope_ref, recorded_at],
            )
            .map_err(|e| format!("envelope_ref de la mission: {e}"))?;
        Ok(())
    }

    pub fn wg_set_mission_observed(&self, mission_id: &str, observed_at: &str, recorded_at: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE wg_missions SET observed_at=?2, recorded_at=?3 WHERE mission_id=?1",
                rusqlite::params![mission_id, observed_at, recorded_at],
            )
            .map_err(|e| format!("observed_at de la mission: {e}"))?;
        Ok(())
    }

    pub fn wg_hops(&self, mission_id: &str) -> Vec<HopRow> {
        let mut stmt = match self.conn.prepare(
            "SELECT hop_id, seq, kind, status, lease_holder, fencing_token, effect_state,
                    effect_attempted_unix, assertion_json, evidence_refs_json, recorded_at
             FROM wg_hops WHERE mission_id=?1 ORDER BY seq",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([mission_id], |r| {
            Ok(HopRow {
                hop_id: r.get(0)?,
                seq: r.get(1)?,
                kind: r.get(2)?,
                status: r.get(3)?,
                lease_holder: r.get(4)?,
                fencing_token: r.get(5)?,
                effect_state: r.get(6)?,
                effect_attempted_unix: r.get(7)?,
                assertion_json: r.get(8)?,
                evidence_refs_json: r.get(9)?,
                recorded_at: r.get(10)?,
            })
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn wg_get_hop(&self, mission_id: &str, hop_id: &str) -> Option<HopRow> {
        self.wg_hops(mission_id).into_iter().find(|h| h.hop_id == hop_id)
    }

    pub fn wg_insert_hop(
        &self,
        mission_id: &str,
        hop_id: &str,
        seq: i64,
        kind: &str,
        status: &str,
        holder: &str,
        fencing_token: i64,
        assertion_json: Option<&str>,
        recorded_at: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO wg_hops(mission_id, hop_id, seq, kind, status, lease_holder, fencing_token, assertion_json, recorded_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![mission_id, hop_id, seq, kind, status, holder, fencing_token, assertion_json, recorded_at],
            )
            .map_err(|e| format!("insertion du hop: {e}"))?;
        Ok(())
    }

    pub fn wg_update_hop(
        &self,
        mission_id: &str,
        hop_id: &str,
        status: &str,
        effect_state: &str,
        effect_attempted_unix: Option<i64>,
        holder: &str,
        fencing_token: i64,
        evidence_refs_json: &str,
        recorded_at: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE wg_hops SET status=?3, effect_state=?4, effect_attempted_unix=?5,
                        lease_holder=?6, fencing_token=?7, evidence_refs_json=?8, recorded_at=?9
                 WHERE mission_id=?1 AND hop_id=?2",
                rusqlite::params![mission_id, hop_id, status, effect_state, effect_attempted_unix, holder, fencing_token, evidence_refs_json, recorded_at],
            )
            .map_err(|e| format!("mise a jour du hop: {e}"))?;
        Ok(())
    }

    pub fn wg_transition_id(&self, mission_id: &str, hop_id: &str, transition_key: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT id FROM wg_transitions WHERE mission_id=?1 AND hop_id=?2 AND transition_key=?3",
                rusqlite::params![mission_id, hop_id, transition_key],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Ecriture d'une transition AU STORE : le store refuse une transition
    /// presentee sous un token qui n'est pas le token courant de la mission
    /// (exigence 2.4 de la qualification stockage, ecrite des deux cotes —
    /// le service verifie, le store refuse).
    pub fn wg_insert_transition(
        &self,
        mission_id: &str,
        hop_id: &str,
        transition_key: &str,
        event_type: &str,
        fencing_token: i64,
        trace_id: i64,
        detail_json: &str,
        recorded_unix: i64,
        recorded_at: &str,
    ) -> Result<i64, String> {
        let current = self.current_token(mission_id);
        if fencing_token != current {
            return Err(format!(
                "transition refusee au store : token {fencing_token} != token courant {current} (bail fence ou expire)"
            ));
        }
        self.conn
            .execute(
                "INSERT INTO wg_transitions(mission_id, hop_id, transition_key, event_type, fencing_token, trace_id, detail_json, recorded_unix, recorded_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![mission_id, hop_id, transition_key, event_type, fencing_token, trace_id, detail_json, recorded_unix, recorded_at],
            )
            .map_err(|e| format!("insertion de la transition: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn wg_insert_reconciliation(
        &self,
        mission_id: &str,
        hop_id: &str,
        resolved_to: &str,
        source_system: &str,
        source_locator: &str,
        source_observed: &str,
        read_at: &str,
        recorded_at: &str,
        evidence_ref: &str,
    ) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO wg_reconciliations(mission_id, hop_id, resolved_to, source_system, source_locator, source_observed, read_at, recorded_at, evidence_ref)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![mission_id, hop_id, resolved_to, source_system, source_locator, source_observed, read_at, recorded_at, evidence_ref],
            )
            .map_err(|e| format!("insertion de la reconciliation: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// (mission_id, hop_id, effect_attempted_unix) des effets en
    /// ATTEMPTED_UNKNOWN, toutes missions — la file d'observation du PRD 5.
    pub fn wg_pending_unknown(&self) -> Vec<(String, String, Option<i64>)> {
        let mut stmt = match self.conn.prepare(
            "SELECT mission_id, hop_id, effect_attempted_unix FROM wg_hops
             WHERE effect_state='ATTEMPTED_UNKNOWN' ORDER BY effect_attempted_unix",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HopRow {
    pub hop_id: String,
    pub seq: i64,
    pub kind: String,
    pub status: String,
    pub lease_holder: Option<String>,
    pub fencing_token: Option<i64>,
    pub effect_state: String,
    pub effect_attempted_unix: Option<i64>,
    pub assertion_json: Option<String>,
    pub evidence_refs_json: String,
    pub recorded_at: String,
}

