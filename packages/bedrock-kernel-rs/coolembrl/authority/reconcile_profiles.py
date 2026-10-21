#!/usr/bin/env python3
"""Reconciliateur des profils des cinq Apps (D-N038, patron #566).

Diffe l'etat courant -> l'etat desire (manifestes compiles). Regles du
design (section 4) : detection de tout elargissement, fail-closed sur
permission inconnue, garde --approve sous recu R10 d'un Doctor avant
tout effet externe.

Etat courant : l'existence live des Apps est un INCONNU NOMME (PRD
Phase 2, section 4.2 — enregistrement/installation jamais demontre, non
observable avec une authentification utilisateur). Sans fichier d'etat
courant fourni (--current), le reconciliateur rapporte donc
current = UNOBSERVED et ne pretend aucun diff ligne a ligne contre du
live. L'effet externe (--approve) est refuse par construction dans
cette phase : l'enrolement entrera par une qualification ecrite
separee, jamais par presomption.
"""
import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RANK = {"No access": 0, "read": 1, "write": 2}


def load_manifests():
    out = {}
    for f in sorted((HERE / "manifests").glob("*.json")):
        if f.name == "resolution_report.json":
            continue
        data = json.loads(f.read_text())
        out[data["profile"]] = data
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--current", help="fichier JSON d'etat courant observe (profils -> permissions)")
    ap.add_argument("--approve", action="store_true", help="appliquer le diff (effet externe)")
    ap.add_argument("--receipt", help="reference du recu R10 d'un Doctor (garde --approve)")
    args = ap.parse_args()

    desired = load_manifests()
    if not desired:
        print("ECHEC: aucun manifeste compile — lancer compile_profiles.py d'abord", file=sys.stderr)
        return 1

    if args.approve:
        if not args.receipt:
            print("REFUSE: --approve exige un recu R10 d'un Doctor (--receipt) — garde du design section 4.", file=sys.stderr)
            return 2
        print("REFUSE: aucun effet externe dans cette phase — l'existence live des Apps est un "
              "inconnu nomme et leur enrolement releve d'une qualification ecrite separee "
              "(PRD Phase 2, section 4.2). Le diff ci-dessous est le seul produit.", file=sys.stderr)
        # on continue vers le rapport de diff, sans effet

    if not args.current:
        print("current: UNOBSERVED (inconnu nomme — aucune App observable avec l'authentification disponible)")
        for name, man in desired.items():
            n = len(man["permissions"])
            print(f"  {name}: desire {n} octrois, etat live non prouve — l'integralite de l'etat desire "
                  f"reste a etablir par la qualification d'enrolement.")
        print("diff: non calculable contre un etat non observe ; aucune derive detectee ni exclue.")
        return 0

    current = json.loads(Path(args.current).read_text())
    widenings, reductions, unknown = [], [], []
    for name, man in desired.items():
        want = man["permissions"]
        have = current.get(name, {})
        for perm in sorted(set(want) | set(have)):
            if perm not in RANK and perm not in ("read", "write"):
                pass
            w = want.get(perm, "No access")
            h = have.get(perm, "No access")
            if h not in RANK or w not in RANK:
                unknown.append(f"{name}.{perm}: niveau inconnu (current={h!r}, desired={w!r}) — fail-closed")
                continue
            if RANK[h] > RANK[w]:
                widenings.append(f"{name}.{perm}: current {h} > desire {w} (ELARGISSEMENT)")
            elif RANK[h] < RANK[w]:
                reductions.append(f"{name}.{perm}: current {h} -> desire {w} (a accorder par l'enrolement)")
    for line in widenings:
        print("  " + line)
    for line in reductions:
        print("  " + line)
    for line in unknown:
        print("  " + line, file=sys.stderr)
    if unknown:
        return 1
    print(f"diff: {len(widenings)} elargissement(s), {len(reductions)} octroi(s) en attente, 0 effet applique.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
