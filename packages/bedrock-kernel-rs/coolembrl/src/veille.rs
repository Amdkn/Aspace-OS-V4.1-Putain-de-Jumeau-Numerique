//! Phase 4 — service `veille` : second corps de la veille L00 (watchdog N2).
//!
//! N2 execute la spec W1-W7 (`docs/SPEC-VEILLE-W1-W7.md`, version rapportee
//! SPEC_VERSION) sur ses PROPRES lectures des sources primaires (design
//! section 0.4 : l'ombre lit le monde, pas l'interimaire). Il consomme les
//! surfaces internes du daemon (file UNKNOWN sans seuil, etats de missions
//! et de baux, etat du daemon) et ne mute AUCUN objet veille (PRD §3).
//!
//! Protocole de relais (design §2) : A ombre (ne route rien) → B
//! concordance (fenetre 14 jours ET >=336 cycles comparables consecutifs
//! concordants, conjonctifs) → C bascule consignee (acte unique, trois
//! traces, recu R10) → D veille du veilleur. La bascule et le rollback sont
//! des actes d'operateur mecaniquement gates ; jamais des glissements.

use crate::store::Store;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Version de la spec W1-W7 implementee — rapportee a chaque cycle
/// (H-P4-10 : un ecart avec la version courante = anomalie de maintenance).
pub const SPEC_VERSION: &str = "W1-W7@v1.0.0";
/// Source de trace de N2, fixee par le PRD.
pub const TRACE_SOURCE: &str = "noyau-n2";
/// Intervalle nominal d'un cycle de veille : 1 h (design §0.2).
pub const INTERVAL_SECS_DEFAULT: u64 = 3600;

// ---------- Seuils de la spec, appliques par le veilleur ----------
// Ils appartiennent a la spec, jamais au store (H-P3-5b re-verifie).

pub const SEUIL_SILENCE_REGISTRE_SECS: i64 = 12 * 3600; // W1
pub const SEUIL_GEL_MAIN_SECS: i64 = 24 * 3600; // W2
pub const SEUIL_FRAICHEUR_TRACE_SECS: i64 = 2 * 3600; // W5
pub const SEUIL_SILENCE_PORTEUR_SECS: i64 = 24 * 3600; // W7
pub const SEUIL_VIEILLISSEMENT_UNKNOWN_SECS: i64 = 12 * 3600; // file UNKNOWN
pub const CYCLES_CALE_SANS_CRASH: i64 = 2; // motif 2x (design §0.7 heredite)
pub const RECOVERIES_AVANT_RESCUE: i64 = 3; // semantique H-03-A1 signee

// ---------- Fenetre de concordance (design §0.2) : bornes conjonctives ----------

pub const FENETRE_JOURS_MIN: i64 = 14;
pub const FENETRE_CYCLES_MIN: i64 = 336;

/// Un verdict par item de la spec.
#[derive(Clone, Debug)]
pub struct ItemVerdict {
    pub item: &'static str,
    pub verdict: String, // "OK" ou "ANOMALIE"
    pub detail: String,
}

impl ItemVerdict {
    fn ok(item: &'static str, detail: impl Into<String>) -> Self {
        Self { item, verdict: "OK".into(), detail: detail.into() }
    }
    fn anomalie(item: &'static str, detail: impl Into<String>) -> Self {
        Self { item, verdict: "ANOMALIE".into(), detail: detail.into() }
    }
}

/// Instantane de l'etat du daemon porte par le cycle de veille.
#[derive(Clone, Debug)]
pub struct DaemonSnapshot {
    pub stage: String,
    pub store_integrity: String,
    pub anchor_mechanism: String,
}

/// Rapport d'un cycle — ce que les fixtures H-P4 asserten.
#[derive(Clone, Debug)]
pub struct CycleReport {
    pub cycle: i64,
    pub at_iso: String,
    pub stade: String,
    pub spec_version: String,
    pub verdicts: Vec<ItemVerdict>,
    pub alerts_routed: Vec<Value>,
    pub routing_requests: Vec<Value>,
    pub concordant: i64,
    pub window_start_iso: Option<String>,
    pub trace_line: String,
}

impl CycleReport {
    pub fn to_json(&self) -> Value {
        json!({
            "cycle": self.cycle,
            "at": self.at_iso,
            "stade": self.stade,
            "spec_version": self.spec_version,
            "verdicts": self.verdicts.iter().map(|v| json!({
                "item": v.item, "verdict": v.verdict, "detail": v.detail,
            })).collect::<Vec<_>>(),
            "alerts_routed": self.alerts_routed,
            "routing_requests": self.routing_requests,
            "concordant_consecutifs": self.concordant,
            "debut_fenetre": self.window_start_iso,
            "trace_line": self.trace_line,
        })
    }
}
#[derive(Clone)]
pub struct VeillePaths {
    pub registry: PathBuf,
    pub interim_trace: PathBuf,
    pub trace: PathBuf,
    pub outbox: PathBuf,
    pub concordance_report: PathBuf,
    pub policy_dir: PathBuf,
    pub journal_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub bearers_map: PathBuf,
    /// Version gouvernee de la spec (fichier pose par la voie Yaz -> Clara -> S1).
    /// Si present, chaque cycle compare la version implementee ; un ecart est
    /// une anomalie de maintenance (H-P4-10).
    pub spec_courante: Option<PathBuf>,
}

pub struct Veille {
    store: Arc<Mutex<Store>>,
    paths: VeillePaths,
    pub interval_secs: u64,
    gh_stub: Option<PathBuf>,
    home: PathBuf,
    last_report: Arc<Mutex<Option<CycleReport>>>,
}

impl Veille {
    pub fn new(
        store: Arc<Mutex<Store>>,
        paths: VeillePaths,
        interval_secs: u64,
        gh_stub: Option<PathBuf>,
        home: PathBuf,
        last_report: Arc<Mutex<Option<CycleReport>>>,
    ) -> Self {
        Self { store, paths, interval_secs, gh_stub, home, last_report }
    }

    pub fn last_report_slot(&self) -> Arc<Mutex<Option<CycleReport>>> {
        Arc::clone(&self.last_report)
    }

    /// Accesseurs d'etat pour le serveur (MCP `veille_*`).
    pub fn store_t2(&self) -> Option<String> {
        self.store.lock().ok()?.t2_ready_at()
    }

    pub fn store_stage(&self) -> String {
        self.store
            .lock()
            .map(|s| s.veille_stage())
            .unwrap_or_else(|_| "?".into())
    }

    /// `veille_status` : stade, spec, autorite, t2, concordance.
    pub fn status_json(&self) -> Value {
        let (concordant, window) = self
            .store
            .lock()
            .map(|s| (s.veille_concordant(), s.veille_window_start()))
            .unwrap_or((0, None));
        let last = self.last_report.lock().ok().and_then(|r| r.clone());
        json!({
            "service": "veille",
            "source_trace": TRACE_SOURCE,
            "spec_version": SPEC_VERSION,
            "stade": self.store_stage(),
            "autoritaire": matches!(self.store_stage().as_str(), "C" | "D"),
            "t2_ready_at": self.store_t2().map(Value::String).unwrap_or(Value::Null),
            "concordant_consecutifs": concordant,
            "fenetre": {"jours_min": FENETRE_JOURS_MIN, "cycles_min": FENETRE_CYCLES_MIN},
            "debut_fenetre": window.map(crate::timefmt::iso8601_edt),
            "dernier_cycle": last.as_ref().map(|r| r.cycle),
            "dernier_battement": last.as_ref().map(|r| r.at_iso.clone()),
        })
    }

    /// `veille_last_beat` : le dernier battement ecrit a la trace.
    pub fn last_beat_json(&self) -> Value {
        match self.last_report.lock().ok().and_then(|r| r.clone()) {
            Some(r) => json!({"at": r.at_iso, "cycle": r.cycle, "stade": r.stade, "source": TRACE_SOURCE}),
            None => json!({"note": "aucun cycle de veille execute pour l'instant"}),
        }
    }

    /// `veille_observations` : les verdicts par item du dernier cycle.
    pub fn observations_json(&self) -> Value {
        match self.last_report.lock().ok().and_then(|r| r.clone()) {
            Some(r) => json!({
                "cycle": r.cycle, "at": r.at_iso, "stade": r.stade,
                "verdicts": r.verdicts.iter().map(|v| json!({
                    "item": v.item, "verdict": v.verdict, "detail": v.detail,
                })).collect::<Vec<_>>(),
            }),
            None => json!({"note": "aucun cycle de veille execute pour l'instant"}),
        }
    }

    // ---------- Cycle ----------

    /// Execute un cycle complet de veille. Appelable de facon synchrone
    /// (fixtures) ou par le thread horaire du daemon (production).
    pub fn run_cycle(&self, now_unix: i64, snap: &DaemonSnapshot) -> Result<CycleReport, String> {
        let store = self.store.lock().map_err(|e| format!("veille: verrou store: {e}"))?;
        let cycle: i64 = store.meta_get("veille_cycle").and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
        store
            .meta_set_pub("veille_cycle", &cycle.to_string())
            .map_err(|e| format!("veille: compteur de cycle: {e}"))?;
        let stade = store.veille_stage();

        // H-P4-10 : la version de spec est rapportee ; une version perimee
        // est une anomalie de maintenance.
        let mut verdicts: Vec<ItemVerdict> = Vec::new();
        match store.veille_spec_version() {
            None => {
                store.set_veille_spec_version(SPEC_VERSION).map_err(|e| format!("veille: {e}"))?;
                verdicts.push(ItemVerdict::ok(
                    "SPEC",
                    format!("version de spec posee par acte trace : {SPEC_VERSION}"),
                ));
            }
            Some(v) if v != SPEC_VERSION => {
                verdicts.push(ItemVerdict::anomalie(
                    "SPEC",
                    format!("version perimee rapportee {v} != {SPEC_VERSION} — anomalie de maintenance"),
                ));
            }
            _ => {
                // La version gouvernee (voie Yaz -> Clara -> S1) : tout ecart
                // avec la version implementee est une anomalie de maintenance.
                let mut spec_note = format!("version implementee {SPEC_VERSION}");
                if let Some(p) = &self.paths.spec_courante {
                    if let Ok(g) = fs::read_to_string(p) {
                        let g = g.trim().to_string();
                        if g != SPEC_VERSION {
                            verdicts.push(ItemVerdict::anomalie(
                                "SPEC",
                                format!("spec gouvernee {g} != version implementee {SPEC_VERSION} — anomalie de maintenance"),
                            ));
                        } else {
                            spec_note.push_str(" = version gouvernee");
                        }
                    }
                }
                verdicts.push(ItemVerdict::ok("SPEC", spec_note));
            }
        }

        drop(store);
        // Surfaces externes W1-W7 (lectures independantes, design §0.4).
        verdicts.push(self.w1_registre(now_unix));
        verdicts.push(self.w2_main(now_unix));
        verdicts.push(self.w3_integrite());
        verdicts.push(self.w4_policy());
        verdicts.push(self.w5_fraicheur(now_unix, cycle));
        verdicts.push(self.w6_fichiers());
        verdicts.push(self.w7_porteurs(now_unix));
        // Surfaces internes du daemon.
        verdicts.push(self.uq_file_unknown(now_unix));
        verdicts.push(self.ls_baux_missions(now_unix));
        verdicts.push(self.ds_etat_daemon(snap));

        let at_iso = crate::timefmt::iso8601_edt(now_unix);
        let mut alerts_routed: Vec<Value> = Vec::new();
        let mut routing_requests: Vec<Value> = Vec::new();

        // Stades A/B : l'ombre consigne, ne route rien (H-P4-1).
        let authoritative = stade == "C" || stade == "D";
        if authoritative {
            for v in &verdicts {
                if v.verdict == "ANOMALIE" {
                    let alert = json!({
                        "at": at_iso, "classe": "alerte", "a": "S1",
                        "item": v.item, "detail": v.detail,
                        "source_primaire": "noyau-n2", "stade": stade,
                    });
                    alerts_routed.push(alert);
                }
            }
            // Les demandes de routage recovery/rescue sont ecrites par les
            // checks ls (elles sont dans routing_requests via l'outbox).
            routing_requests = self.drain_routing_queue();
        }

        // Stade B : concordance par item contre la trace de l'interimaire.
        let (concordant, window_start_iso) = if stade == "B" {
            self.etape_concordance(now_unix, &at_iso, &verdicts)?
        } else {
            let store = self.store.lock().map_err(|e| format!("veille: verrou store: {e}"))?;
            let c = store.veille_concordant();
            let w = store.veille_window_start().map(crate::timefmt::iso8601_edt);
            (c, w)
        };

        // Ecriture de l'outbox (stades C/D uniquement — jamais en A/B).
        if authoritative {
            self.append_outbox(&alerts_routed, &routing_requests)?;
        }

        let report = CycleReport {
            cycle,
            at_iso: at_iso.clone(),
            stade: stade.clone(),
            spec_version: SPEC_VERSION.into(),
            verdicts,
            alerts_routed,
            routing_requests,
            concordant,
            window_start_iso,
            trace_line: String::new(),
        };
        let trace_line = self.trace_line(&report);
        self.append_trace(&trace_line)?;
        let mut report = report;
        report.trace_line = trace_line;
        *self.last_report.lock().map_err(|e| format!("veille: slot rapport: {e}"))? = Some(report.clone());
        Ok(report)
    }

    // ---------- Ligne de trace ----------

    fn trace_line(&self, r: &CycleReport) -> String {
        let mut parts: Vec<String> = Vec::new();
        for v in &r.verdicts {
            let mark = if v.verdict == "OK" { "OK" } else { "ANOMALIE" };
            if v.verdict == "OK" && v.item != "SPEC" {
                parts.push(format!("{} {}", v.item, mark));
            } else {
                parts.push(format!("{} {} ({})", v.item, mark, v.detail));
            }
        }
        let routed = if r.stade == "C" || r.stade == "D" {
            let n = r.alerts_routed.len() + r.routing_requests.len();
            if n == 0 { "Aucune anomalie routée.".into() } else { format!("{n} routage(s).") }
        } else {
            "Ombre : rien routé.".into()
        };
        format!(
            "- **{}** — Run #{} (créneau {}) : {} · spec {} · stade {}{} · source {} · {}",
            r.at_iso,
            r.cycle,
            r.at_iso,
            parts.join(" · "),
            r.spec_version,
            r.stade,
            if r.stade == "A" || r.stade == "B" { " (non autoritaire)" } else { " (autoritaire)" },
            TRACE_SOURCE,
            routed,
        )
    }

    fn append_trace(&self, line: &str) -> Result<(), String> {
        use std::io::Write;
        if let Some(parent) = self.paths.trace.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("veille: trace: {e}"))?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.paths.trace)
            .map_err(|e| format!("veille: trace: {e}"))?;
        writeln!(f, "{line}").map_err(|e| format!("veille: trace: {e}"))?;
        Ok(())
    }

    fn append_outbox(&self, alerts: &[Value], requests: &[Value]) -> Result<(), String> {
        use std::io::Write;
        if alerts.is_empty() && requests.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.paths.outbox.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("veille: outbox: {e}"))?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.paths.outbox)
            .map_err(|e| format!("veille: outbox: {e}"))?;
        for a in alerts.iter().chain(requests.iter()) {
            writeln!(f, "{}", serde_json::to_string(a).unwrap())
                .map_err(|e| format!("veille: outbox: {e}"))?;
        }
        Ok(())
    }

    /// File d'attente des demandes de routage ecrites par les checks
    /// (ls) pendant le cycle — videe a chaque cycle.
    fn drain_routing_queue(&self) -> Vec<Value> {
        let q = self.paths.outbox.with_extension("queue.json");
        if !q.exists() {
            return Vec::new();
        }
        let v: Vec<Value> = fs::read_to_string(&q)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let _ = fs::remove_file(&q);
        v
    }

    fn queue_routing(&self, req: Value) {
        let q = self.paths.outbox.with_extension("queue.json");
        let mut v: Vec<Value> = fs::read_to_string(&q)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        v.push(req);
        let _ = fs::write(&q, serde_json::to_string(&v).unwrap());
    }

    // ---------- Appels gh (reels ou stub de fixture) ----------

    fn gh_api(&self, path: &str) -> Result<Value, String> {
        if let Some(stub) = &self.gh_stub {
            let out = Command::new(stub)
                .arg("api")
                .arg(path)
                .output()
                .map_err(|e| format!("veille: stub gh: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "veille: stub gh en echec: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            return serde_json::from_slice(&out.stdout).map_err(|e| format!("veille: stub gh JSON: {e}"));
        }
        let out = Command::new("gh")
            .arg("api")
            .arg(path)
            .output()
            .map_err(|e| format!("veille: gh api: {e}"))?;
        if !out.status.success() {
            return Err(format!("veille: gh api {path}: {}", String::from_utf8_lossy(&out.stderr)));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("veille: gh JSON: {e}"))
    }
}

// ---------- Checks W1-W7 (lectures independantes des sources primaires) ----------

impl Veille {
    fn read_text(&self, p: &std::path::Path) -> Result<String, String> {
        fs::read_to_string(p).map_err(|e| format!("veille: lecture {}: {e}", p.display()))
    }

    // W1 : silences du registre. Parse les lignes `| D-N### | ... | STATUT | ... | MAJ`.
    fn w1_registre(&self, now_unix: i64) -> ItemVerdict {
        let text = match self.read_text(&self.paths.registry) {
            Ok(t) => t,
            Err(e) => return ItemVerdict::anomalie("W1", format!("registre illisible : {e}")),
        };
        // Journal : datetimes des entrees mentionnant chaque dispatch.
        let mut last_event: HashMap<String, i64> = HashMap::new();
        for line in text.lines() {
            if let Some(ts) = parse_fr_datetime(line) {
                for m in extract_dispatch_ids(line) {
                    let e = last_event.entry(m).or_insert(0);
                    if ts > *e {
                        *e = ts;
                    }
                }
            }
        }
        let mut anomalies: Vec<String> = Vec::new();
        let mut open = 0;
        for line in text.lines() {
            let t = line.trim_start();
            if !t.starts_with("| D-N") {
                continue;
            }
            let cells: Vec<&str> = t.split('|').collect();
            if cells.len() < 10 {
                continue;
            }
            let id = cells[1].trim().to_string();
            let executant = cells[4].trim();
            let statut = cells[6].trim();
            let maj = cells[9].trim();
            if statut != "EN COURS" && statut != "ASSIGNÉ" && statut != "ASSIGNE" {
                continue;
            }
            open += 1;
            let mut last = last_event.get(&id).copied().unwrap_or(0);
            if let Some(ts) = parse_fr_datetime(maj) {
                if ts > last {
                    last = ts;
                }
            }
            if last == 0 {
                anomalies.push(format!("{id} : aucun evenement horodate lisible (porteur {executant})"));
                continue;
            }
            let age = now_unix - last;
            if age > SEUIL_SILENCE_REGISTRE_SECS {
                anomalies.push(format!(
                    "{id} : silence {} (porteur {executant}, seuil 12 h)",
                    fmt_duree(age)
                ));
            }
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("W1", format!("{open} dispatch(s) ouvert(s), aucun silence > 12 h"))
        } else {
            ItemVerdict::anomalie("W1", anomalies.join(" ; "))
        }
    }

    /// Retourne aussi (open_count, sequence_due) pour W2.
    fn registry_open(&self) -> (usize, Vec<String>) {
        let text = match self.read_text(&self.paths.registry) {
            Ok(t) => t,
            Err(_) => return (0, Vec::new()),
        };
        let mut open = Vec::new();
        for line in text.lines() {
            let t = line.trim_start();
            if !t.starts_with("| D-N") {
                continue;
            }
            let cells: Vec<&str> = t.split('|').collect();
            if cells.len() < 10 {
                continue;
            }
            let statut = cells[6].trim();
            if statut == "EN COURS" || statut == "ASSIGNÉ" || statut == "ASSIGNE" {
                open.push(cells[1].trim().to_string());
            }
        }
        (open.len(), open)
    }

    // W2 : tete de main fige + issues ouvertes au fond livre.
    // Lecture mecanique de la sequence signee : la sequence vit au registre
    // (F-02 -> G-01-C / BYB Noyau -> G-02-C -> D-01 -> E-01 -> F-01) ; une
    // etape est "due" si son predecesseur est clos sans qu'elle soit ouverte.
    fn w2_main(&self, now_unix: i64) -> ItemVerdict {
        let head = match self.gh_api("repos/Amdkn/Aspace_OS-V4/commits/main") {
            Ok(v) => v,
            Err(e) => return ItemVerdict::anomalie("W2", format!("tete de main illisible : {e}")),
        };
        let sha = head.pointer("/sha").and_then(|s| s.as_str()).unwrap_or("?");
        let date = head.pointer("/commit/committer/date").and_then(|s| s.as_str()).unwrap_or("");
        let head_unix = parse_iso8601(date).unwrap_or(0);
        let age = now_unix - head_unix;
        let mut notes: Vec<String> = Vec::new();
        let mut anomalies: Vec<String> = Vec::new();
        notes.push(format!("main {} age {}", &sha[..7.min(sha.len())], fmt_duree(age)));
        if age > SEUIL_GEL_MAIN_SECS {
            let (open_n, _) = self.registry_open();
            if open_n == 0 {
                anomalies.push(format!(
                    "main fige depuis {} sans aucun dispatch ouvert (gel > 24 h)",
                    fmt_duree(age)
                ));
            } else {
                notes.push(format!("gel > 24 h mais {open_n} dispatch(s) ouvert(s) : etape en vol"));
            }
        }
        // Issues ouvertes au fond livre : reference a une PR mergee sans
        // commentaire de statut.
        match self.gh_api("repos/Amdkn/Aspace_OS-V4/issues?state=open&per_page=30") {
            Ok(v) => {
                if let Some(arr) = v.as_array() {
                    for issue in arr.iter().filter(|i| i.get("pull_request").is_none()) {
                        let num = issue.get("number").and_then(|n| n.as_i64()).unwrap_or(0);
                        let body = issue.get("body").and_then(|b| b.as_str()).unwrap_or("");
                        for pr in extract_pr_refs(body) {
                            if let Ok(p) = self.gh_api(&format!("repos/Amdkn/Aspace_OS-V4/pulls/{pr}")) {
                                let merged = p.get("merged").and_then(|m| m.as_bool()).unwrap_or(false);
                                let merged_at = p.get("merged_at").and_then(|m| m.as_str()).unwrap_or("");
                                if merged {
                                    // Un commentaire de statut posterieur au merge ?
                                    let has_status = self
                                        .gh_api(&format!(
                                            "repos/Amdkn/Aspace_OS-V4/issues/{num}/comments?per_page=20"
                                        ))
                                        .ok()
                                        .and_then(|c| c.as_array().cloned())
                                        .map(|cs| {
                                            let mu = parse_iso8601(merged_at).unwrap_or(0);
                                            cs.iter().any(|cm| {
                                                cm.get("created_at")
                                                    .and_then(|d| d.as_str())
                                                    .and_then(parse_iso8601)
                                                    .map(|t| t > mu)
                                                    .unwrap_or(false)
                                            })
                                        })
                                        .unwrap_or(false);
                                    if !has_status {
                                        anomalies.push(format!(
                                            "issue #{num} ouverte, fond livre par PR #{pr} mergee sans statut"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => anomalies.push(format!("issues illisibles : {e}")),
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("W2", notes.join(" ; "))
        } else {
            ItemVerdict::anomalie("W2", format!("{} ; {}", notes.join(" ; "), anomalies.join(" ; ")))
        }
    }

    // W3 : integrite des artefacts de regime (empreintes vs watermarks).
    fn w3_integrite(&self) -> ItemVerdict {
        let store = match self.store.lock() {
            Ok(s) => s,
            Err(e) => return ItemVerdict::anomalie("W3", format!("verrou store : {e}")),
        };
        let files: [(&str, PathBuf); 2] = [
            ("registre", self.paths.registry.clone()),
            ("policy_registry", self.paths.policy_dir.join("REGISTRY.md")),
        ];
        let mut anomalies: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (name, path) in files {
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    anomalies.push(format!("{name} illisible : {e}"));
                    continue;
                }
            };
            let fp = sha256_hex(&bytes);
            let key = format!("veille_w3_{name}");
            match store.meta_get(&key) {
                None => {
                    let _ = store.meta_set_pub(&key, &fp);
                    notes.push(format!("{name} : watermark pose {}", &fp[..12]));
                }
                Some(w) if w == fp => notes.push(format!("{name} : inchange {}", &fp[..12])),
                Some(_) => anomalies.push(format!("{name} : EMPREINTE CHANGEE sans acte couvreur nomme")),
            }
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("W3", notes.join(" ; "))
        } else {
            ItemVerdict::anomalie("W3", anomalies.join(" ; "))
        }
    }

    // W4 : version du policy store.
    fn w4_policy(&self) -> ItemVerdict {
        let reg = match self.read_text(&self.paths.policy_dir.join("REGISTRY.md")) {
            Ok(t) => t,
            Err(e) => return ItemVerdict::anomalie("W4", format!("REGISTRY.md illisible : {e}")),
        };
        let version = extract_version(&reg).unwrap_or_else(|| "?".into());
        let mtime = fs::metadata(self.paths.policy_dir.join("CHANGELOG.md"))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ItemVerdict::ok("W4", format!("policy {version}, CHANGELOG mtime {}", crate::timefmt::iso8601_edt(mtime)))
    }

    // W5 : fraicheur des traces (dead-man).
    fn w5_fraicheur(&self, now_unix: i64, cycle: i64) -> ItemVerdict {
        let date = crate::timefmt::iso8601_edt(now_unix);
        let journal = self.paths.journal_dir.join(format!("{}.md", &date[..10]));
        let mut anomalies: Vec<String> = Vec::new();
        for (name, path) in [
            ("journal", journal),
            ("trace L00", self.paths.interim_trace.clone()),
        ] {
            match file_age_secs(&path, now_unix) {
                Some(age) if age > SEUIL_FRAICHEUR_TRACE_SECS => {
                    anomalies.push(format!("{name} : derniere ligne il y a {} (> 2 h)", fmt_duree(age)))
                }
                None => anomalies.push(format!("{name} : illisible")),
                _ => {}
            }
        }
        // Sa propre trace : surveillee par la couche native, pas par N2
        // lui-meme — ici simple constat, jamais une auto-anomalie.
        if cycle > 1 {
            if let Some(age) = file_age_secs(&self.paths.trace, now_unix) {
                if age > SEUIL_FRAICHEUR_TRACE_SECS {
                    anomalies.push(format!("trace N2 : derniere ligne il y a {} (> 2 h)", fmt_duree(age)));
                }
            }
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("W5", "traces < 2 h (dead-man tenu)")
        } else {
            ItemVerdict::anomalie("W5", anomalies.join(" ; "))
        }
    }

    // W6 : fraicheur des fichiers (mesure simple, sans jugement).
    fn w6_fichiers(&self) -> ItemVerdict {
        match count_fresh(&self.paths.workspace_root, 3, 24 * 3600) {
            Ok(n) => ItemVerdict::ok("W6", format!("{n} fichier(s) < 24 h (profondeur 3) — niveau constate")),
            Err(e) => ItemVerdict::anomalie("W6", format!("parcours impossible : {e}")),
        }
    }

    // W7 : activite des porteurs des dispatches ouverts.
    fn w7_porteurs(&self, now_unix: i64) -> ItemVerdict {
        let text = match self.read_text(&self.paths.registry) {
            Ok(t) => t,
            Err(e) => return ItemVerdict::anomalie("W7", format!("registre illisible : {e}")),
        };
        let bearers: HashMap<String, String> = fs::read_to_string(&self.paths.bearers_map)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let date = crate::timefmt::iso8601_edt(now_unix);
        let journal = fs::read_to_string(self.paths.journal_dir.join(format!("{}.md", &date[..10])))
            .unwrap_or_default();
        let mut anomalies: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for line in text.lines() {
            let t = line.trim_start();
            if !t.starts_with("| D-N") {
                continue;
            }
            let cells: Vec<&str> = t.split('|').collect();
            if cells.len() < 10 {
                continue;
            }
            let statut = cells[6].trim();
            if statut != "EN COURS" && statut != "ASSIGNÉ" && statut != "ASSIGNE" {
                continue;
            }
            let id = cells[1].trim().to_string();
            let executant = cells[4].trim();
            let bearer = executant.split('(').next().unwrap_or(executant).trim().to_string();
            if seen.contains(&bearer) {
                continue;
            }
            seen.push(bearer.clone());
            let mut fresh: Option<i64> = None;
            if let Some(rel) = bearers.get(&bearer) {
                let p = self.home.join(rel);
                if let Some(age) = file_age_secs(&p, now_unix) {
                    fresh = Some(age);
                }
            }
            if journal.contains(&bearer) {
                fresh = Some(0);
            }
            match fresh {
                Some(age) if age > SEUIL_SILENCE_PORTEUR_SECS => {
                    anomalies.push(format!("{bearer} (porteur {id}) : silence {} (> 24 h)", fmt_duree(age)))
                }
                None => anomalies.push(format!("{bearer} (porteur {id}) : aucune activite lisible")),
                _ => {}
            }
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("W7", format!("{} porteur(s) de dispatches ouverts actif(s)", seen.len()))
        } else {
            ItemVerdict::anomalie("W7", anomalies.join(" ; "))
        }
    }

    // ---------- Surfaces internes ----------

    // File des UNKNOWN : consommee telle que posee (sans seuil dans le
    // store) ; le seuil d'age est celui de la spec, applique par le veilleur.
    fn uq_file_unknown(&self, now_unix: i64) -> ItemVerdict {
        let rows = match self.store.lock() {
            Ok(s) => s.wg_pending_unknown(),
            Err(e) => return ItemVerdict::anomalie("UQ", format!("verrou store : {e}")),
        };
        let mut anomalies: Vec<String> = Vec::new();
        for (mission, hop, attempted) in &rows {
            let age = attempted.map(|t| now_unix - t).unwrap_or(i64::MAX);
            if age > SEUIL_VIEILLISSEMENT_UNKNOWN_SECS {
                anomalies.push(format!(
                    "UNKNOWN {mission}/{hop} age {} (> 12 h) — source store",
                    fmt_duree(age.min(99_999_999))
                ));
            }
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("UQ", format!("{} UNKNOWN(s) en file, aucun vieilli > 12 h", rows.len()))
        } else {
            ItemVerdict::anomalie("UQ", anomalies.join(" ; "))
        }
    }

    // Etats de missions et de baux : cales sans crash -> rescue ; holders
    // morts / claims orphelins -> recovery. N2 ne mute jamais la mission.
    fn ls_baux_missions(&self, now_unix: i64) -> ItemVerdict {
        let (leases, transitions) = match self.store.lock() {
            Ok(s) => (s.live_leases_snapshot(), s.mission_last_transition()),
            Err(e) => return ItemVerdict::anomalie("LS", format!("verrou store : {e}")),
        };
        let mut anomalies: Vec<String> = Vec::new();
        for (mission, holder, expires_unix, claimed_unix) in &leases {
            let last_tx = transitions.get(mission).copied().unwrap_or(*claimed_unix);
            let idle = now_unix - last_tx;
            let interval = self.interval_secs as i64;
            if *expires_unix > now_unix {
                // Bail vivant : cale sans crash si aucune progression sur
                // 2 cycles de veille consecutifs.
                if idle > CYCLES_CALE_SANS_CRASH * interval {
                    let req = json!({
                        "at": crate::timefmt::iso8601_edt(now_unix),
                        "classe": "rescue",
                        "objet": format!("mission {mission}"),
                        "decideur": "Doctor S2 de la lane",
                        "faits": format!("bail tenu par {holder} sans progression depuis {}", fmt_duree(idle)),
                        "motif": "cale sans crash (2x)",
                    });
                    self.queue_routing(req);
                    anomalies.push(format!("{mission} : cale sans crash — rescue constitue ({holder})"));
                }
            } else {
                // Bail expire : holder mort ou claim orphelin -> recovery,
                // sauf 3e echec repete -> rescue (semantique H-03-A1 signee).
                let key = format!("veille_recoveries_{mission}");
                let n: i64 = match self.store.lock() {
                    Ok(s) => {
                        let n: i64 = s.meta_get(&key).and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
                        let _ = s.meta_set_pub(&key, &n.to_string());
                        n
                    }
                    Err(_) => 1,
                };
                if n >= RECOVERIES_AVANT_RESCUE {
                    let req = json!({
                        "at": crate::timefmt::iso8601_edt(now_unix),
                        "classe": "rescue",
                        "objet": format!("mission {mission}"),
                        "decideur": "Doctor S2 de la lane",
                        "faits": format!("{n} recoveries routees sans resolution (bail expire, {holder})"),
                        "motif": "echec de recovery repete (3x)",
                    });
                    self.queue_routing(req);
                    anomalies.push(format!("{mission} : {n} recoveries sans resolution — rescue ({holder})"));
                } else {
                    let req = json!({
                        "at": crate::timefmt::iso8601_edt(now_unix),
                        "classe": "recovery",
                        "objet": format!("mission {mission}"),
                        "porteur": holder,
                        "faits": "bail expire sans reconciliation",
                        "tentative": n,
                    });
                    self.queue_routing(req);
                    anomalies.push(format!("{mission} : bail expire — recovery a router ({holder}, tentative {n})"));
                }
            }
        }
        if anomalies.is_empty() {
            ItemVerdict::ok("LS", format!("{} bail/baux vivant(s), aucune cale", leases.len()))
        } else {
            ItemVerdict::anomalie("LS", anomalies.join(" ; "))
        }
    }

    // Etat du daemon lui-meme.
    fn ds_etat_daemon(&self, snap: &DaemonSnapshot) -> ItemVerdict {
        if snap.store_integrity != "OK" {
            return ItemVerdict::anomalie("DS", format!("integrite du store : {}", snap.store_integrity));
        }
        ItemVerdict::ok(
            "DS",
            format!("stade {}, store OK, ancrage {}", snap.stage, snap.anchor_mechanism),
        )
    }

    // ---------- Stade B : concordance ----------

    fn etape_concordance(
        &self,
        now_unix: i64,
        at_iso: &str,
        verdicts: &[ItemVerdict],
    ) -> Result<(i64, Option<String>), String> {
        let store = self.store.lock().map_err(|e| format!("veille: verrou store: {e}"))?;
        let mut concordant = store.veille_concordant();
        let mut window_start = store.veille_window_start();
        drop(store);

        let interim = self.interim_verdicts(now_unix)?;
        let n2: HashMap<&str, &str> =
            verdicts.iter().map(|v| (v.item, v.verdict.as_str())).collect();
        let mut discordances: Vec<Value> = Vec::new();
        let mut comparable = false;
        if let Some((iline_at, ref iv)) = interim {
            if (now_unix - iline_at).abs() <= 1800 {
                comparable = true;
                for item in ["W1", "W2", "W3", "W4", "W5", "W6", "W7"] {
                    let a = n2.get(item).copied().unwrap_or("?");
                    let b = iv.get(item).map(|s| s.as_str()).unwrap_or("?");
                    if a != "?" && b != "?" && a != b {
                        discordances.push(json!({
                            "at": at_iso, "item": item,
                            "n2": a, "interimaire": b,
                            "cause": "a nommer — investigation sur l'entree (design §0.3)",
                        }));
                    }
                }
            }
        }

        let store = self.store.lock().map_err(|e| format!("veille: verrou store: {e}"))?;
        if !comparable {
            // Cycle non comparable : ni compte ni reset, note au rapport.
            let w = window_start.map(crate::timefmt::iso8601_edt);
            return Ok((concordant, w));
        }
        let mut per_item: HashMap<String, (String, String)> = HashMap::new();
        if let Some((_, ref iv)) = interim {
            for item in ["W1", "W2", "W3", "W4", "W5", "W6", "W7"] {
                let a = n2.get(item).copied().unwrap_or("?").to_string();
                let b = iv.get(item).map(|s| s.as_str()).unwrap_or("?").to_string();
                per_item.insert(item.to_string(), (a, b));
            }
        }
        if discordances.is_empty() {
            if window_start.is_none() {
                window_start = Some(now_unix);
                store.set_veille_window_start(now_unix).map_err(|e| format!("veille: {e}"))?;
            }
            concordant += 1;
            store.set_veille_concordant(concordant).map_err(|e| format!("veille: {e}"))?;
        } else {
            // Toute discordance remet le compteur consecutif a zero
            // (design §0.3) ; la fenetre recommence.
            concordant = 0;
            window_start = Some(now_unix);
            store.set_veille_concordant(0).map_err(|e| format!("veille: {e}"))?;
            store.set_veille_window_start(now_unix).map_err(|e| format!("veille: {e}"))?;
            for d in &discordances {
                self.append_trace(&format!(
                    "- **{at_iso}** — anomalie DU RELAIS (stade B) : {d} · source {TRACE_SOURCE}"
                ))?;
            }
        }
        self.save_concordance_report(now_unix, concordant, window_start, comparable, true, &discordances, &per_item)?;
        Ok((concordant, window_start.map(crate::timefmt::iso8601_edt)))
    }

    /// Derniers verdicts par item de l'interimaire (sa trace).
    fn interim_verdicts(&self, _now: i64) -> Result<Option<(i64, HashMap<String, String>)>, String> {
        let text = self.read_text(&self.paths.interim_trace).unwrap_or_default();
        let mut last: Option<(i64, String)> = None;
        for line in text.lines() {
            if line.trim_start().starts_with("- **") {
                if let Some(ts) = parse_fr_datetime(line) {
                    last = Some((ts, line.to_string()));
                }
            }
        }
        let (ts, line) = match last {
            Some(x) => x,
            None => return Ok(None),
        };
        let mut items: HashMap<String, String> = HashMap::new();
        for n in 1..=7 {
            let key = format!("W{n} ");
            if let Some(i) = line.find(&key) {
                let rest = &line[i + key.len()..];
                let tok: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '(' && *c != '·').collect();
                items.insert(format!("W{n}"), if tok.starts_with("OK") { "OK".into() } else { "ANOMALIE".into() });
            }
        }
        Ok(Some((ts, items)))
    }

    fn save_concordance_report(
        &self,
        now_unix: i64,
        concordant: i64,
        window_start: Option<i64>,
        comparable: bool,
        concordant_cycle: bool,
        discordances: &[Value],
        per_item: &HashMap<String, (String, String)>,
    ) -> Result<(), String> {
        let mut report: Value = fs::read_to_string(&self.paths.concordance_report)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({"cycles": [], "discordances": []}));
        if let Some(arr) = report.get_mut("cycles").and_then(|c| c.as_array_mut()) {
            arr.push(json!({
                "at": crate::timefmt::iso8601_edt(now_unix),
                "comparable": comparable,
                "concordant": concordant_cycle && discordances.is_empty(),
                "par_item": per_item.iter().map(|(k, (a, b))| {
                    json!({"item": k, "n2": a, "interimaire": b, "egaux": a == b})
                }).collect::<Vec<_>>(),
            }));
        }
        if let Some(arr) = report.get_mut("discordances").and_then(|d| d.as_array_mut()) {
            for d in discordances {
                arr.push(d.clone());
            }
        }
        report["window_start"] = window_start.map(crate::timefmt::iso8601_edt).into();
        report["concordant_consecutifs"] = concordant.into();
        report["fenetre_requise"] =
            json!({"jours_min": FENETRE_JOURS_MIN, "cycles_min": FENETRE_CYCLES_MIN});
        if let Some(parent) = self.paths.concordance_report.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("veille: rapport: {e}"))?;
        }
        fs::write(&self.paths.concordance_report, serde_json::to_string_pretty(&report).unwrap())
            .map_err(|e| format!("veille: rapport: {e}"))?;
        Ok(())
    }
}

// ---------- Actes d'operateur (mecaniquement gates) ----------

/// Bascule (stade C) : refusee sans rapport de concordance complet
/// (H-P4-11). L'acte ecrit t2_ready_at exactement une fois (H-P4-12).
pub fn veille_switchover(store: &Store, paths: &VeillePaths, now_unix: i64) -> Result<String, String> {
    if store.veille_stage() != "B" {
        return Err(format!(
            "BASCULE_REFUSEE: stade {} — la bascule exige le stade B et un rapport complet (H-P4-11)",
            store.veille_stage()
        ));
    }
    let report: Value = fs::read_to_string(&paths.concordance_report)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or("BASCULE_REFUSEE: aucun rapport de concordance (H-P4-11)")?;
    let concordant = report.get("concordant_consecutifs").and_then(|c| c.as_i64()).unwrap_or(0);
    let window_start = report
        .get("window_start")
        .and_then(|w| w.as_str())
        .and_then(parse_fr_datetime)
        .ok_or("BASCULE_REFUSEE: debut de fenetre absent du rapport")?;
    let jours = (now_unix - window_start) / 86_400;
    if jours < FENETRE_JOURS_MIN {
        return Err(format!("BASCULE_REFUSEE: fenetre {jours} j < {FENETRE_JOURS_MIN} j (H-P4-11)"));
    }
    if concordant < FENETRE_CYCLES_MIN {
        return Err(format!(
            "BASCULE_REFUSEE: {concordant} cycles concordants < {FENETRE_CYCLES_MIN} (H-P4-11)"
        ));
    }
    let sans_cause = report
        .get("discordances")
        .and_then(|d| d.as_array())
        .map(|ds| {
            ds.iter().any(|d| {
                d.get("cause")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("a nommer"))
                    .unwrap_or(true)
            })
        })
        .unwrap_or(false);
    if sans_cause {
        return Err("BASCULE_REFUSEE: discordances sans cause nommee (H-P4-11)".into());
    }
    // H-P4-11 : chaque cycle du rapport est comparable, concordant, et les
    // verdicts sont egaux par item W1->W7.
    let cycles = report.get("cycles").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    let mut cycles_verifies = 0i64;
    for c in &cycles {
        let comparable = c.get("comparable").and_then(|x| x.as_bool()).unwrap_or(false);
        let conc = c.get("concordant").and_then(|x| x.as_bool()).unwrap_or(false);
        let items_egaux = c
            .get("par_item")
            .and_then(|p| p.as_array())
            .map(|ps| !ps.is_empty() && ps.iter().all(|p| p.get("egaux").and_then(|e| e.as_bool()).unwrap_or(false)))
            .unwrap_or(false);
        if comparable && conc && items_egaux {
            cycles_verifies += 1;
        }
    }
    if cycles_verifies < FENETRE_CYCLES_MIN {
        return Err(format!(
            "BASCULE_REFUSEE: {cycles_verifies} cycles verifies par item < {FENETRE_CYCLES_MIN} (H-P4-11)"
        ));
    }
    let at = crate::timefmt::iso8601_edt(now_unix);
    // Ecriture unique, mecanique (H-P4-12) : le store refuse la seconde.
    store.set_t2_ready_at(&at)?;
    store.set_veille_stage("C")?;
    let line = format!(
        "- **{at}** — BASCULE CONSIGNEE : relais accompli, N2 devient l'autorite de veille · t2_ready_at={at} · source {TRACE_SOURCE} · recu R10 a emettre par l'operateur"
    );
    if let Some(parent) = paths.trace.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("veille: bascule: {e}"))?;
    }
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.trace)
        .map_err(|e| format!("veille: bascule: {e}"))?;
    writeln!(f, "{line}").map_err(|e| format!("veille: bascule: {e}"))?;
    Ok(json!({
        "acte": "bascule",
        "at": at,
        "t2_ready_at": at,
        "stade": "C",
        "note": "traces restantes : ligne au registre (Nardole), mise en veille de l'interimaire (A0) — actes externes, non executes ici",
    })
    .to_string())
}

/// Rollback : acte consigne qui replace N2 en ombre (stade A) ; le rearmement
/// de l'interimaire est l'acte externe symetrique (H-P4-13).
pub fn veille_rollback(store: &Store, paths: &VeillePaths, now_unix: i64) -> Result<String, String> {
    let stade = store.veille_stage();
    if stade != "C" && stade != "D" {
        return Err(format!("ROLLBACK_REFUSE: stade {stade} — rien a retourner"));
    }
    store.set_veille_stage("A")?;
    store.set_veille_concordant(0)?;
    let at = crate::timefmt::iso8601_edt(now_unix);
    // Le rollback invalide le rapport de concordance : toute bascule
    // ulterieure exige une fenetre neuve et entiere (design §0.7, H-P4-13).
    let _ = fs::write(
        &paths.concordance_report,
        serde_json::to_string_pretty(&json!({
            "invalidated_by_rollback_at": at,
            "cycles": [],
            "discordances": [],
            "concordant_consecutifs": 0,
        }))
        .unwrap(),
    );
    let line = format!(
        "- **{at}** — ROLLBACK CONSIGNE : N2 replace en ombre (stade A) · toute bascule ulterieure exige une fenetre de concordance neuve et entiere · source {TRACE_SOURCE}"
    );
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.trace)
        .map_err(|e| format!("veille: rollback: {e}"))?;
    writeln!(f, "{line}").map_err(|e| format!("veille: rollback: {e}"))?;
    Ok(json!({"acte": "rollback", "at": at, "stade": "A"}).to_string())
}

// ---------- Utilitaires ----------

fn parse_fr_datetime(s: &str) -> Option<i64> {
    // "2026-10-23 12:19 EDT", "2026-10-23 ~13:40 EDT", ou en gras
    // "**2026-10-23 12:19 EDT**" (lignes de trace).
    let s = s.replace('~', " ").replace('*', " ");
    let parts: Vec<&str> = s.split_whitespace().collect();
    for w in parts.windows(3) {
        if w[0].len() == 10 && w[0].chars().nth(4) == Some('-') && w[2] == "EDT" {
            let d: Vec<&str> = w[0].split('-').collect();
            let t: Vec<&str> = w[1].split(':').collect();
            if d.len() == 3 && t.len() >= 2 {
                let (y, m, day) = (d[0].parse().ok()?, d[1].parse().ok()?, d[2].parse().ok()?);
                let (hh, mm): (i64, i64) = (t[0].parse().ok()?, t[1].parse().ok()?);
                return Some(days_to_unix(y, m, day) + (hh + 4) * 3600 + mm * 60);
            }
        }
    }
    None
}

fn parse_iso8601(s: &str) -> Option<i64> {
    // "2026-10-22T06:53:27Z"
    if s.len() < 19 {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    let hh: i64 = s[11..13].parse().ok()?;
    let mm: i64 = s[14..16].parse().ok()?;
    let ss: i64 = s[17..19].parse().ok()?;
    Some(days_to_unix(y, m, d) + hh * 3600 + mm * 60 + ss)
}

fn days_to_unix(y: i64, m: i64, d: i64) -> i64 {
    // Jours civils -> secondes UNIX (algorithme inverse du module timefmt).
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    ((era * 146097 + doe as i64) - 719468) * 86_400
}

fn extract_dispatch_ids(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = s[i..].find("D-N") {
        let j = i + p + 3;
        let num: String = s[j..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            out.push(format!("D-N{num}"));
        }
        i = j + num.len().max(1);
    }
    out
}

fn extract_pr_refs(s: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = s[i..].find("pull/") {
        let j = i + p + 5;
        let num: String = s[j..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<i64>() {
            out.push(n);
        }
        i = j + num.len().max(1);
    }
    out
}

fn extract_version(s: &str) -> Option<String> {
    for w in s.split(|c: char| !c.is_alphanumeric() && c != '.') {
        if w.starts_with('v') && w[1..].chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            return Some(w.to_string());
        }
    }
    None
}

fn file_age_secs(p: &std::path::Path, now_unix: i64) -> Option<i64> {
    let m = fs::metadata(p).ok()?;
    let t = m.modified().ok()?;
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(now_unix - secs)
}

fn count_fresh(root: &std::path::Path, maxdepth: u32, within_secs: i64) -> Result<usize, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut n = 0usize;
    let mut stack: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > maxdepth {
            continue;
        }
        let entries = fs::read_dir(&dir).map_err(|e| format!("veille: parcours {}: {e}", dir.display()))?;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push((p, depth + 1));
            } else if let Some(age) = file_age_secs(&p, now) {
                if age < within_secs {
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

fn sha256_hex(bytes: &[u8]) -> String {
    // SHA-256 minimal, sans dependance (usage : empreintes W3).
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g; g = f; f = e; e = d.wrapping_add(t1);
            d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }
    let mut out = String::with_capacity(64);
    for x in h {
        out.push_str(&format!("{x:08x}"));
    }
    out
}

fn fmt_duree(secs: i64) -> String {
    let s = secs.max(0);
    if s < 3600 {
        format!("{} min", s / 60)
    } else if s < 86_400 {
        format!("{} h {:02} min", s / 3600, (s % 3600) / 60)
    } else {
        format!("{} j {} h", s / 86_400, (s % 86_400) / 3600)
    }
}
