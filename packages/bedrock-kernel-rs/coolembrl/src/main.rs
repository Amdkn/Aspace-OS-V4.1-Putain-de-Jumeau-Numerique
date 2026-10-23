//! noyau — daemon du Noyau A1 (CoolEmbrl 1.0), Phase 1.
//!
//! Sequence de boot (design signe, section 2.1) :
//!   S0 process-start : journal de boot append-only ouvert, boot_id emis.
//!   S1 stratum-guard : le store vise est-il sur une strate persistante
//!                      detectable ? Non -> FAILED(stratum), arret.
//!   S2 store-open    : store durable (SQLite WAL) + controle d'integrite.
//!   S3 replay        : rejeu de l'etat canonique ; les effets inconnus sont
//!                      COMPTES au recu, jamais resolus par hypothese.
//!   S4 anchor-arm    : l'ancrage d'enveloppe passe en deny-by-default.
//!   S5 surfaces-up   : API + serveur MCP leves ; enregistrement H-04.
//!   S6 self-probes   : API repond ET MCP repond ET l'ancrage refuse la
//!                      mutation-sonde. Echec -> FAILED(probe-<surface>).
//!   -> T1 DECLARE sur verification, jamais sur minuterie.
//!
//! Code propre (clean-room) : concu depuis le PRD et le design signes du
//! programme ; aucune reprise de code d'un daemon tiers, aucun identifiant
//! de marque tierce (H-P1-5 / H-P1-11).

mod envelope;
mod journal;
mod policy;
mod server;
mod store;
mod timefmt;
mod veille;
mod workgraph;

use envelope::EnvelopeService;
use journal::Journal;
use serde_json::{json, Value};
use server::{DaemonState, ServerCtx};
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use store::Store;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
const CLOCK_FLOOR_UNIX: i64 = 1_717_200_000; // 2024-06-01 — en dessous : horloge manifestement incoherente

struct Config {
    data_dir: PathBuf,
    api_bind: String,
    mcp_bind: String,
    board_id: String,
    l00_trace: Option<PathBuf>,
    registry_path: Option<PathBuf>,
    policy_dir: Option<PathBuf>,
    heartbeat_secs: u64,
    // Phase 4 — service veille (N2).
    veille_interval_secs: u64,
    veille_trace: Option<PathBuf>,
    veille_outbox: Option<PathBuf>,
    veille_report: Option<PathBuf>,
    veille_registry: Option<PathBuf>,
    veille_interim_trace: Option<PathBuf>,
    veille_journal_dir: Option<PathBuf>,
    veille_workspace: Option<PathBuf>,
    veille_bearers: Option<PathBuf>,
    veille_gh_stub: Option<PathBuf>,
    veille_spec_courante: Option<PathBuf>,
}

impl Config {
    fn from_env() -> Config {
        let env_or = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        Config {
            data_dir: PathBuf::from(env_or("NOYAU_DATA_DIR", "./noyau-data")),
            api_bind: env_or("NOYAU_API_BIND", "127.0.0.1:7474"),
            mcp_bind: env_or("NOYAU_MCP_BIND", "127.0.0.1:7475"),
            board_id: env_or("NOYAU_BOARD_ID", "amd64-generic"),
            l00_trace: std::env::var("NOYAU_L00_TRACE").ok().map(PathBuf::from),
            registry_path: std::env::var("NOYAU_REGISTRY_PATH").ok().map(PathBuf::from),
            policy_dir: std::env::var("NOYAU_POLICY_DIR").ok().map(PathBuf::from),
            heartbeat_secs: std::env::var("NOYAU_HEARTBEAT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            veille_interval_secs: std::env::var("NOYAU_VEILLE_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(veille::INTERVAL_SECS_DEFAULT),
            veille_trace: std::env::var("NOYAU_VEILLE_TRACE").ok().map(PathBuf::from),
            veille_outbox: std::env::var("NOYAU_VEILLE_OUTBOX").ok().map(PathBuf::from),
            veille_report: std::env::var("NOYAU_VEILLE_REPORT").ok().map(PathBuf::from),
            veille_registry: std::env::var("NOYAU_VEILLE_REGISTRY").ok().map(PathBuf::from),
            veille_interim_trace: std::env::var("NOYAU_VEILLE_INTERIM_TRACE").ok().map(PathBuf::from),
            veille_journal_dir: std::env::var("NOYAU_VEILLE_JOURNAL_DIR").ok().map(PathBuf::from),
            veille_workspace: std::env::var("NOYAU_VEILLE_WORKSPACE").ok().map(PathBuf::from),
            veille_bearers: std::env::var("NOYAU_VEILLE_BEARERS").ok().map(PathBuf::from),
            veille_gh_stub: std::env::var("NOYAU_VEILLE_GH_STUB").ok().map(PathBuf::from),
            veille_spec_courante: std::env::var("NOYAU_VEILLE_SPEC_COURANTE").ok().map(PathBuf::from),
        }
    }
}

/// Chemins du service veille resolus depuis la Config (defauts : les
/// emplacements reels du programme sous $HOME).
fn veille_paths(cfg: &Config) -> veille::VeillePaths {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let h = |rel: &str| PathBuf::from(&home).join(rel);
    let policy_dir = cfg
        .policy_dir
        .clone()
        .unwrap_or_else(|| h("workspace/v4-capabilities/policy"));
    veille::VeillePaths {
        registry: cfg.veille_registry.clone().unwrap_or_else(|| h("workspace/v4-factory/dispatch/REGISTRE-NARDOLE.md")),
        interim_trace: cfg.veille_interim_trace.clone().unwrap_or_else(|| h("workspace/v4-factory/dispatch/L00-ETAT.md")),
        trace: cfg.veille_trace.clone().unwrap_or_else(|| h("workspace/v4-factory/dispatch/L00-N2-ETAT.md")),
        outbox: cfg.veille_outbox.clone().unwrap_or_else(|| cfg.data_dir.join("veille-outbox.jsonl")),
        concordance_report: cfg.veille_report.clone().unwrap_or_else(|| cfg.data_dir.join("veille-concordance.json")),
        policy_dir,
        journal_dir: cfg.veille_journal_dir.clone().unwrap_or_else(|| h("memory")),
        workspace_root: cfg.veille_workspace.clone().unwrap_or_else(|| h("workspace")),
        bearers_map: cfg.veille_bearers.clone().unwrap_or_else(|| cfg.data_dir.join("veille-bearers.json")),
        spec_courante: cfg.veille_spec_courante.clone(),
    }
}

fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Type de systeme de fichiers du repertoire vise (statvfs, magic Linux).
fn fs_magic(dir: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    Some(st.f_type as u64)
}

const TMPFS_MAGIC: u64 = 0x0102_1994;
const RAMFS_MAGIC: u64 = 0x8584_58f6;

/// t2_ready_at porte au recu de boot : null jusqu'a la bascule, l'horodatage
/// de l'acte ensuite (H-P4-9). Un recu pre-bascule le portant non nul est un
/// echec de hold-out, pas une avance (design section 0.5 : ecriture unique,
/// mecanique).
fn t2_value(store: &Option<Arc<Mutex<store::Store>>>) -> Value {
    store
        .as_ref()
        .and_then(|s| s.lock().ok())
        .and_then(|s| s.t2_ready_at())
        .map(Value::String)
        .unwrap_or(Value::Null)
}

/// Etat du watchdog N2 porte au recu de boot et a l'etat du daemon.
fn watchdog_boot_json(store: &Option<Arc<Mutex<store::Store>>>) -> Value {
    let (stade, spec) = store
        .as_ref()
        .and_then(|s| s.lock().ok())
        .map(|s| {
            (
                s.veille_stage(),
                s.veille_spec_version().unwrap_or_else(|| "non posee".into()),
            )
        })
        .unwrap_or(("?".into(), "?".into()));
    json!({
        "service": "veille",
        "source_trace": veille::TRACE_SOURCE,
        "spec_version": spec,
        "stade": stade,
        "autoritaire": stade == "C" || stade == "D",
    })
}

fn main() {
    // Acte d'activation de l'ancrage (Phase 2, design 0.1) : le mecanisme
    // ne se declare ACTIVE qu'apres ses preuves — cet acte d'operateur,
    // trace au store, est la seule voie d'activation.
    if std::env::args().nth(1).as_deref() == Some("anchor-activate") {
        let cfg = Config::from_env();
        let store_path = cfg.data_dir.join("noyau-store.sqlite");
        match Store::open(&store_path) {
            Ok(store) => {
                let now = timefmt::now_unix_secs();
                match store.set_anchor_mechanism("ACTIVE") {
                    Ok(()) => {
                        let _ = store.trace(
                            "ANCHOR_ACTIVATED",
                            None,
                            None,
                            None,
                            &json!({"act": "anchor-activate", "by": "operateur local"}).to_string(),
                            now,
                        );
                        println!("noyau: ancrage ACTIVE — acte trace au store ({}).", store_path.display());
                        return;
                    }
                    Err(e) => {
                        eprintln!("noyau: anchor-activate FAILED — {e}");
                        std::process::exit(2);
                    }
                }
            }
            Err(e) => {
                eprintln!("noyau: anchor-activate FAILED — store illisible: {e}");
                std::process::exit(2);
            }
        }
    }
    // Cycle unique de veille (Phase 4) : surface d'operateur pour les
    // fixtures H-P4 — execute un cycle reel contre le binaire et emet le
    // rapport en JSON. La configuration passe par l'environnement
    // (NOYAU_VEILLE_*), comme en production.
    if std::env::args().nth(1).as_deref() == Some("veille-cycle-once") {
        let cfg = Config::from_env();
        let store_path = cfg.data_dir.join("noyau-store.sqlite");
        match Store::open(&store_path) {
            Ok(store) => {
                let paths = veille_paths(&cfg);
                let slot = Arc::new(Mutex::new(None));
                let v = veille::Veille::new(
                    Arc::new(Mutex::new(store)),
                    paths,
                    cfg.veille_interval_secs,
                    cfg.veille_gh_stub.clone(),
                    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())),
                    slot,
                );
                let snap = veille::DaemonSnapshot {
                    stage: "T1".into(),
                    store_integrity: "OK".into(),
                    anchor_mechanism: "ABSENT".into(),
                };
                match v.run_cycle(timefmt::now_unix_secs(), &snap) {
                    Ok(r) => {
                        println!("{}", r.to_json());
                        return;
                    }
                    Err(e) => {
                        eprintln!("noyau: veille-cycle-once FAILED — {e}");
                        std::process::exit(2);
                    }
                }
            }
            Err(e) => {
                eprintln!("noyau: veille-cycle-once FAILED — store illisible: {e}");
                std::process::exit(2);
            }
        }
    }
    // et le rollback sont des actes d'operateur mecaniquement gates — jamais
    // des glissements. La bascule exige un rapport de concordance complet
    // (H-P4-11) ; elle ecrit t2_ready_at exactement une fois (H-P4-12).
    if let Some(cmd) = std::env::args().nth(1) {
        if cmd == "veille-switchover" || cmd == "veille-rollback" {
            let cfg = Config::from_env();
            let store_path = cfg.data_dir.join("noyau-store.sqlite");
            match Store::open(&store_path) {
                Ok(store) => {
                    let paths = veille_paths(&cfg);
                    let now = timefmt::now_unix_secs();
                    let res = if cmd == "veille-switchover" {
                        veille::veille_switchover(&store, &paths, now)
                    } else {
                        veille::veille_rollback(&store, &paths, now)
                    };
                    match res {
                        Ok(receipt) => {
                            let _ = store.trace(
                                if cmd == "veille-switchover" { "VEILLE_SWITCHOVER" } else { "VEILLE_ROLLBACK" },
                                None, None, None, &receipt, now,
                            );
                            println!("noyau: {cmd} OK — {receipt}");
                            return;
                        }
                        Err(e) => {
                            eprintln!("noyau: {cmd} REFUSE — {e}");
                            std::process::exit(2);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("noyau: {cmd} FAILED — store illisible: {e}");
                    std::process::exit(2);
                }
            }
        }
    }
    let boot_start = Instant::now();
    let cfg = Config::from_env();
    let t0_unix = timefmt::now_unix_secs();
    let t0_at = timefmt::iso8601_utc(t0_unix);
    let boot_id = format!("boot-{}-{}", t0_at.replace(['-', ':', 'T', 'Z'], ""), random_hex(4));

    let mut anomalies: Vec<String> = Vec::new();

    // ---------- S0 process-start : journal de boot (etage 0) ----------
    if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
        eprintln!("noyau: FAILED(process-start) — creation du volume du daemon: {e}");
        std::process::exit(2);
    }
    let journal_path = cfg.data_dir.join("boot-journal.jsonl");
    let journal = match Journal::open(&journal_path) {
        Ok(j) => Arc::new(j),
        Err(e) => {
            // Volume indisponible : sortie en erreur non nulle (journal systemd).
            eprintln!("noyau: FAILED(process-start) — {e}");
            std::process::exit(2);
        }
    };
    // Chaine des boots + horloge : lire l'etat du journal avant ce boot.
    let (prev_boot_id, prev_t0_unix) = read_previous_boot(&journal_path);
    if t0_unix < CLOCK_FLOOR_UNIX {
        anomalies.push(format!(
            "clock-incoherent: horloge de l'hote sous le plancher ({t0_at})"
        ));
    }
    if let Some(p) = prev_t0_unix {
        if t0_unix < p {
            anomalies.push(format!(
                "clock-incoherent: t0 du boot courant ({t0_at}) anterieur au boot precedent"
            ));
        }
    }
    if let Some(prev) = journal.previous_boot_unterminated(&boot_id) {
        anomalies.push(format!(
            "previous-boot-unterminated: {prev} — recuperation constatee au boot courant"
        ));
    }
    journal.append(&json!({
        "kind": "stage", "boot_id": boot_id, "stage": "S0 process-start",
        "at": t0_at, "t0_unix": t0_unix, "daemon_version": DAEMON_VERSION,
        "board_id": cfg.board_id, "prev_boot_id": prev_boot_id,
    }));

    let state = Arc::new(RwLock::new(DaemonState {
        boot_id: boot_id.clone(),
        daemon_version: DAEMON_VERSION.to_string(),
        board_id: cfg.board_id.clone(),
        stage: "T0".to_string(),
        t0_unix,
        stage_marks: vec![("S0 process-start".to_string(), t0_at.clone())],
        store_integrity: "UNKNOWN".to_string(),
        last_receipt: None,
        anomalies: anomalies.clone(),
    }));

    let mut ctx = BootCtx {
        cfg: &cfg,
        journal: journal.clone(),
        state: state.clone(),
        boot_id: boot_id.clone(),
        t0_at: t0_at.clone(),
        t0_unix,
        prev_boot_id: prev_boot_id.clone(),
        anomalies,
        store: None,
        replay: None,
        boot_start,
    };

    // ---------- S1 stratum-guard ----------
    let magic = fs_magic(&cfg.data_dir);
    match magic {
        Some(m) if m == TMPFS_MAGIC || m == RAMFS_MAGIC => {
            ctx.fail(
                "stratum",
                "le store vise est sur une strate non persistante detectable (tmpfs/ramfs) — garde 4.2 du PRD",
            );
        }
        None => ctx.anomalies.push(
            "stratum-unverified: type de systeme de fichiers non lisible".to_string(),
        ),
        _ => {}
    }
    ctx.mark("S1 stratum-guard");

    // ---------- S2 store-open ----------
    let store_path = cfg.data_dir.join("noyau-store.sqlite");
    match Store::open(&store_path) {
        Ok(s) => {
            ctx.store = Some(Arc::new(Mutex::new(s)));
            ctx.state.write().unwrap().store_integrity = "OK".to_string();
        }
        Err(detail) => {
            ctx.state.write().unwrap().store_integrity = "FAILED".to_string();
            ctx.fail("store-integrity", &detail);
        }
    }
    ctx.mark("S2 store-open");

    // ---------- S3 replay ----------
    {
        let store = ctx.store.as_ref().unwrap().lock().unwrap();
        match store.replay() {
            Ok(r) => ctx.replay = Some((r.hops_replayed, r.effects_unknown_pending)),
            Err(detail) => {
                drop(store);
                ctx.fail("replay", &detail);
            }
        }
    }
    ctx.mark("S3 replay");

    // ---------- S4 anchor-arm ----------
    // L'ancrage est arme en deny-by-default des la construction de l'etat
    // (AnchorState::phase1) ; S4 le consigne.
    ctx.mark("S4 anchor-arm");

    // ---------- S5 surfaces-up ----------
    let api_listener = match TcpListener::bind(&cfg.api_bind) {
        Ok(l) => l,
        Err(e) => ctx.fail("surfaces", &format!("liaison API {}: {e}", cfg.api_bind)),
    };
    let mcp_listener = TcpListener::bind(&cfg.mcp_bind).ok(); // echec differe a la sonde S6 (H-P1-12)
    if mcp_listener.is_none() {
        ctx.anomalies.push(format!(
            "mcp-bind-failed: liaison MCP {} impossible a S5 — la sonde S6 tranchera",
            cfg.mcp_bind
        ));
    }
    let envelope_service = Arc::new(EnvelopeService {
        store: ctx.store.as_ref().unwrap().clone(),
        policy_dir: cfg.policy_dir.clone(),
    });
    let server_ctx = Arc::new(ServerCtx {
        state: state.clone(),
        journal: journal.clone(),
        envelope: envelope_service.clone(),
        workgraph: Arc::new(workgraph::WorkGraph {
            store: ctx.store.as_ref().unwrap().clone(),
            envelope: envelope_service,
        }),
        veille: Arc::new(veille::Veille::new(
            ctx.store.as_ref().unwrap().clone(),
            veille_paths(&cfg),
            cfg.veille_interval_secs,
            cfg.veille_gh_stub.clone(),
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())),
            Arc::new(Mutex::new(None)),
        )),
    });
    {
        let c = server_ctx.clone();
        std::thread::spawn(move || server::serve(api_listener, c, false));
    }
    if let Some(l) = mcp_listener {
        let c = server_ctx.clone();
        std::thread::spawn(move || server::serve(l, c, true));
    }
    // Enregistrement au registre canonique H-04 : le service s'enregistre,
    // il ne s'auto-declare pas.
    if let Some(path) = &cfg.registry_path {
        let entry = json!({
            "name": "noyau",
            "kind": "mcp-server",
            "version": DAEMON_VERSION,
            "api_bind": cfg.api_bind,
            "mcp_bind": cfg.mcp_bind,
            "mcp_tools": server::MCP_TOOLS,
            "board_id": cfg.board_id,
            "boot_id": boot_id,
            "registered_at": timefmt::iso8601_utc(timefmt::now_unix_secs()),
        });
        if let Err(e) = write_atomic(path, &entry) {
            ctx.fail("surfaces", &format!("enregistrement H-04 ({}) : {e}", path.display()));
        }
    }
    ctx.mark("S5 surfaces-up");

    // ---------- S6 self-probes ----------
    // Sonde API
    match server::http_request(&cfg.api_bind, "GET", "/health", None) {
        Ok((200, _)) => {}
        other => ctx.fail("probe-api", &format!("GET /health -> {other:?}")),
    }
    // Sonde MCP : initialize + tools/list (les 3 outils, exactement)
    let mcp_ok = (|| -> Result<(), String> {
        let (_, init) = server::http_request(
            &cfg.mcp_bind,
            "POST",
            "/",
            Some(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})),
        )?;
        let name = init
            .pointer("/result/serverInfo/name")
            .and_then(|n| n.as_str())
            .unwrap_or("");
        if name != "noyau" {
            return Err(format!("initialize: serverInfo.name = {name:?}"));
        }
        let (_, list) = server::http_request(
            &cfg.mcp_bind,
            "POST",
            "/",
            Some(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})),
        )?;
        let tools = list
            .pointer("/result/tools")
            .and_then(|t| t.as_array())
            .ok_or("tools/list illisible")?;
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        if names.len() != server::MCP_TOOLS.len()
            || !server::MCP_TOOLS.iter().all(|t| names.contains(t))
        {
            return Err(format!("tools/list inattendu: {names:?}"));
        }
        Ok(())
    })();
    if let Err(cause) = mcp_ok {
        ctx.fail("probe-mcp", &cause);
    }
    // Sonde ancrage : une requete en forme de bail doit etre refusee, typee.
    let lease_shaped = json!({"kind": "lease-request", "capability": "store.write", "ttl_s": 60});
    match server::http_request(&cfg.api_bind, "POST", "/anchor", Some(&lease_shaped)) {
        Ok((403, body)) => {
            let code = body
                .pointer("/error/code")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if code != "ANCHOR_DENY_ALL" {
                ctx.fail("probe-anchor", &format!("refus non type: {body}"));
            }
        }
        other => ctx.fail("probe-anchor", &format!("POST /anchor -> {other:?}")),
    }
    ctx.mark("S6 self-probes");

    // ---------- T1 DECLARE ----------
    let d_t1_ms = boot_start.elapsed().as_millis() as i64;
    let t1_unix = timefmt::now_unix_secs();
    let t1_at = timefmt::iso8601_utc(t1_unix);
    let (hops, unknown) = ctx.replay.unwrap_or((0, 0));
    let receipt = json!({
        "boot_id": boot_id,
        "daemon_version": DAEMON_VERSION,
        "store_schema_version": store::STORE_SCHEMA_VERSION,
        "board_id": cfg.board_id,
        "t0_at": ctx.t0_at,
        "t1_at": t1_at,
        "t2_ready_at": t2_value(&ctx.store),
        "watchdog": watchdog_boot_json(&ctx.store),
        "d_t1_ms": d_t1_ms,
        "store_integrity": "OK",
        "replay": {"hops_replayed": hops, "effects_unknown_pending": unknown},
        "outcome": "T1_REACHED",
        "prev_boot_id": ctx.prev_boot_id,
        "anomalies": ctx.anomalies,
    });
    ctx.emit_receipt(&receipt, t1_unix);
    {
        let mut st = state.write().unwrap();
        st.stage = "T1".to_string();
        st.last_receipt = Some(receipt.clone());
        st.anomalies = ctx.anomalies.clone();
    }
    println!(
        "noyau: T1 atteint — boot {boot_id}, d_t1_ms={d_t1_ms} (mesure), ancrage DENY_ALL (mecanisme ABSENT, Phase 2), veille N2 stade A — ombre (Phase 4)"
    );

    // ---------- battement interimaire vers la trace L00 ----------
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as usize as libc::sighandler_t);
    }
    let hb_ctx = (cfg.l00_trace.clone(), cfg.heartbeat_secs, boot_id.clone());
    let hb = std::thread::spawn(move || {
        let (trace, secs, bid) = hb_ctx;
        while !SHUTDOWN.load(Ordering::SeqCst) {
            // Sommeil en tranches courtes : l'arret reste prompt (< 1 s)
            // quelle que soit la periode du battement.
            let mut waited = 0u64;
            while waited < secs * 1000 && !SHUTDOWN.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(200));
                waited += 200;
            }
            if SHUTDOWN.load(Ordering::SeqCst) {
                break;
            }
            let line = format!(
                "- {} — noyau · battement — boot_id {} · stade T1 · store OK · ancrage DENY_ALL · watchdog NOT_INSTALLED",
                timefmt::iso8601_utc(timefmt::now_unix_secs()),
                bid
            );
            match &trace {
                Some(path) => {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                        let _ = writeln!(f, "{line}");
                    }
                }
                None => println!("{line}"),
            }
        }
    });

    // ---------- service veille N2 : cycles horaires ----------
    // Stade A a l'installation : l'ombre bat, ecrit sa trace, ne route rien.
    // La bascule (stade C) est un acte d'operateur (`veille-switchover`),
    // jamais un glissement du thread.
    let veille_ctx = server_ctx.veille.clone();
    let veille_state = state.clone();
    let veille_envelope = server_ctx.envelope.clone();
    let _veille_thread = std::thread::spawn(move || {
        let mut waited = 0u64;
        let interval_ms = veille_ctx.interval_secs * 1000;
        // Premier cycle immediat : le premier battement N2 existe des le boot.
        let mut first = true;
        while !SHUTDOWN.load(Ordering::SeqCst) {
            if !first {
                while waited < interval_ms && !SHUTDOWN.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    waited += 200;
                }
                waited = 0;
            }
            first = false;
            if SHUTDOWN.load(Ordering::SeqCst) {
                break;
            }
            let snap = {
                let st = veille_state.read().unwrap();
                let anchor = veille_envelope
                    .state_json()
                    .get("mechanism")
                    .and_then(|m| m.as_str())
                    .unwrap_or("?")
                    .to_string();
                veille::DaemonSnapshot {
                    stage: st.stage.clone(),
                    store_integrity: st.store_integrity.clone(),
                    anchor_mechanism: anchor,
                }
            };
            let now = timefmt::now_unix_secs();
            if let Err(e) = veille_ctx.run_cycle(now, &snap) {
                eprintln!("noyau: veille — cycle en echec : {e}");
            }
        }
    });

    while !SHUTDOWN.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = hb.join();
    journal.append(&json!({
        "kind": "shutdown", "boot_id": boot_id,
        "at": timefmt::iso8601_utc(timefmt::now_unix_secs()),
        "reason": "signal",
    }));
    println!("noyau: arret propre — boot {boot_id}");
}

struct BootCtx<'a> {
    cfg: &'a Config,
    journal: Arc<Journal>,
    state: Arc<RwLock<DaemonState>>,
    boot_id: String,
    t0_at: String,
    t0_unix: i64,
    prev_boot_id: Option<String>,
    anomalies: Vec<String>,
    store: Option<Arc<Mutex<Store>>>,
    replay: Option<(i64, i64)>,
    boot_start: Instant,
}

impl<'a> BootCtx<'a> {
    fn mark(&mut self, stage: &str) {
        let at = timefmt::iso8601_utc(timefmt::now_unix_secs());
        self.journal.append(&json!({
            "kind": "stage", "boot_id": self.boot_id, "stage": stage, "at": at,
        }));
        self.state
            .write()
            .unwrap()
            .stage_marks
            .push((stage.to_string(), at));
    }

    /// Etage 1 : verser le recu au store (s'il est ouvert), au fichier de
    /// recus du volume, et au journal. L'echec est une mesure de premiere
    /// classe : meme traitement que le succes.
    fn emit_receipt(&self, receipt: &Value, recorded_unix: i64) {
        if let Some(store) = &self.store {
            let store = store.lock().unwrap();
            let _ = store.record_boot_receipt(&self.boot_id, &receipt.to_string(), recorded_unix);
        }
        let dir = self.cfg.data_dir.join("receipts");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join(format!("{}.json", self.boot_id)),
            serde_json::to_string_pretty(receipt).unwrap_or_default(),
        );
        self.journal.append(&json!({
            "kind": "receipt", "boot_id": self.boot_id, "receipt": receipt,
        }));
    }

    fn fail(&mut self, stage: &str, cause: &str) -> ! {
        let (hops, unknown) = self.replay.unwrap_or((0, 0));
        let store_open = self.store.is_some();
        let receipt = json!({
            "boot_id": self.boot_id,
            "daemon_version": DAEMON_VERSION,
            "store_schema_version": store::STORE_SCHEMA_VERSION,
            "board_id": self.cfg.board_id,
            "t0_at": self.t0_at,
            "t1_at": Value::Null,
            "t2_ready_at": t2_value(&self.store),
            "watchdog": watchdog_boot_json(&self.store),
            "d_t1_ms": Value::Null,
            "store_integrity": if store_open { "OK" } else { "NOT_REACHED" },
            "replay": if store_open {
                json!({"hops_replayed": hops, "effects_unknown_pending": unknown})
            } else {
                Value::Null
            },
            "outcome": {"FAILED": {"stage": stage, "cause": cause}},
            "prev_boot_id": self.prev_boot_id,
            "anomalies": self.anomalies,
        });
        self.emit_receipt(&receipt, self.t0_unix);
        eprintln!("noyau: FAILED({stage}) — {cause} — boot {}", self.boot_id);
        std::process::exit(2);
    }
}

fn read_previous_boot(journal_path: &std::path::Path) -> (Option<String>, Option<i64>) {
    let Ok(content) = std::fs::read_to_string(journal_path) else {
        return (None, None);
    };
    let mut prev_id = None;
    let mut prev_t0 = None;
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(bid) = v.get("boot_id").and_then(|b| b.as_str()) {
            prev_id = Some(bid.to_string());
        }
        if let Some(t0) = v.get("t0_unix").and_then(|t| t.as_i64()) {
            prev_t0 = Some(t0);
        }
    }
    (prev_id, prev_t0)
}

fn write_atomic(path: &std::path::Path, value: &Value) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(value).unwrap_or_default())
        .map_err(|e| format!("ecriture: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renommage: {e}"))?;
    Ok(())
}
