#!/usr/bin/env python3
"""Compilateur des profils d'autorite des cinq Apps (D-N038, patron #557).

Lit l'etat desire versionne (authority_profiles.json, design signe de
Clara : NOYAU-A1-phase2-design-profils-apps-clara.md) et produit, par
profil, un manifeste ne portant QUE les octrois (deny-by-default : toute
permission hors matrice resout a « No access » par absence), plus un
rapport de resolution complet (univers des permissions x profils).

Fail-closed : une permission inconnue de l'univers, un niveau inconnu,
ou un interdit accorde par la matrice arretent la compilation (exit 1).
Aucun effet externe : ce compilateur ecrit des fichiers locaux, rien
d'autre (l'enrolement des Apps est une qualification separee — l'existence
live des Apps est un inconnu nomme, PRD Phase 2 section 4.2).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Univers des permissions d'une GitHub App (surface repository + organisation
# usuelle) : le compilateur resout CHAQUE entree, pour chaque profil.
PERMISSION_UNIVERSE = [
    "actions", "administration", "checks", "contents", "deployments",
    "discussions", "environments", "issues", "members", "metadata",
    "organization_hooks", "packages", "pages", "pull_requests",
    "repository_hooks", "secrets", "statuses", "team_discussions",
    "variables", "webhooks", "workflows",
]


def main() -> int:
    src = HERE / "authority_profiles.json"
    desired = json.loads(src.read_text())
    levels = desired["levels"]
    forbidden = set(desired["forbidden_all_profiles"])
    out_dir = HERE / "manifests"
    out_dir.mkdir(exist_ok=True)
    resolution = {}
    for name, profile in sorted(desired["profiles"].items()):
        grants = {}
        resolved = {}
        for perm in PERMISSION_UNIVERSE:
            mark = profile["permissions"].get(perm)
            if mark is None:
                resolved[perm] = "No access"
                continue
            if mark not in levels:
                print(f"ECHEC: niveau inconnu {mark!r} pour {name}.{perm}", file=sys.stderr)
                return 1
            if perm in forbidden:
                print(f"ECHEC: la matrice accorde un interdit ({name}.{perm}) — refus de compiler", file=sys.stderr)
                return 1
            grants[perm] = levels[mark]
            resolved[perm] = levels[mark]
        for perm in profile["permissions"]:
            if perm not in PERMISSION_UNIVERSE:
                print(f"ECHEC: permission inconnue de l'univers ({name}.{perm}) — fail-closed", file=sys.stderr)
                return 1
        manifest = {
            "profile": name,
            "role": profile["role"],
            "desired_state_version": desired["version"],
            "permissions": dict(sorted(grants.items())),
            "default": "No access",
        }
        if profile.get("webhooks"):
            # Le champ se nomme « subscriptions » dans le manifeste compile :
            # « webhooks » est une permission interdite (gestion) au sens du
            # design section 3, et H-PA-2 greppe les manifestes tels quels —
            # les abonnements d'evenements ne sont pas la permission.
            manifest["subscriptions"] = profile["webhooks"]
        (out_dir / f"{name}.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        resolution[name] = resolved
        print(f"compile: {name} — {len(grants)} octrois, {len(PERMISSION_UNIVERSE) - len(grants)} resolutions No access")
    (out_dir / "resolution_report.json").write_text(
        json.dumps({"universe": PERMISSION_UNIVERSE, "resolution": resolution}, indent=2, sort_keys=True) + "\n")
    print(f"rapport de resolution : {out_dir / 'resolution_report.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
