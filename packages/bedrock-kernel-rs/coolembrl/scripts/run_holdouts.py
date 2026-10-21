#!/usr/bin/env python3
"""Batterie de preuves des hold-outs H-P1-1 -> H-P1-12 (Phase 1 du Noyau A1).

Chaque hold-out est execute contre le binaire reel et produit des evidences
(recus, reponses, mesures) dans le repertoire de sortie. Aucun resultat n'est
declare : il est mesure et consigne dans RESULTS.md.

Usage : python3 scripts/run_holdouts.py [--bin target/release/noyau] [--out DIR]
Prerequis : binaire release deja construit ; python3 ; curl non requis.
"""
import argparse
import json
import os
import re
import shutil
import signal
import socket
import sqlite3
import statistics
import subprocess
import sys
import time
import urllib.request
import urllib.error

RESULTS = []  # (id, statut, preuve)


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
    except Exception as e:
        raise RuntimeError(f"{method} {url}: {e}")


class Daemon:
    _port_counter = 0

    def __init__(self, binpath, data_dir, evidence_dir, extra_env=None, heartbeat=None, trace=None, registry=None):
        Daemon._port_counter += 1
        base = 18000 + Daemon._port_counter * 10
        self.api = f"127.0.0.1:{base}"
        self.mcp = f"127.0.0.1:{base + 1}"
        self.data_dir = data_dir
        env = dict(os.environ)
        env.update({
            "NOYAU_DATA_DIR": data_dir,
            "NOYAU_API_BIND": self.api,
            "NOYAU_MCP_BIND": self.mcp,
            "NOYAU_BOARD_ID": "amd64-generic",
        })
        if heartbeat:
            env["NOYAU_HEARTBEAT_SECS"] = str(heartbeat)
        if trace:
            env["NOYAU_L00_TRACE"] = trace
        if registry:
            env["NOYAU_REGISTRY_PATH"] = registry
        if extra_env:
            env.update(extra_env)
        self.log_path = os.path.join(evidence_dir, f"daemon-{Daemon._port_counter}.log")
        self.log = open(self.log_path, "wb")
        self.proc = subprocess.Popen([binpath], env=env, stdout=self.log, stderr=subprocess.STDOUT)

    def wait_t1(self, timeout=10):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                return False
            try:
                status, body = http(self.api, "GET", "/health")
                if status == 200 and body and body.get("stage") == "T1":
                    return True
            except Exception:
                pass
            time.sleep(0.05)
        return False

    def receipts(self):
        rdir = os.path.join(self.data_dir, "receipts")
        out = []
        if os.path.isdir(rdir):
            for f in sorted(os.listdir(rdir)):
                with open(os.path.join(rdir, f)) as fh:
                    out.append(json.load(fh))
        out.sort(key=lambda r: (r.get("t0_at", ""), r.get("boot_id", "")))
        return out

    def stop(self, sig=signal.SIGTERM):
        if self.proc.poll() is None:
            self.proc.send_signal(sig)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        self.log.close()
        return self.proc.returncode


def fresh_dir(path):
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path, exist_ok=True)
    return path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/noyau")
    ap.add_argument("--out", default="holdout-evidence")
    ap.add_argument("--workdir", default="/home/hatch/workspace/noyau-holdout-work")
    args = ap.parse_args()
    binpath = os.path.abspath(args.bin)
    evid = fresh_dir(os.path.abspath(args.out))
    work = fresh_dir(args.workdir)
    assert os.access(binpath, os.X_OK), f"binaire introuvable: {binpath}"

    # ---------- H-P1-7 (d'abord : il conditionne le choix des repertoires) ----------
    shm_dir = "/dev/shm/noyau-holdout-stratum"
    shutil.rmtree(shm_dir, ignore_errors=True)
    env = dict(os.environ, NOYAU_DATA_DIR=shm_dir)
    p = subprocess.run([binpath], env=env, capture_output=True, text=True, timeout=20)
    stratum_receipt = None
    rdir = os.path.join(shm_dir, "receipts")
    if os.path.isdir(rdir):
        for f in os.listdir(rdir):
            stratum_receipt = json.load(open(os.path.join(rdir, f)))
    ok = (p.returncode != 0 and stratum_receipt
          and stratum_receipt["outcome"]["FAILED"]["stage"] == "stratum")
    note("H-P1-7", bool(ok), f"store sur /dev/shm (tmpfs) -> exit {p.returncode}, recu FAILED(stratum)")

    # ---------- boots nominaux + H-P1-1 (serie de recus) ----------
    data = os.path.join(work, "data-main")
    boots_receipts = []
    d = Daemon(binpath, data, evid)
    assert d.wait_t1(), "boot nominal initial en echec"
    boots_receipts += d.receipts()
    d.stop()

    # ---------- H-P1-2 : 10 boots consecutifs, d_t1_ms ----------
    mesures = []
    for i in range(10):
        dd = Daemon(binpath, data, evid)
        assert dd.wait_t1(), f"boot {i} en echec"
        r = dd.receipts()[-1]
        mesures.append(r["d_t1_ms"])
        boots_receipts.append(r)
        dd.stop()
    med = statistics.median(mesures)
    mx = max(mesures)
    note("H-P1-2", mx < 3000, f"10 boots : d_t1_ms median={med} max={mx} (cible < 3000, MESURE sur cette machine amd64)")

    # ---------- H-P1-9 : aucun faux T2 sur la serie ----------
    ok = all(r["t2_ready_at"] is None for r in boots_receipts)
    note("H-P1-9", ok, f"t2_ready_at null sur {len(boots_receipts)} recus ; watchdog NOT_INSTALLED verifie a /state")

    note("H-P1-1", len([r for r in boots_receipts if r["outcome"] == "T1_REACHED"]) >= 10 and stratum_receipt is not None,
         f"serie de recus au journal : {len(boots_receipts)} T1 + 1 FAILED provoque (stratum)")

    # ---------- H-P1-3 + H-P1-8 : mutations et honnetete de l'ancrage ----------
    d = Daemon(binpath, data, evid)
    # Sonde pendant T0 : aucune surface ne repond encore utilement.
    t0_refused = False
    try:
        s, _ = http(d.api, "POST", "/anchor", {"kind": "lease-request"})
        t0_refused = s == 403
    except Exception:
        t0_refused = True  # connexion refusee = aucune mutation servie avant T1
    assert d.wait_t1()
    s1, b1 = http(d.api, "POST", "/anchor", {"kind": "lease-request", "capability": "store.write", "ttl_s": 60})
    s2, b2 = http(d.api, "POST", "/anchor", {"kind": "capability-grant", "capability": "store.write"})
    s3, b3 = http(d.mcp, "POST", "/", {"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                                       "params": {"name": "store_write", "arguments": {}}})
    st, state = http(d.api, "GET", "/state")
    anchor = state["anchor"]
    lease_shaped = any(k in json.dumps(b1) for k in ("lease_id", "fencing_token", "\"granted\": true"))
    ok3 = t0_refused and s1 == 403 and s2 == 403
    note("H-P1-3", ok3, f"mutation-sonde pendant T0 : refusee ; en T1 : POST /anchor -> {s1}/{s2} types ; outil mutant MCP : isError={b3['result']['isError']}")
    ok8 = (s1 == 403 and b1["error"]["code"] == "ANCHOR_DENY_ALL"
           and anchor["mechanism"] == "ABSENT" and anchor["capabilities_granted"] == 0
           and not lease_shaped)
    note("H-P1-8", ok8, "requete en forme de bail + demande d'octroi -> refus types ANCHOR_DENY_ALL ; mechanism ABSENT, capabilities_granted 0 ; aucune reponse en forme de bail")
    # /state n'a jamais rapporte T2
    note("H-P1-9b", state["stage"] == "T1" and state["watchdog"] == "NOT_INSTALLED", "etat rapporte : T1, watchdog NOT_INSTALLED")
    d.stop()

    # ---------- H-P1-4 : kill -9 pendant le boot, recuperation ----------
    data4 = os.path.join(work, "data-kill")
    d = Daemon(binpath, data4, evid)
    assert d.wait_t1()
    d.stop()
    db = os.path.join(data4, "noyau-store.sqlite")
    conn = sqlite3.connect(db)
    for i in range(5):
        conn.execute("INSERT INTO state_entries(kind, correlation_id, effect_status) VALUES('seed', ?, ?)",
                     (f"corr-{i}", "ATTEMPTED_UNKNOWN" if i == 0 else "NONE"))
    conn.commit(); conn.close()
    killed_clean = False
    for _ in range(6):
        dd = Daemon(binpath, data4, evid)
        time.sleep(0.03)  # pendant le boot / le rejeu
        dd.stop(sig=signal.SIGKILL)
        killed_clean = True
    dd = Daemon(binpath, data4, evid)
    assert dd.wait_t1(), "boot de recuperation en echec"
    r = dd.receipts()[-1]
    st, state = http(dd.api, "GET", "/state")
    conn = sqlite3.connect(db)
    rows = conn.execute("SELECT COUNT(*) FROM state_entries").fetchone()[0]
    conn.close()
    anomalies = " ".join(r["anomalies"])
    ok = (killed_clean and "previous-boot-unterminated" in anomalies
          and r["replay"]["hops_replayed"] == 5 and r["replay"]["effects_unknown_pending"] == 1
          and rows == 5)
    note("H-P1-4", ok, f"kill -9 repete pendant le boot ; boot suivant : anomalie de recuperation au recu, rejeu 5 hops / 1 inconnu COMPTE (non resolu), etat canonique identique ({rows} lignes)")
    dd.stop()

    # ---------- H-P1-12 : injection de faute MCP ----------
    d = Daemon(binpath, os.path.join(work, "data-probe"), evid)
    assert d.wait_t1()
    mcp_port = int(d.mcp.split(":")[1])
    d.stop()
    blocker = socket.socket()
    blocker.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    blocker.bind(("127.0.0.1", mcp_port))
    blocker.listen(1)
    # Reutiliser les memes ports : instancier a la main
    env = dict(os.environ, NOYAU_DATA_DIR=os.path.join(work, "data-probe"),
               NOYAU_API_BIND=f"127.0.0.1:{mcp_port + 100}", NOYAU_MCP_BIND=f"127.0.0.1:{mcp_port}")
    p = subprocess.run([binpath], env=env, capture_output=True, text=True, timeout=20)
    blocker.close()
    rec = None
    rdir = os.path.join(work, "data-probe", "receipts")
    for f in sorted(os.listdir(rdir)):
        cand = json.load(open(os.path.join(rdir, f)))
        if isinstance(cand.get("outcome"), dict) and cand["outcome"].get("FAILED", {}).get("stage") == "probe-mcp":
            rec = cand
    assert rec is not None, "aucun recu FAILED(probe-mcp) trouve"
    ok = (p.returncode != 0 and isinstance(rec["outcome"], dict)
          and rec["outcome"]["FAILED"]["stage"] == "probe-mcp" and rec["t1_at"] is None)
    note("H-P1-12", ok, f"port MCP occupe -> exit {p.returncode}, recu FAILED(probe-mcp), aucun T1 declare")

    # ---------- H-P1-10 : battement interimaire ----------
    trace = os.path.join(evid, "l00-trace-demo.md")
    open(trace, "w").write("# Trace L00 interimaire (demonstration)\n")
    d = Daemon(binpath, os.path.join(work, "data-hb"), evid, heartbeat=2, trace=trace)
    assert d.wait_t1()
    time.sleep(7)
    beats1 = len([l for l in open(trace) if "noyau · battement" in l])
    d.stop()
    size_after_stop = os.path.getsize(trace)
    time.sleep(5)
    beats_stopped = os.path.getsize(trace) == size_after_stop
    ok = beats1 >= 2 and beats_stopped
    note("H-P1-10", ok, f"{beats1} battements au format interimaire dans la trace ; daemon arrete -> plus aucun battement (absence lisible)")

    # ---------- H-P1-6 : boot reseau coupe (namespace si permis) ----------
    netns_ok = None
    try:
        test = subprocess.run(["unshare", "-n", "true"], capture_output=True, timeout=10)
        if test.returncode == 0:
            data6 = os.path.join(work, "data-netns")
            log6 = os.path.join(evid, "netns-daemon.log")
            cmd = ("ip link set lo up 2>/dev/null; "
                   f"NOYAU_DATA_DIR={data6} NOYAU_API_BIND=127.0.0.1:19741 NOYAU_MCP_BIND=127.0.0.1:19742 "
                   f"{binpath} > {log6} 2>&1 & sleep 3; curl -s --max-time 4 http://127.0.0.1:19741/health; kill -TERM %1; sleep 2")
            r = subprocess.run(["unshare", "-n", "bash", "-c", cmd], capture_output=True, text=True, timeout=30)
            netns_ok = '"stage": "T1"' in r.stdout or '"stage":"T1"' in r.stdout
            note("H-P1-6", bool(netns_ok), "boot complet T0->T1 dans un namespace reseau isole (loopback seule)")
        else:
            raise PermissionError("unshare -n refuse")
    except Exception as e:
        note("H-P1-6", False, f"NON EXECUTABLE dans cet environnement ({e}) — degradation honnete : le daemon ne lie que loopback et n'initie aucune connexion externe (construction) ; preuve namespace a refaire sur la machine de reference")

    # ---------- H-P1-5 + H-P1-11 : hygiene clean-room ----------
    pkg = os.path.dirname(os.path.dirname(os.path.abspath(args.bin)))  # racine du paquet (target/..)
    pkg = os.path.dirname(pkg)
    hits = []
    for root, dirs, files in os.walk(pkg):
        dirs[:] = [x for x in dirs if x not in ("target", ".git", "holdout-evidence")]
        for f in files:
            if f == "run_holdouts.py":
                continue  # le harnais nomme le motif qu'il cherche : exclu de son propre grep, par transparence
            path = os.path.join(root, f)
            try:
                with open(path, encoding="utf-8", errors="ignore") as fh:
                    for i, line in enumerate(fh, 1):
                        if re.search(r"umbrel", line, re.IGNORECASE):
                            hits.append(f"{os.path.relpath(path, pkg)}:{i}")
            except Exception:
                pass
    strings = subprocess.run(["strings", binpath], capture_output=True, text=True).stdout
    bin_hits = [l for l in strings.splitlines() if re.search(r"umbrel", l, re.IGNORECASE)]
    note("H-P1-5", not hits, f"grep de l'arbre source du paquet : {len(hits)} identifiant interdit {hits[:3]}")
    note("H-P1-11", not bin_hits, f"artefacts livres : binaire (strings) {len(bin_hits)} occurrence ; unit, manifeste et routes verifies dans le meme grep d'arbre")

    # ---------- bilan ----------
    with open(os.path.join(evid, "RESULTS.md"), "w") as fh:
        fh.write("# Resultats des hold-outs H-P1 — daemon noyau (Phase 1, D-N037)\n\n")
        fh.write("Chaque ligne est un resultat MESURE contre le binaire reel, evidences dans ce repertoire.\n\n")
        for hid, statut, preuve in RESULTS:
            fh.write(f"- **{hid}** : {statut} — {preuve}\n")
    fails = [h for h, s, _ in RESULTS if s == "FAIL"]
    print(f"\nBilan : {len(RESULTS) - len(fails)}/{len(RESULTS)} PASS ; echecs : {fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
