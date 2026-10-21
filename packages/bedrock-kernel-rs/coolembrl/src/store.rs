//! Store durable du daemon : SQLite en mode WAL sur les volumes du daemon
//! (voie qualifiee en Phase 0 du programme — ce module la consomme, il ne la
//! re-choisit pas). La Phase 1 ouvre le store et rejoue ce qu'il contient ;
//! le WorkGraph type qui definira les missions est la Phase 3.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub const STORE_SCHEMA_VERSION: i64 = 1;

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
                if v != STORE_SCHEMA_VERSION {
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
}
