#!/usr/bin/env python3
"""Batterie de preuves des hold-outs de la Phase 2 du Noyau A1 (D-N038) :
les quatre preuves de l'issue #2 en execution (H-P2-1 -> H-P2-4) et la
completude du modele (H-P2-5 -> H-P2-11), contre le binaire reel, avec un
policy store L05 reel (copie) puis modifie (preuve comportementale).

Usage : python3 scripts/run_holdouts_phase2.py [--bin target/release/noyau]
        [--out DIR] [--workdir DIR] [--policy-src DIR]
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
import urllib.request
import urllib.error

RESULTS = []
REFUSALS = []  # corps des refus du service d'enveloppe, pour H-P2-9


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
        base = 21000 + Daemon._n * 10
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

    def stop(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill(); self.proc.wait(timeout=5)
        self.log.close()

    def claim(self, **kw):
        body = {"work_id": "w-1", "mission_id": "m", "operation_id": "op",
                "correlation_id": "c-1", "holder": "h", "ttl_s": 300,
                "return_to": "channel:requester",
                "capabilities": {"permanent": [], "promotions": []},
                "budget": {"per_effect_caps": {}}}
        body.update(kw)
        return http(self.api, "POST", "/envelope/claim", body)

    def effect(self, envelope, **eff):
        body = {"effect_class": "store.write", "head_sha": "a" * 40}
        body.update(eff)
        return http(self.api, "POST", "/envelope/effect", {"envelope": envelope, "effect": body})


def store_db(data_dir):
    return sqlite3.connect(os.path.join(data_dir, "noyau-store.sqlite"))


def traces(db, kind=None):
    q = "SELECT id, kind, mission_id FROM authority_traces"
    if kind:
        q += f" WHERE kind='{kind}'"
    return db.execute(q + " ORDER BY id").fetchall()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/noyau")
    ap.add_argument("--out", default="holdout-evidence-phase2")
    ap.add_argument("--workdir", default="/home/hatch/workspace/noyau-holdout-work-p2")
    ap.add_argument("--policy-src", default="/home/hatch/workspace/v4-capabilities/policy")
    args = ap.parse_args()
    binpath = os.path.abspath(args.bin)
    evid = os.path.abspath(args.out)
    shutil.rmtree(evid, ignore_errors=True); os.makedirs(evid)
    work = os.path.abspath(args.workdir)
    shutil.rmtree(work, ignore_errors=True); os.makedirs(work)
    bin_hash = hashlib.sha256(open(binpath, "rb").read()).hexdigest()

    # Policy store A : copie du store L05 reel. Store B : P-003 passee
    # DECLARATIVE + version du store bumpee (preuve comportementale H-P2-11).
    pol_a = os.path.join(work, "policy-a"); shutil.copytree(args.policy_src, pol_a)
    pol_b = os.path.join(work, "policy-b"); shutil.copytree(args.policy_src, pol_b)
    reg = open(os.path.join(pol_b, "REGISTRY.md")).read()
    reg = reg.replace("| OPPOSABLE | check CI `secret-scan` (grep) |", "| DÉCLARATIVE | check CI `secret-scan` (grep) |")
    open(os.path.join(pol_b, "REGISTRY.md"), "w").write(reg)
    chg = open(os.path.join(pol_b, "CHANGELOG.md")).read()
    chg = "## 2026-10-21 — v1.1.0 — test H-P2-11 (P-003 passee declarative)\n\n" + chg
    open(os.path.join(pol_b, "CHANGELOG.md"), "w").write(chg)

    data = os.path.join(work, "data-main")
    d = Daemon(binpath, data, os.path.join(evid, "daemon-main.log"), policy_dir=pol_a)
    assert d.wait_t1(), "boot principal en echec"
    db = store_db(data)

    def record_refusal(status, body):
        if isinstance(body, dict) and "error" in body and "trace_id" in body.get("error", {}):
            REFUSALS.append(body)

    # ---------- honnetete d'activation (design 0.1) ----------
    s, st = http(d.api, "GET", "/state")
    absent_during_proofs = st["anchor"]["mechanism"] == "ABSENT"

    # ---------- H-P2-1 : mutation autorisee reussie ----------
    s, r = d.claim(mission_id="m1", operation_id="op1", holder="holon-a",
                   capabilities={"permanent": ["store.write"], "promotions": []},
                   budget={"per_effect_caps": {"store.write": 3}})
    env1 = r["envelope"]
    ok = (s == 200 and env1["lease"]["fencing_token"] == 1
          and "L05@v1.0.0" in env1["policy_ref"] and "P-001@v1" in env1["policy_ref"]
          and env1["evidence_ref"] == f"trace:{r['trace_id']}")
    s, e = d.effect(env1, required_capability="store.write")
    ok = ok and s == 200 and e["authorized"] and e["proof_state"] == "CURRENT"
    tr = traces(db)
    kinds = [k for _, k, _ in tr]
    ok = ok and kinds.index("ISSUE") < kinds.index("AUTHORIZE")
    note("H-P2-1", ok, f"bail emis (token 1, policy_ref epingle {env1['policy_ref']}) ; mutation autorisee -> effet #{e.get('effect_id')} ; trace ISSUE avant AUTHORIZE")

    # ---------- H-P2-2 : bail perime refuse au point d'effet ----------
    s, r = d.claim(mission_id="m2", operation_id="op1", holder="holon-b", ttl_s=1,
                   budget={"per_effect_caps": {"store.write": 3}})
    env2 = r["envelope"]
    time.sleep(2.2)  # ttl 1 s, horloge a la seconde : marge sure
    s, e = d.effect(env2)
    record_refusal(s, e)
    n_eff = db.execute("SELECT COUNT(*) FROM effects WHERE mission_id='m2'").fetchone()[0]
    ok = s == 403 and e["error"]["code"] == "LEASE_EXPIRED" and n_eff == 0
    note("H-P2-2", ok, "enveloppe autrefois valide presentee apres expires_at -> 403 LEASE_EXPIRED, zero effet porte")

    # ---------- H-P2-3 + H-P2-5 : reprise sans double effet + fencing ----------
    s, r = d.claim(mission_id="m3", operation_id="op1", holder="holder-1",
                   budget={"per_effect_caps": {"store.write": 5}})
    env3a = r["envelope"]
    s, e1 = d.effect(env3a, head_sha="h" * 40)
    # holder-1 meurt en cours de mission (abandon) ; reprise par bail successeur
    s, r = d.claim(mission_id="m3", operation_id="op1", holder="holder-2", mode="successor",
                   budget={"per_effect_caps": {"store.write": 5}})
    env3b = r["envelope"]
    ok = env3b["lease"]["fencing_token"] == 2
    s, e_old = d.effect(env3a, head_sha="h" * 40)
    record_refusal(s, e_old)
    ok = ok and s == 403 and e_old["error"]["code"] == "FENCED"
    note("H-P2-5", ok, "ancien holder, ancien token, apres bail successeur -> 403 FENCED au point d'effet")
    s, proof = http(d.api, "GET", "/envelope/proof?mission_id=m3&operation_id=op1")
    ok = proof["found"] and proof["head_sha"] == "h" * 40 and proof["proof_state"] == "CURRENT"
    s, e2 = d.effect(env3b, head_sha="h" * 40)
    rows = db.execute("SELECT COUNT(*), COUNT(DISTINCT id) FROM effects WHERE mission_id='m3'").fetchone()
    ok = ok and s == 200 and rows[0] == 2
    note("H-P2-3", ok, f"reconciliation sur l'etat reel (e1 present, non rejoue) ; e2 porte une fois ; effets m3 = {rows[0]} (1 + 1, aucun doublon)")

    # ---------- H-P2-4 : promotion expiree effectivement retiree ----------
    now = int(time.time())
    s, r = d.claim(mission_id="m4", operation_id="step-1", holder="harness",
                   capabilities={"permanent": [], "promotions": [
                       {"capability": "credentials.step", "granted_by": "harness", "expires_at_unix": now + 2}]},
                   budget={"per_effect_caps": {"store.write": 5}})
    env4 = r["envelope"]
    s, e = d.effect(env4, required_capability="credentials.step")
    ok = s == 200
    time.sleep(3.0)  # promotion a +2 s, horloge a la seconde : marge sure
    s, e = d.effect(env4, required_capability="credentials.step")
    record_refusal(s, e)
    n_eff = db.execute("SELECT COUNT(*) FROM effects WHERE mission_id='m4'").fetchone()[0]
    ok = ok and s == 403 and e["error"]["code"] == "PROMOTION_EXPIRED" and n_eff == 1
    note("H-P2-4", ok, "cas step-credentials du PRD 3.3 : effet sous promotion vivante OK ; apres expiration -> 403 PROMOTION_EXPIRED, aucun nouvel effet (retrait effectif, prouve par execution)")

    # ---------- H-P2-6 : budget par effet + classe paid_fallback a 0 ----------
    s, r = d.claim(mission_id="m5", operation_id="op1", holder="h",
                   budget={"per_effect_caps": {"store.write": 1}})
    env5 = r["envelope"]
    s, e = d.effect(env5)
    s, e = d.effect(env5)
    record_refusal(s, e)
    ok = s == 403 and e["error"]["code"] == "BUDGET_EXCEEDED"
    s, r = d.claim(mission_id="m6", operation_id="op1", holder="h",
                   budget={"per_effect_caps": {"paid_fallback": 99}})
    env6 = r["envelope"]
    s, e = d.effect(env6, effect_class="paid_fallback")
    record_refusal(s, e)
    ok = ok and s == 403 and e["error"]["code"] == "BUDGET_EXCEEDED" and e["error"].get("trace_id") is not None
    det = db.execute("SELECT detail_json FROM authority_traces WHERE id=?", (e["error"]["trace_id"],)).fetchone()
    ok = ok and det and json.loads(det[0])["detail"]["cap"] == 0
    note("H-P2-6", ok, "plafond 1 consomme -> 403 BUDGET_EXCEEDED ; classe paid_fallback : plafond mecanique 0 malgre un cap declare de 99 (design 0.6)")

    # ---------- H-P2-7 : liaison de preuve au head_sha ----------
    s, r = d.claim(mission_id="m7", operation_id="op-pr", holder="h",
                   budget={"per_effect_caps": {"store.write": 5}})
    env7 = r["envelope"]
    s, e = d.effect(env7, head_sha="A" * 40)
    s, proof = http(d.api, "GET", "/envelope/proof?mission_id=m7&operation_id=op-pr")
    ok = proof["head_sha"] == "A" * 40 and proof["proof_state"] == "CURRENT"
    s, e = d.effect(env7, head_sha="B" * 40)
    s, proof = http(d.api, "GET", "/envelope/proof?mission_id=m7&operation_id=op-pr")
    obs = [o["head_sha"] for o in proof["obsolete"]]
    ok = ok and proof["head_sha"] == "B" * 40 and ("A" * 40) in obs
    ok = ok and any(k == "OBSOLETE" for _, k, _ in traces(db))
    note("H-P2-7", ok, "mutation sur tete A ; la PR passe en tete B -> preuve de A marquee OBSOLETE (trace OBSOLETE), jamais portee en silence sur B")

    # ---------- H-P2-8 : claim orphelin + forme avant fond ----------
    forged = {"work_id": "w", "mission_id": "m8", "operation_id": "op1", "correlation_id": "c",
              "lease": {"holder": "ghost", "fencing_token": 1, "issued_at_unix": now, "expires_at_unix": now + 600},
              "budget": {"per_effect_caps": {"store.write": 5}},
              "capabilities": {"permanent": [], "promotions": []},
              "policy_ref": "L05@v1.0.0", "evidence_ref": "trace:0", "return_to": "channel:x"}
    s, e = d.effect(forged)
    record_refusal(s, e)
    ok = s == 403 and e["error"]["code"] == "LEASE_UNKNOWN"
    s, r = d.claim(mission_id="m8", operation_id="op1", holder="h", return_to=None)
    # return_to=None supprime le champ cote client via json null -> le service doit refuser la forme
    record_refusal(s, r)
    ok = ok and s == 400 and r["error"]["code"] == "ENVELOPE_MALFORMED"
    bad_env = dict(env1); bad_env.pop("return_to", None)
    s, e = d.effect(bad_env)
    record_refusal(s, e)
    ok = ok and s == 400 and e["error"]["code"] == "ENVELOPE_MALFORMED"
    s, e = d.effect(forged)
    record_refusal(s, e)
    ok = ok and s == 403 and e["error"]["code"] == "LEASE_UNKNOWN"
    note("H-P2-8", ok, "CLAIMED sans bail (enveloppe fabriquee) -> aucun effet (LEASE_UNKNOWN) ; forme refusee avant tout controle de bail (400) ; la reprise ne passe que par le vrai chemin (successeur + reconciliation, H-P2-3)")

    # ---------- H-P2-9 : tracabilite des refus ----------
    refuse_traces = {tid for tid, _, _ in traces(db, "REFUSE")}
    seen = [r["error"]["trace_id"] for r in REFUSALS]
    ok = len(seen) >= 6 and all(t in refuse_traces for t in seen)
    ok = ok and len(refuse_traces) == len(seen)
    note("H-P2-9", ok, f"{len(seen)} refus du service, {len(refuse_traces)} traces REFUSE au store — correspondance 1:1, chaque refus trace avant son retour")

    # ---------- LEASE_HELD (unicite du bail vivant, PRD section 5) ----------
    s, r = d.claim(mission_id="m1", operation_id="op2", holder="autre")
    record_refusal(s, r)
    ok = s == 409 and r["error"]["code"] == "LEASE_HELD"
    refuse_traces = {tid for tid, _, _ in traces(db, "REFUSE")}
    ok = ok and r["error"]["trace_id"] in refuse_traces
    note("H-P2-5b", ok, "second claim initial pendant un bail vivant -> 409 LEASE_HELD (un seul bail vivant par portee de mission)")

    d.stop()
    db.close()

    # ---------- H-P2-11 : la politique n'habite pas le runtime ----------
    src = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(args.bin))), "..")
    src = os.path.abspath(src)
    hits = []
    sdir = os.path.join(src, "src")
    for f in os.listdir(sdir):
        text = open(os.path.join(sdir, f)).read()
        for pat in (r"P-00[1-4]", r"OPPOSABLE", r"Sans reçu", r"swarm sans", r"secret-scan"):
            if re.search(pat, text):
                hits.append(f"{f}:{pat}")
    d2 = Daemon(binpath, os.path.join(work, "data-polb"), os.path.join(evid, "daemon-polb.log"), policy_dir=pol_b)
    assert d2.wait_t1()
    s, r = d2.claim(mission_id="mx", operation_id="op", holder="h")
    d2.stop()
    db2 = store_db(os.path.join(work, "data-polb"))
    tr2 = db2.execute("SELECT detail_json FROM authority_traces WHERE kind='REFUSE'").fetchall()
    db2.close()
    pin_in_trace = any("v1.1.0" in json.loads(t[0]).get("detail", {}).get("policy_ref_read", "") for t in tr2)
    ok = (not hits and s == 403 and r["error"]["code"] == "POLICY_NOT_IN_FORCE"
          and "v1.1.0" in json.dumps(r) and pin_in_trace)
    note("H-P2-11", ok, f"grep des sources : {len(hits)} constante de politique {hits[:3]} ; meme binaire (sha256 inchange) + store modifie (P-003 passee declarative, v1.1.0) -> claim refuse POLICY_NOT_IN_FORCE, version lue epinglee dans la reponse ET dans la trace R10")

    # ---------- activation (design 0.1) + persistance des baux au restart ----------
    env = dict(os.environ); env["NOYAU_DATA_DIR"] = data
    act = subprocess.run([binpath, "anchor-activate"], env=env, capture_output=True, text=True, timeout=20)
    d3 = Daemon(binpath, data, os.path.join(evid, "daemon-restart.log"), policy_dir=pol_a)
    assert d3.wait_t1()
    s, st = http(d3.api, "GET", "/state")
    mech = st["anchor"]["mechanism"]
    s, e = d3.effect(env1, required_capability="store.write")
    ok = act.returncode == 0 and absent_during_proofs and mech == "ACTIVE" and s == 200
    db3 = store_db(data)
    ok = ok and any(k == "ANCHOR_ACTIVATED" for _, k, _ in traces(db3))
    db3.close()
    d3.stop()
    note("H-P2-0", ok, "mecanisme rapporte ABSENT pendant toutes les preuves ; acte d'activation trace (ANCHOR_ACTIVATED) -> ACTIVE ; les baux survivent au redemarrage (etat au store, jamais en memoire seule)")

    print(f"\nBinaire : sha256 {bin_hash[:16]}…")
    with open(os.path.join(evid, "RESULTS-P2.md"), "w") as fh:
        fh.write("# Resultats des hold-outs H-P2 — service d'enveloppe (Phase 2, D-N038)\n\n")
        for hid, statut, preuve in RESULTS:
            fh.write(f"- **{hid}** : {statut} — {preuve}\n")
    fails = [h for h, s_, _ in RESULTS if s_ == "FAIL"]
    print(f"Bilan : {len(RESULTS) - len(fails)}/{len(RESULTS)} PASS ; echecs : {fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
