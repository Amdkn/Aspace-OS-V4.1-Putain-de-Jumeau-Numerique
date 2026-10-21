#!/usr/bin/env python3
"""Verification des hold-outs H-PA-1 -> H-PA-4 du design « profils des
cinq Apps » (Clara, signe) contre les manifestes compiles par
compile_profiles.py. Exit 0 si les quatre tiennent.
"""
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = []


def note(hid, ok, preuve):
    RESULTS.append((hid, ok))
    print(f"[{'PASS' if ok else 'FAIL'}] {hid} — {preuve}")


def main() -> int:
    r = subprocess.run([sys.executable, str(HERE / "compile_profiles.py")],
                       capture_output=True, text=True)
    if r.returncode != 0:
        print(r.stdout + r.stderr)
        print("compilation en echec — hold-outs non evaluables")
        return 1
    desired = json.loads((HERE / "authority_profiles.json").read_text())
    report = json.loads((HERE / "manifests" / "resolution_report.json").read_text())
    manifests = {}
    for name in desired["profiles"]:
        manifests[name] = json.loads((HERE / "manifests" / f"{name}.json").read_text())

    # H-PA-1 : deny-by-default compile — toute permission hors matrice
    # resout a No access dans le rapport de resolution, et n'apparait
    # pas dans les octrois du manifeste.
    bad = []
    for name, prof in desired["profiles"].items():
        for perm in report["universe"]:
            resolved = report["resolution"][name][perm]
            granted = manifests[name]["permissions"].get(perm)
            if perm not in prof["permissions"]:
                if resolved != "No access" or granted is not None:
                    bad.append(f"{name}.{perm}")
            else:
                want = desired["levels"][prof["permissions"][perm]]
                if resolved != want or granted != want:
                    bad.append(f"{name}.{perm}")
    note("H-PA-1", not bad,
         f"resolution complete de l'univers ({len(report['universe'])} permissions x 5 profils) : hors-matrice = No access partout ; ecarts : {bad[:4]}")

    # H-PA-2 : aucun interdit present dans les manifestes, a tout niveau.
    found = []
    for name, man in manifests.items():
        for perm in desired["forbidden_all_profiles"]:
            if perm in man["permissions"]:
                found.append(f"{name}.{perm}={man['permissions'][perm]}")
        text = json.dumps(man)
        for perm in desired["forbidden_all_profiles"]:
            if f'"{perm}"' in text:
                found.append(f"{name}: occurrence textuelle de {perm}")
    note("H-PA-2", not found,
         f"grep des cinq manifestes : zero occurrence des interdits {desired['forbidden_all_profiles']} ; trouvailles : {found[:4]}")

    # H-PA-3 : evenements sans orphelin — le manifeste gateway porte
    # exactement les evenements du design, chacun avec sa boucle.
    gw = manifests["gateway"]["subscriptions"]
    src_events = {w["event"]: w["consumer"] for w in desired["profiles"]["gateway"]["webhooks"]}
    man_events = {w["event"]: w["consumer"] for w in gw}
    note("H-PA-3", src_events == man_events and "push" not in man_events and len(gw) == 10,
         f"gateway : {len(gw)} evenements abonnes, chacun nomme avec sa boucle consommatrice ; 'push' absent (exclu deliberement)")

    # H-PA-4 : le correctif S2 est dans l'etat desire compile.
    note("H-PA-4", manifests["s2"]["permissions"].get("contents") == "write",
         "manifeste compile de s2 : contents = write (le no-human-gate est executable par l'identite d'App)")

    fails = [h for h, ok in RESULTS if not ok]
    print(f"Bilan H-PA : {len(RESULTS) - len(fails)}/{len(RESULTS)} PASS ; echecs : {fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
