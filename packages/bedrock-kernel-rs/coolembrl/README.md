# noyau — daemon du Noyau A1 (CoolEmbrl 1.0, Phase 1)

Binaire Rust unique du Noyau A1. Il démarre sous systemd, exécute la séquence
de boot T0→T1 instrumentée (étapes S0→S6), ouvre le store durable qualifié
(SQLite WAL), lève ses surfaces de lecture, arme le point d'ancrage de
l'enveloppe d'autorité en deny-by-default, et émet un **reçu de boot R10 à
chaque démarrage** — succès ou échec.

Documents de programme (dépôt de la Factory, `v4-factory/plans/`) :
PRD Phase 1 (13e Doctor), design signé de Clara (gate d'exécution),
MasterPlan du Noyau A1, qualification du stockage de Phase 0.

## Surfaces de T1 (liste fermée)

- API lecture seule : `GET /health`, `GET /boot`, `GET /state`.
- MCP (groupe `daemon`) : `daemon_status`, `boot_state`, `boot_receipt_last`.
  Aucun outil mutant n'est enregistré.
- Ancrage : `POST /anchor` ne sert que des **refus typés**
  (`ANCHOR_DENY_ALL`, `mechanism: ABSENT`, `capabilities_granted: 0`) —
  le mécanisme d'enveloppe (baux, fencing, budget) est la Phase 2 et n'est
  **pas simulé** ici.
- Watchdog : emplacement préparé, `NOT_INSTALLED` (service N2, Phase 4).
  `t2_ready_at` reste `null` sur chaque reçu de la phase.
- Battement : émis vers la trace L00 intérimaire (format intérimaire en
  vigueur) quand `NOYAU_L00_TRACE` est configuré.

## Configuration (environnement)

| Variable | Défaut | Rôle |
|---|---|---|
| `NOYAU_DATA_DIR` | `./noyau-data` | volume du daemon (store, journal, reçus) |
| `NOYAU_API_BIND` | `127.0.0.1:7474` | liaison de l'API |
| `NOYAU_MCP_BIND` | `127.0.0.1:7475` | liaison du serveur MCP |
| `NOYAU_BOARD_ID` | `amd64-generic` | matériel de référence porté aux reçus |
| `NOYAU_L00_TRACE` | — | chemin de la trace L00 intérimaire (battements) |
| `NOYAU_REGISTRY_PATH` | — | fichier d'enregistrement au registre H-04 |
| `NOYAU_HEARTBEAT_SECS` | `30` | période du battement |

Le boot complet se déroule **réseau coupé** : aucune étape n'attend un
service externe, une clé ou une résolution de nom. Le daemon refuse de
démarrer si son store vise une strate non persistante détectable
(tmpfs/ramfs) — reçu `FAILED(stratum)` consigné au journal de boot.

## Preuves

`scripts/run-holdouts.sh` exécute la batterie des hold-outs H-P1-1 → H-P1-12
et dépose les évidences (reçus, mesures, journaux) dans un répertoire
horodaté. Les chiffres de boot publiés sont **mesurés** (étiquette « mesuré »,
matériel de référence `amd64-generic`), jamais déclarés : la cible « T1 < 3 s »
est une cible de design, pas un fait.

## Provenance

Code original écrit pour ce programme (clean-room) : aucune reprise de code
d'un daemon tiers, aucun identifiant de marque tierce dans les sources ni
dans les artefacts livrés. Dépendances : crates publiques Rust (voir le
registre des licences du mono-repo, `licenses/`).
