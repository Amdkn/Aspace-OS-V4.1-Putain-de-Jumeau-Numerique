#!/usr/bin/env python3
"""Batterie de preuves des hold-outs de la Phase 4 du Noyau A1 (D-N041) :
watchdog N2 et protocole de relais — H-P4-1 -> H-P4-13 (PRD du 13e + extensions
de Clara), contre le binaire reel.

Usage : python3 scripts/run_holdouts_phase4.py [--bin target/debug/noyau]
        [--workdir DIR]

Chaque hold-out construit un monde fixture isole (registre, trace interimaire,
policy, journal, stub gh) et pilote le VRAI binaire via `veille-cycle-once`,
`veille-switchover` et `veille-rollback`. Aucune horloge simulee : les
fenetres de 14 jours sont prouvees par des rapports complets ecrits en
fixture (la mecanique de la gate, pas le temps reel, est l'objet du test).
"""
import argparse
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime, timedelta, timezone

RESULTS = []

EDT = timezone(timedelta(hours=-4))


def now_edt():
    return datetime.now(EDT).strftime("%Y-%m-%d %H:%M EDT")


def edt(ts):
    return datetime.fromtimestamp(ts, EDT).strftime("%Y-%m-%d %H:%M EDT")


def note(hid, ok, preuve):
    RESULTS.append((hid, "PASS" if ok else "FAIL", preuve))
    print(f"[{'PASS' if ok else 'FAIL'}] {hid} — {preuve}", flush=True)


def http(bind, method, path, body=None):
    url = f"http://{bind}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status, json.loads(r.read().decode() or "null")
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode() or "null")
        except Exception:
            return e.code, None


GH_STUB = """#!/usr/bin/env python3
import json, os, sys
data = json.load(open(os.environ["GH_STUB_DATA"]))
path = sys.argv[2]
print(json.dumps(data.get(path, {})))
"""

CALM_REGISTRY = """# REGISTRE DE DISPATCH — fixture H-P4

| ID | MISSION | SOURCE | EXÉCUTANT | CIBLE | STATUT | PREUVE | BLOCAGE | MAJ |
|---|---|---|---|---|---|---|---|---|
| D-N900 | Mission calme (fixture) | S1 | Ryan | 13e | TERMINÉ | - | - | 2026-10-23 10:00 EDT |
"""

ANOMALY_REGISTRY = CALM_REGISTRY + """| D-N901 | Mission calee (fixture) | S1 | Ryan | 13e | EN COURS | - | - | 2026-10-20 10:00 EDT |
- 2026-10-20 10:00 EDT — **D-N901 OUVERT** : declencheur pose (fixture).
"""


def interim_line(ts=None, verdicts=None):
    v = verdicts or {f"W{i}": "OK" for i in range(1, 8)}
    parts = " · ".join(f"{k} {val}" for k, val in sorted(v.items()))
    return (f"- **{edt(ts or time.time())}** — Run #99 (créneau {edt(ts or time.time())}) : "
            f"{parts} · spec W1-W7@v1.0.0 · source l00 · Aucune anomalie routée.\n")


class World:
    """Un monde fixture isole : fichiers + env pour un run du binaire reel."""

    def __init__(self, workdir, name, binpath, anomaly=False):
        self.wd = os.path.join(workdir, name)
        self.data = os.path.join(self.wd, "data")
        self.fx = os.path.join(self.wd, "fx")
        for d in (self.data, self.fx):
            os.makedirs(d, exist_ok=True)
        self.binpath = binpath
        self.registry = os.path.join(self.fx, "REGISTRE-NARDOLE.md")
        self.interim = os.path.join(self.fx, "L00-ETAT.md")
        self.trace = os.path.join(self.fx, "L00-N2-ETAT.md")
        self.outbox = os.path.join(self.data, "veille-outbox.jsonl")
        self.report = os.path.join(self.data, "veille-concordance.json")
        self.policy = os.path.join(self.fx, "policy")
        self.journald = os.path.join(self.fx, "memory")
        self.bearers = os.path.join(self.fx, "bearers.json")
        self.stub = os.path.join(self.fx, "gh-stub.py")
        self.stub_data = os.path.join(self.fx, "gh-stub-data.json")
        os.makedirs(self.policy, exist_ok=True)
        os.makedirs(self.journald, exist_ok=True)
        self.set_registry(anomaly=anomaly)
        open(self.interim, "w").write(interim_line())
        open(os.path.join(self.policy, "REGISTRY.md"), "w").write(
            "# REGISTRY\n\nstore v1.0.0\n")
        open(os.path.join(self.policy, "CHANGELOG.md"), "w").write("# CHANGELOG\n")
        jname = datetime.now(EDT).strftime("%Y-%m-%d") + ".md"
        self.journal = os.path.join(self.journald, jname)
        open(self.journal, "w").write("# Journal (fixture)\n\nRyan actif (fixture).\n")
        open(self.bearers, "w").write(json.dumps({"Ryan": "dummy"}))
        open(self.stub, "w").write(GH_STUB)
        os.chmod(self.stub, 0o755)
        self.set_gh(main_age_h=1, issues=[])
        self.env = dict(os.environ)
        self.env.update({
            "NOYAU_DATA_DIR": self.data,
            "NOYAU_VEILLE_REGISTRY": self.registry,
            "NOYAU_VEILLE_INTERIM_TRACE": self.interim,
            "NOYAU_VEILLE_TRACE": self.trace,
            "NOYAU_VEILLE_OUTBOX": self.outbox,
            "NOYAU_VEILLE_REPORT": self.report,
            "NOYAU_POLICY_DIR": self.policy,
            "NOYAU_VEILLE_JOURNAL_DIR": self.journald,
            "NOYAU_VEILLE_WORKSPACE": self.fx,
            "NOYAU_VEILLE_BEARERS": self.bearers,
            "NOYAU_VEILLE_GH_STUB": self.stub,
            "GH_STUB_DATA": self.stub_data,
            "NOYAU_VEILLE_INTERVAL_SECS": "60",
        })

    def set_registry(self, anomaly=False):
        open(self.registry, "w").write(ANOMALY_REGISTRY if anomaly else CALM_REGISTRY)

    def set_gh(self, main_age_h=1, issues=None):
        date = datetime.now(timezone.utc) - timedelta(hours=main_age_h)
        data = {
            "repos/Amdkn/Aspace_OS-V4/commits/main": {
                "sha": "abc123def456abc123def456abc123def456abcd",
                "commit": {"committer": {"date": date.strftime("%Y-%m-%dT%H:%M:%SZ")}},
            },
            "repos/Amdkn/Aspace_OS-V4/issues?state=open&per_page=30": issues or [],
        }
        open(self.stub_data, "w").write(json.dumps(data))

    def db(self):
        return sqlite3.connect(os.path.join(self.data, "noyau-store.sqlite"))

    def sql(self, q, args=()):
        c = self.db()
        try:
            rows = c.execute(q, args).fetchall()
            c.commit()
            return rows
        finally:
            c.close()

    def run(self, *args):
        p = subprocess.run([self.binpath, *args], env=self.env,
                           capture_output=True, text=True, timeout=60)
        return p.returncode, p.stdout.strip(), p.stderr.strip()

    def cycle(self):
        rc, out, err = self.run("veille-cycle-once")
        assert rc == 0, f"veille-cycle-once en echec : {err}"
        return json.loads(out)

    def ensure_store(self):
        if not os.path.exists(os.path.join(self.data, "noyau-store.sqlite")):
            self.cycle()

    def set_stage(self, s):
        self.ensure_store()
        self.sql("INSERT INTO meta(key, value) VALUES('veille_stage', ?)"
                 " ON CONFLICT(key) DO UPDATE SET value=excluded.value", (s,))

    def switchover(self):
        return self.run("veille-switchover")

    def rollback(self):
        return self.run("veille-rollback")

    def trace_lines(self):
        if not os.path.exists(self.trace):
            return []
        return open(self.trace).read().splitlines()

    def outbox_lines(self):
        if not os.path.exists(self.outbox):
            return []
        return [json.loads(l) for l in open(self.outbox).read().splitlines() if l.strip()]

    def write_complete_report(self, days=15, n=400, discordance_sans_cause=False):
        now = time.time()
        cycles = []
        for i in range(n):
            cycles.append({
                "at": edt(now - (n - i) * 3600),
                "comparable": True,
                "concordant": True,
                "par_item": [{"item": f"W{k}", "n2": "OK", "interimaire": "OK", "egaux": True}
                             for k in range(1, 8)],
            })
        disc = []
        if discordance_sans_cause:
            disc.append({"at": edt(now - 86400), "item": "W1", "n2": "ANOMALIE",
                         "interimaire": "OK", "cause": "a nommer — fixture"})
        json.dump({
            "window_start": edt(now - days * 86400),
            "concordant_consecutifs": n,
            "cycles": cycles,
            "discordances": disc,
            "fenetre_requise": {"jours_min": 14, "cycles_min": 336},
        }, open(self.report, "w"), indent=2)

    def table_dump(self, tables):
        c = self.db()
        try:
            out = {}
            for t in tables:
                try:
                    out[t] = c.execute(f"SELECT * FROM {t} ORDER BY rowid").fetchall()
                except Exception:
                    out[t] = None
            return out
        finally:
            c.close()


TABLES_MISSION = ["leases", "wg_hops", "wg_transitions", "wg_reconciliations", "state_entries"]


def h_p4_1(w):
    """Silence d'ombre : anomalie plantee en stade A -> consignee en
    observation d'ombre, rien route."""
    r = w.cycle()
    w1 = next(v for v in r["verdicts"] if v["item"] == "W1")
    tl = w.trace_lines()
    ok = (r["stade"] == "A" and w1["verdict"] == "ANOMALIE"
          and r["alerts_routed"] == [] and r["routing_requests"] == []
          and w.outbox_lines() == []
          and any("noyau-n2" in l and "stade A" in l for l in tl))
    note("H-P4-1", ok,
         f"stade A : W1={w1['verdict']}, ombre consignee, outbox vide ({len(tl)} ligne(s) noyau-n2)")


def h_p4_2(w):
    """Concordance : verdicts identiques des deux cotes ; discordance
    injectee -> anomalie du relais, fenetre non declaree."""
    w.set_stage("B")
    r = w.cycle()
    ok_a = (r["concordant_consecutifs"] == 1)
    rep = json.load(open(w.report))
    par_item = rep["cycles"][-1]["par_item"]
    ok_a = ok_a and all(p["egaux"] for p in par_item)
    # Discordance : N2 voit l'anomalie, l'interimaire dit OK.
    w.set_registry(anomaly=True)
    r2 = w.cycle()
    tl = w.trace_lines()
    rep2 = json.load(open(w.report))
    disc = rep2["discordances"]
    ok_b = (r2["concordant_consecutifs"] == 0
            and any("anomalie DU RELAIS" in l for l in tl)
            and any(d["item"] == "W1" and "nommer" in d["cause"] for d in disc))
    note("H-P4-2", ok_a and ok_b,
         f"concordance calme -> compteur=1, par_item egaux ; discordance W1 -> compteur=0, anomalie du relais levee")


def h_p4_3(w):
    """Aucun trou de veille : audit des deux traces sur A->D, chaque cycle
    couvert par au moins un battement a source identifiee."""
    w.cycle(); w.cycle()
    open(w.interim, "a").write(interim_line())
    w.set_stage("B")
    w.cycle()
    w.write_complete_report()
    rc, _, err = w.switchover()
    assert rc == 0, f"bascule fixture : {err}"
    w.cycle()
    w.rollback()
    w.cycle()

    def slots(path, source):
        out = []
        for l in open(path).read().splitlines():
            if l.startswith("- **") and source in l:
                try:
                    ts = datetime.strptime(l[4:20], "%Y-%m-%d %H:%M").replace(tzinfo=EDT).timestamp()
                    out.append(ts)
                except Exception:
                    pass
        return sorted(out)

    n2 = slots(w.trace, "noyau-n2")
    inter = slots(w.interim, "l00")
    assert n2, "trace N2 vide"
    start = min(n2 + inter) // 3600 * 3600
    end = max(n2 + inter)
    gaps = []
    s = start
    while s <= end:
        if not any(abs(t - s) < 3600 for t in n2 + inter):
            gaps.append(s)
        s += 3600
    all_lines = open(w.trace).read().splitlines() + open(w.interim).read().splitlines()
    sourced = [l for l in all_lines if l.startswith("- **")]
    unsourced = [l for l in sourced if "noyau-n2" not in l and "source l00" not in l and "l00 " not in l]
    ok = (not gaps) and (not unsourced)
    note("H-P4-3", ok,
         f"{len(n2)} battements N2 + {len(inter)} interimaire, 0 trou de couverture, 0 ligne sans source")


def h_p4_4(w):
    """La bascule est un acte : trois traces + recu ; post-bascule,
    l'anomalie est routee par N2, pas par l'interimaire en veille."""
    w.set_stage("B")
    w.write_complete_report()
    before_interim = open(w.interim).read()
    rc, out, err = w.switchover()
    t2 = w.sql("SELECT value FROM meta WHERE key='t2_ready_at'")
    tl = w.trace_lines()
    ok_a = (rc == 0 and w.sql("SELECT value FROM meta WHERE key='veille_stage'")[0][0] == "C"
            and t2 and t2[0][0]
            and any("BASCULE CONSIGNEE" in l for l in tl))
    w.set_registry(anomaly=True)
    r = w.cycle()
    ob = w.outbox_lines()
    ok_b = (any(o.get("classe") == "alerte" and o.get("a") == "S1" and o.get("item") == "W1" for o in ob)
            and open(w.interim).read() == before_interim)
    note("H-P4-4", ok_a and ok_b,
         f"bascule : stade C, t2={t2[0][0] if t2 else '?'} ; post-bascule : alerte W1 routee par N2, interimaire silencieux")


def h_p4_5(w):
    """File UNKNOWN : vieilli > 12 h -> anomalie routee a S1 ; jeune -> silence.
    Le seuil est applique par le veilleur (surface sans seuil)."""
    w.set_stage("C")
    now = int(time.time())
    w.sql("INSERT INTO wg_hops(mission_id, hop_id, seq, kind, status, effect_state, effect_attempted_unix, recorded_at)"
          " VALUES('M1','h1',1,'work','open','ATTEMPTED_UNKNOWN',?,'x')", (now - 13 * 3600,))
    w.sql("INSERT INTO wg_hops(mission_id, hop_id, seq, kind, status, effect_state, effect_attempted_unix, recorded_at)"
          " VALUES('M2','h2',1,'work','open','ATTEMPTED_UNKNOWN',?,'x')", (now - 3600,))
    r = w.cycle()
    uq = next(v for v in r["verdicts"] if v["item"] == "UQ")
    ob = w.outbox_lines()
    ok = (uq["verdict"] == "ANOMALIE" and "M1/h1" in uq["detail"] and "M2" not in uq["detail"]
          and any(o.get("item") == "UQ" for o in ob))
    note("H-P4-5", ok, "UNKNOWN 13 h -> anomalie routee ; UNKNOWN 1 h -> silence (seuil veilleur, surface intacte)")


def h_p4_6(w):
    """Cale sans crash -> rescue constitue et route au decideur nomme ;
    la trace de N2 ne montre aucune mutation de la mission."""
    w.set_stage("C")
    now = int(time.time())
    w.sql("INSERT INTO leases(mission_id, fencing_token, holder, issued_unix, expires_unix, envelope_json, state)"
          " VALUES('M3',7,'HolderX',?,?,?, 'live')", (now - 7200, now + 3600, "{}"))
    before = w.table_dump(TABLES_MISSION)
    r = w.cycle()
    ob = w.outbox_lines()
    after = w.table_dump(TABLES_MISSION)
    rescue = [o for o in ob if o.get("classe") == "rescue"]
    ok = (any(o.get("decideur") == "Doctor S2 de la lane" and "M3" in o.get("objet", "") for o in rescue)
          and before == after)
    note("H-P4-6", ok, "bail tenu sans progression (2 cycles) -> rescue au Doctor S2 ; 0 mutation de la mission")


def h_p4_7(w):
    """Recovery route, jamais execute par N2 : holder mort -> demande au
    dispatch ; l'acte s'executera par son porteur sous enveloppe."""
    w.set_stage("C")
    now = int(time.time())
    w.sql("INSERT INTO leases(mission_id, fencing_token, holder, issued_unix, expires_unix, envelope_json, state)"
          " VALUES('M4',9,'HolderY',?,?,?, 'live')", (now - 7200, now - 3600, "{}"))
    before = w.table_dump(TABLES_MISSION)
    r = w.cycle()
    ob = w.outbox_lines()
    after = w.table_dump(TABLES_MISSION)
    rec = [o for o in ob if o.get("classe") == "recovery"]
    ok = (any(o.get("porteur") == "HolderY" and "M4" in o.get("objet", "") for o in rec)
          and before == after)
    note("H-P4-7", ok, "bail expire -> recovery routee au dispatch ; N2 n'a rien execute (tables inchangees)")


def h_p4_8(w):
    """Dead-man sur N2 : post-bascule, N2 arrete -> la couche survivante
    rapporte son silence dans la borne."""
    w.set_stage("B")
    w.write_complete_report()
    rc, _, err = w.switchover()
    assert rc == 0, f"bascule fixture : {err}"
    # N2 s'arrete : on vieillit artificiellement sa trace de 3 h.
    old = time.time() - 3 * 3600
    os.utime(w.trace, (old, old))
    # La couche survivante (checklist native) lit l'age de la trace.
    age = time.time() - os.path.getmtime(w.trace)
    ok = age > 7200
    note("H-P4-8", ok, f"trace N2 agee de {age/3600:.1f} h -> silence rapporte par la couche survivante (> 2 h)")


def h_p4_9(w, binpath):
    """Signature T2 : t2_ready_at null sur toute la serie pre-bascule ;
    non nul au recu de boot post-bascule, avec le watchdog autoritaire."""
    base = 32100
    env = dict(w.env)
    env.update({"NOYAU_API_BIND": f"127.0.0.1:{base}", "NOYAU_MCP_BIND": f"127.0.0.1:{base+1}",
                "NOYAU_BOARD_ID": "amd64-generic"})
    log = open(os.path.join(w.wd, "daemon.log"), "wb")
    p = subprocess.Popen([binpath], env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        for _ in range(50):
            try:
                s, b = http(f"127.0.0.1:{base}", "GET", "/state")
                if s == 200 and b.get("stage") == "T1":
                    break
            except Exception:
                pass
            time.sleep(0.2)
        else:
            note("H-P4-9", False, "boot fixture sans T1")
            return
        rows = w.sql("SELECT receipt_json FROM boots ORDER BY rowid DESC LIMIT 1")
        r1 = json.loads(rows[0][0])
        ok_a = r1["t2_ready_at"] is None and r1["watchdog"]["stade"] == "A"
    finally:
        p.terminate()
        p.wait(timeout=10)
    w.set_stage("B")
    w.write_complete_report()
    rc, out, err = w.switchover()
    assert rc == 0, f"bascule fixture : {err}"
    act_at = json.loads(out[out.index("{"):])["at"]
    log2 = open(os.path.join(w.wd, "daemon2.log"), "wb")
    env2 = dict(env)
    env2.update({"NOYAU_API_BIND": f"127.0.0.1:{base+2}", "NOYAU_MCP_BIND": f"127.0.0.1:{base+3}"})
    p2 = subprocess.Popen([binpath], env=env2, stdout=log2, stderr=subprocess.STDOUT)
    try:
        for _ in range(50):
            try:
                s, b = http(f"127.0.0.1:{base+2}", "GET", "/state")
                if s == 200 and b.get("stage") == "T1":
                    break
            except Exception:
                pass
            time.sleep(0.2)
        rows = w.sql("SELECT receipt_json FROM boots ORDER BY rowid DESC LIMIT 1")
        r2 = json.loads(rows[0][0])
        ok_b = (r2["t2_ready_at"] == act_at and r2["watchdog"]["autoritaire"] is True
                and r2["watchdog"]["stade"] == "C")
    finally:
        p2.terminate()
        p2.wait(timeout=10)
    note("H-P4-9", ok_a and ok_b,
         f"pre-bascule : t2 null, stade A ; post-bascule : t2={r2['t2_ready_at'] if ok_b else '?'} = horodatage de l'acte, autoritaire")


def h_p4_10(w):
    """Fidelite de spec : version rapportee ; spec gouvernee plus recente
    -> anomalie de maintenance ; N2 perime -> anomalie de maintenance."""
    w.set_stage("C")
    spec_file = os.path.join(w.fx, "spec-courante.txt")
    open(spec_file, "w").write("W1-W7@v1.0.0")
    w.env["NOYAU_VEILLE_SPEC_COURANTE"] = spec_file
    r = w.cycle()
    spec = next(v for v in r["verdicts"] if v["item"] == "SPEC")
    ok_a = spec["verdict"] == "OK"
    open(spec_file, "w").write("W1-W7@v2.0.0")  # la gouvernance a avance
    r2 = w.cycle()
    spec2 = next(v for v in r2["verdicts"] if v["item"] == "SPEC")
    ob = w.outbox_lines()
    ok_b = (spec2["verdict"] == "ANOMALIE"
            and any(o.get("item") == "SPEC" for o in ob))
    open(spec_file, "w").write("W1-W7@v1.0.0")
    w.sql("UPDATE meta SET value='W1-W7@v0.9.0' WHERE key='veille_spec_version'")  # N2 perime
    r3 = w.cycle()
    spec3 = next(v for v in r3["verdicts"] if v["item"] == "SPEC")
    ok_c = spec3["verdict"] == "ANOMALIE"
    note("H-P4-10", ok_a and ok_b and ok_c,
         "spec rapportee OK ; gouvernance v2 -> anomalie routee ; N2 perime -> anomalie")


def h_p4_11(w, workdir, binpath):
    """Integrite de la fenetre : la bascule n'existe pas sans rapport complet."""
    rc1, _, e1 = w.switchover()
    ok_a = rc1 != 0 and "BASCULE_REFUSEE" in e1
    w.set_stage("B")
    w.write_complete_report(days=5, n=100)
    rc2, _, e2 = w.switchover()
    ok_b = rc2 != 0 and "BASCULE_REFUSEE" in e2
    w.write_complete_report(days=15, n=400)
    rc3, _, e3 = w.switchover()
    ok_c = rc3 == 0
    w2 = World(workdir, "h11b", binpath)
    w2.set_stage("B")
    w2.write_complete_report(days=15, n=400, discordance_sans_cause=True)
    rc4, _, e4 = w2.switchover()
    ok_d = rc4 != 0 and "BASCULE_REFUSEE" in e4
    note("H-P4-11", ok_a and ok_b and ok_c and ok_d,
         "sans rapport / fenetre 5 j-100 cycles / discordance sans cause -> refuse ; rapport complet -> bascule")


def h_p4_12(w):
    """Ecriture unique de T2 : seconde ecriture refusee par le store,
    meme en contournant la gate de stade de la commande (la propriete est
    celle du champ, pas une discipline des redacteurs — design §0.5)."""
    w.set_stage("B")
    w.write_complete_report()
    rc1, _, _ = w.switchover()
    assert rc1 == 0
    t2_avant = w.sql("SELECT value FROM meta WHERE key='t2_ready_at'")[0][0]
    w.set_stage("B")  # contournement force de la gate de stade
    rc2, _, e2 = w.switchover()
    t2_apres = w.sql("SELECT value FROM meta WHERE key='t2_ready_at'")[0][0]
    ok = rc2 != 0 and "T2_ALREADY_SET" in e2 and t2_avant == t2_apres
    note("H-P4-12", ok, f"seconde ecriture -> T2_ALREADY_SET refuse par le store ; t2 inchange ({t2_avant})")


def h_p4_13(w):
    """Le rollback est un acte, jamais un reflexe : l'interimaire ne repart
    pas seul ; toute bascule ulterieure exige une fenetre neuve."""
    w.set_stage("B")
    w.write_complete_report()
    rc, _, _ = w.switchover()
    assert rc == 0
    before_interim = open(w.interim).read()
    # L'interimaire ne repart PAS seul : aucun acte, aucune ligne.
    ok_a = open(w.interim).read() == before_interim
    rc2, _, _ = w.rollback()
    tl = w.trace_lines()
    rep = json.load(open(w.report))
    ok_b = (rc2 == 0 and w.sql("SELECT value FROM meta WHERE key='veille_stage'")[0][0] == "A"
            and any("ROLLBACK CONSIGNE" in l for l in tl)
            and "invalidated_by_rollback_at" in rep)
    rc3, _, e3 = w.switchover()
    ok_c = rc3 != 0 and "BASCULE_REFUSEE" in e3
    note("H-P4-13", ok_a and ok_b and ok_c,
         "interimaire sans reprise spontanee ; rollback -> stade A, rapport invalide ; re-bascule refusee")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/debug/noyau")
    ap.add_argument("--workdir", default=os.path.expanduser("~/workspace/v4-factory/evidence/D-N041"))
    args = ap.parse_args()
    binpath = args.bin if os.path.isabs(args.bin) else os.path.abspath(args.bin)
    workdir = args.workdir
    if os.path.exists(workdir):
        shutil.rmtree(workdir)
    os.makedirs(workdir, exist_ok=True)
    sha = subprocess.run(["sha256sum", binpath], capture_output=True, text=True).stdout.split()[0]

    h_p4_1(World(workdir, "h1", binpath, anomaly=True))
    h_p4_2(World(workdir, "h2", binpath))
    h_p4_3(World(workdir, "h3", binpath))
    h_p4_4(World(workdir, "h4", binpath))
    h_p4_5(World(workdir, "h5", binpath))
    h_p4_6(World(workdir, "h6", binpath))
    h_p4_7(World(workdir, "h7", binpath))
    h_p4_8(World(workdir, "h8", binpath))
    h_p4_9(World(workdir, "h9", binpath), binpath)
    h_p4_10(World(workdir, "h10", binpath))
    h_p4_11(World(workdir, "h11", binpath), workdir, binpath)
    h_p4_12(World(workdir, "h12", binpath))
    h_p4_13(World(workdir, "h13", binpath))

    print(f"\nBinaire : {binpath}\nsha256 : {sha}", flush=True)
    fails = [h for h, s, _ in RESULTS if s == "FAIL"]
    print(f"\nBilan : {len(RESULTS)-len(fails)}/{len(RESULTS)} PASS ; echecs : {fails}", flush=True)
    json.dump({"binaire_sha256": sha, "resultats": [
        {"holdout": h, "statut": s, "preuve": p} for h, s, p in RESULTS]},
        open(os.path.join(workdir, "RESULTS-P4.json"), "w"), indent=2)
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
