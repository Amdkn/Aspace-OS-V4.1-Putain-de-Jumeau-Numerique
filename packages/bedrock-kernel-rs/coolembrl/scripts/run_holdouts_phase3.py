#!/usr/bin/env python3
"""Batterie de preuves des hold-outs de la Phase 3 du Noyau A1 (D-N039) :
WorkGraph type — H-P3-1 -> H-P3-13 (PRD du 13e + extensions de Clara),
contre le binaire reel.

Usage : python3 scripts/run_holdouts_phase3.py [--bin target/release/noyau]
        [--out DIR] [--workdir DIR] [--policy-src DIR]
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

RESULTS = []


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


class Daemon:
    _n = 0

    def __init__(self, binpath, data_dir, log_path, policy_dir=None):
        Daemon._n += 1
        base = 31000 + Daemon._n * 10
        self.api = f"127.0.0.1:{base}"
        self.mcp = f"127.0.0.1:{base + 1}"
        self.data_dir = data_dir
        env = dict(os.environ)
        env.update({"NOYAU_DATA_DIR": data_dir, "NOYAU_API_BIND": self.api,
                    "NOYAU_MCP_BIND": self.mcp, "NOYAU_BOARD_ID": "amd64-generic"})
        if policy_dir:
            env["NOYAU_POLICY_DIR"] = policy_dir
        self.env = env
        self.log = open(log_path, "wb")
        self.proc = subprocess.Popen([binpath], env=env, stdout=self.log, stderr=subprocess.STDOUT)

    def wait_t1(self, timeout=10):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                return False
            try:
                s, b = http(self.api, "GET", "/health")
                if s == 200 and b and b.get("stage") == "T1":
                    return True
            except Exception:
                pass
            time.sleep(0.05)
        return False

    def stop(self, sig=signal.SIGTERM):
        if self.proc.poll() is None:
            self.proc.send_signal(sig)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill(); self.proc.wait(timeout=5)
        self.log.close()

    def claim(self, mission, holder="h1", mode=None, ttl=900):
        body = {"work_id": "w-" + mission, "mission_id": mission, "operation_id": "op",
                "correlation_id": "c", "holder": holder, "ttl_s": ttl,
                "return_to": "channel:requester",
                "capabilities": {"permanent": [], "promotions": []},
                "budget": {"per_effect_caps": {"workgraph.transition": 200, "workgraph.reconcile": 50}}}
        if mode:
            body["mode"] = mode
        s, r = http(self.api, "POST", "/envelope/claim", body)
        assert s == 200, (s, r)
        return r["envelope"]

    def tr(self, env, **event):
        return http(self.api, "POST", "/workgraph/transition", {"envelope": env, "event": event})

    def reconcile(self, env, hop, resolved="CONFIRMED", source=None, read_at="2026-10-21T18:40:00Z"):
        body = {"envelope": env, "hop_id": hop, "resolved_to": resolved, "read_at": read_at,
                "source_read": source if source is not None else
                {"system": "github-api", "locator": "repos/X/pulls/2", "observed_value": "parents:2"}}
        return http(self.api, "POST", "/workgraph/reconcile", body)

    def mission(self, m):
        s, r = http(self.api, "GET", f"/workgraph/mission?mission_id={m}")
        assert s == 200, (s, r)
        return r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/noyau")
    ap.add_argument("--out", default="holdout-evidence-phase3")
    ap.add_argument("--workdir", default="/home/hatch/workspace/noyau-holdout-work-p3")
    ap.add_argument("--policy-src", default="/home/hatch/workspace/v4-capabilities/policy")
    args = ap.parse_args()
    binpath = os.path.abspath(args.bin)
    evid = os.path.abspath(args.out)
    shutil.rmtree(evid, ignore_errors=True); os.makedirs(evid)
    work = os.path.abspath(args.workdir)
    shutil.rmtree(work, ignore_errors=True); os.makedirs(work)

    data = os.path.join(work, "data-main")
    d = Daemon(binpath, data, os.path.join(evid, "daemon-main.log"), policy_dir=args.policy_src)
    assert d.wait_t1(), "boot principal en echec"

    # ============ mission wgA : schema, assertion, idempotence ============
    envA = d.claim("wgA")
    s, _ = d.tr(envA, hop_id="h1", transition_key="a1", type="create_hop", kind="picked", status="picked",
                assertion={"oneOf": [{"type": "object"}], "assert": {"head": "abc", "n": 1}, "libre": [1, 2, {"x": None}]})
    assert s == 200
    s, _ = d.tr(envA, hop_id="h2", transition_key="a2", type="create_hop", kind="agent-running", status="recorded")
    assert s == 200
    m = d.mission("wgA")

    # H-P3-11 : pas de second registre — champs fermes du modele, aucun
    # champ de dispatch dans les cles serialisees.
    mission_keys = set(m.keys())
    hop_keys = set(m["hops"][0].keys())
    dispatch_vocab = {"assignee", "assigne", "priority", "priorite", "instruction", "lane", "accuse", "dispatch", "owner"}
    ok = (mission_keys == {"work_id", "mission_id", "envelope_ref", "hops", "observed_at", "recorded_at"}
          and hop_keys == {"hop_id", "kind", "status", "lease_holder", "fencing_token", "evidence_refs", "effect"}
          and not (mission_keys | hop_keys) & dispatch_vocab)
    note("H-P3-11", ok, f"schema ferme : mission {sorted(mission_keys)} ; hop {sorted(hop_keys)} ; vocabulaire de dispatch present : {(mission_keys | hop_keys) & dispatch_vocab or 'aucun'} (la moitie registre — citation par pointeur — releve de la premiere ligne integree du 12e)")

    # H-P3-4 : contrat #64 par construction — assertion dict relue a l'identique
    s, r = http(d.api, "GET", "/workgraph/assertion?mission_id=wgA&hop_id=h1")
    attendu = {"oneOf": [{"type": "object"}], "assert": {"head": "abc", "n": 1}, "libre": [1, 2, {"x": None}]}
    ok = s == 200 and r["assertion"] == attendu
    note("H-P3-4", ok, "charge d'assertion dict (forme #64) stockee en JSON opaque et relue a l'identique, sans rejet de type")

    # H-P3-3 : verite temporelle — recorded_at refuse en entree, pose par le store
    s, r = d.tr(envA, hop_id="hx", transition_key="aX", type="create_hop", kind="picked", recorded_at="2020-01-01T00:00:00Z")
    ok = s == 400 and r["error"]["code"] == "RECORDED_AT_NOT_AN_INPUT"
    ok = ok and m["recorded_at"] >= "2026-10-21T00:00:00Z" and "2020" not in m["recorded_at"]
    note("H-P3-3", ok, "recorded_at fourni par l'appelant -> 400 RECORDED_AT_NOT_AN_INPUT ; les recorded_at lus sont poses par le store (horloge reelle, aucun antidatage)")

    # H-P3-7 : rejeu de transition = no-op detecte
    avant = json.dumps(d.mission("wgA"), sort_keys=True)
    s, r = d.tr(envA, hop_id="h1", transition_key="a1", type="create_hop", kind="picked", status="picked")
    apres = json.dumps(d.mission("wgA"), sort_keys=True)
    db = sqlite3.connect(os.path.join(data, "noyau-store.sqlite"))
    n_tr = db.execute("SELECT COUNT(*) FROM wg_transitions WHERE mission_id='wgA'").fetchone()[0]
    ok = s == 200 and r["noop"] is True and avant == apres and n_tr == 2
    note("H-P3-7", ok, f"meme (hop, transition_key) redelivre -> noop:true, etat canonique inchange, transitions wgA = {n_tr} (aucun double effet)")

    # ============ UNKNOWN : blocage, garde, resolution ============
    s, _ = d.tr(envA, hop_id="h1", transition_key="a3", type="effect_attempt", observed=False)
    assert s == 200
    # H-P3-8 : dependant bloque dans la liste
    s, elig = http(d.api, "GET", "/workgraph/eligible?mission_id=wgA")
    ok = "h2" not in elig["eligible"] and "h1" in elig["eligible"]
    # H-P3-13 : le garde est a la transition (contourner la liste ne sert a rien)
    s, r = d.tr(envA, hop_id="h2", transition_key="a4", type="set_status", status="agent-running")
    ok = ok and s == 409 and r["error"]["code"] == "DEPENDENCY_UNKNOWN"
    note("H-P3-8", "h2" not in elig["eligible"], f"eligible = {elig['eligible']} : le dependant d'un ATTEMPTED_UNKNOWN est absent de list_eligible_hops()")
    note("H-P3-13", s == 409 and r["error"]["code"] == "DEPENDENCY_UNKNOWN", "transition directe sur le dependant (sans passer par la liste) -> 409 DEPENDENCY_UNKNOWN, refus trace")

    # H-P3-6 + H-P3-12 : les modes interdits de resolution
    s, r = d.reconcile(envA, "h1", source="l'agent executant declare l'effet accompli")
    forme_refusee = s == 400 and r["error"]["code"] == "RECONCILIATION_FORM"
    s, r = d.tr(envA, hop_id="h1", transition_key="a5", type="effect_attempt", observed=True)
    retry_refuse = s == 409 and r["error"]["code"] == "EFFECT_ALREADY_UNKNOWN"
    etat = d.mission("wgA")["hops"][0]["effect"]
    ok = forme_refusee and retry_refuse and etat == "ATTEMPTED_UNKNOWN"
    note("H-P3-6", ok, "resolution par recit d'agent refusee ; re-tentative d'effet refusee (pas de retry aveugle) ; l'etat reste ATTEMPTED_UNKNOWN — seul un Reconciliation trace tranche")
    note("H-P3-12", forme_refusee, "source_read en texte libre -> 400 RECONCILIATION_FORM a l'ecriture ; seule une reference typee {system, locator, observed_value} ecrit un Reconciliation")

    # H-P3-9 : reconciliation sur l'etat reel, reprise sans re-execution
    s, r = d.reconcile(envA, "h1", resolved="CONFIRMED")
    ok = s == 200 and r["resolved_to"] == "CONFIRMED"
    s, r = d.tr(envA, hop_id="h1", transition_key="a6", type="effect_attempt", observed=True)
    ok = ok and s == 409 and r["error"]["code"] == "EFFECT_ALREADY_RESOLVED"
    s, r = d.tr(envA, hop_id="h2", transition_key="a7", type="set_status", status="agent-running")
    ok = ok and s == 200
    n_attempt = db.execute("SELECT COUNT(*) FROM wg_transitions WHERE mission_id='wgA' AND hop_id='h1' AND event_type='effect_attempt'").fetchone()[0]
    ok = ok and n_attempt == 1
    note("H-P3-9", ok, "effet applique en externe mais non rapporte -> reconciliation le lit CONFIRMED sur l'etat reel ; la mission reprend (h2 avance) SANS re-executer l'effet (tentatives h1 = 1)")

    # ============ fencing au WorkGraph + envelope_ref en transition ============
    envA2 = d.claim("wgA", holder="h2-successeur", mode="successor")
    s, r = d.tr(envA, hop_id="h2", transition_key="a8", type="set_status", status="qa-evidence")
    ok = s == 403 and r["error"]["code"] == "FENCED"
    n_tr = db.execute("SELECT COUNT(*) FROM wg_transitions WHERE mission_id='wgA' AND transition_key='a8'").fetchone()[0]
    ok = ok and n_tr == 0
    note("H-P3-2", ok, "transition presentee sous l'ancien bail apres succession -> 403 FENCED ; aucune ligne de transition ecrite (le store refuse a l'ecriture)")
    # la succession se prouve dans le graphe (design 0.7)
    s, r = d.tr(envA2, hop_id="h2", transition_key="a9", type="envelope_update")
    m = d.mission("wgA")
    ok = s == 200 and m["envelope_ref"] == envA2["evidence_ref"]
    s, lh = http(d.api, "GET", "/workgraph/lease-holder?mission_id=wgA")
    ok = ok and lh["holder"] == "h2-successeur" and lh["fencing_token"] == 2
    note("H-P3-2b", ok, "envelope_ref mis a jour COMME une transition sous le bail successeur ; current_lease_holder lit h2-successeur, token 2")

    # file des reconciliations pendantes (surface d'observation, sans seuil)
    envP = d.claim("wgP")
    s, _ = d.tr(envP, hop_id="p1", transition_key="p1", type="create_hop", kind="picked", status="picked")
    s, _ = d.tr(envP, hop_id="p1", transition_key="p2", type="effect_attempt", observed=False)
    s, pend = http(d.api, "GET", "/workgraph/pending-reconciliations")
    item = [p for p in pend["pending"] if p["mission_id"] == "wgP"]
    ok = len(item) == 1 and item[0]["age_secs"] is not None and item[0]["age_secs"] >= 0
    note("H-P3-5b", ok, f"list_pending_reconciliations expose l'UNKNOWN wgP/p1 avec son age ({item[0]['age_secs'] if item else '?'} s) — donnee exposee, aucun seuil ni notification attaches (design 0.5)")
    d.stop()

    # ============ H-P3-1 + H-P3-10 : zero perte, honnetete du rejeu ============
    data2 = os.path.join(work, "data-reboot")
    d2 = Daemon(binpath, data2, os.path.join(evid, "daemon-reboot.log"), policy_dir=args.policy_src)
    assert d2.wait_t1()
    envB = d2.claim("wgB")
    snaps = []
    etapes = [
        dict(hop_id="b1", transition_key="b1", type="create_hop", kind="picked", status="picked"),
        dict(hop_id="b2", transition_key="b2", type="create_hop", kind="pr-open", status="recorded"),
        dict(hop_id="b1", transition_key="b3", type="effect_attempt", observed=True),
        dict(hop_id="b2", transition_key="b4", type="effect_attempt", observed=False),
    ]
    ok = True
    for ev in etapes:
        s, _ = d2.tr(envB, **ev)
        assert s == 200, (ev, s)
        snaps.append(json.dumps(d2.mission("wgB"), sort_keys=True))
        d2.stop(sig=signal.SIGKILL)  # tue a CHAQUE frontiere de hop
        d2 = Daemon(binpath, data2, os.path.join(evid, "daemon-reboot.log"), policy_dir=args.policy_src)
        assert d2.wait_t1()
        apres = json.dumps(d2.mission("wgB"), sort_keys=True)
        if apres != snaps[-1]:
            ok = False
    m = d2.mission("wgB")
    etats = {h["hop_id"]: h["effect"] for h in m["hops"]}
    note("H-P3-1", ok, "kill -9 a chaque frontiere de hop (4 frontieres) -> redemarrage -> etat canonique identique a chaque fois, aucun hop perdu")
    note("H-P3-10", etats.get("b2") == "ATTEMPTED_UNKNOWN" and etats.get("b1") == "CONFIRMED",
         f"apres reboot : b2 (effet tente a l'arret) lit ATTEMPTED_UNKNOWN — jamais promu CONFIRMED, jamais reinitialise ; b1 reste CONFIRMED : {etats}")
    d2.stop()
    db.close()

    # ============ H-P3-5 : cycle complet reseau coupe ============
    client = os.path.join(work, "wg_cycle_client.py")
    with open(client, "w") as fh:
        fh.write(CLIENT_NS)
    ns_data = os.path.join(work, "data-ns")
    cmd = (f"ip link set lo up 2>/dev/null; "
           f"NOYAU_DATA_DIR={ns_data} NOYAU_API_BIND=127.0.0.1:31990 NOYAU_MCP_BIND=127.0.0.1:31991 "
           f"NOYAU_POLICY_DIR={args.policy_src} {binpath} & DPID=$!; "
           f"for i in $(seq 1 60); do curl -s http://127.0.0.1:31990/health >/dev/null 2>&1 && break; sleep 0.2; done; "
           f"python3 {client}; RC=$?; kill -TERM $DPID 2>/dev/null; exit $RC")
    r = subprocess.run(["unshare", "-n", "bash", "-c", cmd], capture_output=True, text=True, timeout=120)
    ok = r.returncode == 0 and '"cycle": "OK"' in r.stdout
    note("H-P3-5", ok, f"cycle complet de mission (claim, hops, UNKNOWN, reconciliation typee, reprise) dans un namespace reseau isole, loopback seule : {r.stdout.strip()[:120]}")

    with open(os.path.join(evid, "RESULTS-P3.md"), "w") as fh:
        fh.write("# Resultats des hold-outs H-P3 — WorkGraph type (Phase 3, D-N039)\n\n")
        for hid, statut, preuve in RESULTS:
            fh.write(f"- **{hid}** : {statut} — {preuve}\n")
    fails = [h for h, s_, _ in RESULTS if s_ == "FAIL"]
    print(f"Bilan : {len(RESULTS) - len(fails)}/{len(RESULTS)} PASS ; echecs : {fails}")
    return 1 if fails else 0


CLIENT_NS = '''
import json, urllib.request, urllib.error
B = "http://127.0.0.1:31990"
def http(m, p, body=None):
    req = urllib.request.Request(B + p, data=json.dumps(body).encode() if body is not None else None,
                                 method=m, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status, json.loads(r.read().decode() or "null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "null")
s, r = http("POST", "/envelope/claim", {"work_id": "w-ns", "mission_id": "wgNS", "operation_id": "op",
    "correlation_id": "c", "holder": "h", "ttl_s": 600, "return_to": "chan",
    "capabilities": {"permanent": [], "promotions": []},
    "budget": {"per_effect_caps": {"workgraph.transition": 50, "workgraph.reconcile": 10}}})
assert s == 200, (s, r)
env = r["envelope"]
def tr(**ev): return http("POST", "/workgraph/transition", {"envelope": env, "event": ev})
assert tr(hop_id="n1", transition_key="n1", type="create_hop", kind="picked", status="picked")[0] == 200
assert tr(hop_id="n1", transition_key="n2", type="effect_attempt", observed=False)[0] == 200
s, r = http("POST", "/workgraph/reconcile", {"envelope": env, "hop_id": "n1", "resolved_to": "REFUTED",
    "read_at": "2026-10-21T18:45:00Z",
    "source_read": {"system": "local-store", "locator": "wgNS/n1", "observed_value": "absent"}})
assert s == 200, (s, r)
s, m = http("GET", "/workgraph/mission?mission_id=wgNS")
assert m["hops"][0]["effect"] == "REFUTED", m
print(json.dumps({"cycle": "OK"}))
'''


if __name__ == "__main__":
    sys.exit(main())
