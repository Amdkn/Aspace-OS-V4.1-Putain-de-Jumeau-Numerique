# noyau — daemon du Noyau A1 (CoolEmbrl 1.0, Phases 1–2)

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
  (`ANCHOR_DENY_ALL`) — la surface brute ne sert aucune mutation ; les
  mutations passent par le service d'enveloppe (Phase 2, ci-dessous).
  L'état rapporté de l'ancrage est l'état réel : `mechanism: ABSENT`
  jusqu'à l'acte d'activation post-preuves, `capabilities_granted` =
  capacités des baux vivants.
- Watchdog : emplacement préparé, `NOT_INSTALLED` (service N2, Phase 4).
  `t2_ready_at` reste `null` sur chaque reçu de la phase.
- Battement : émis vers la trace L00 intérimaire (format intérimaire en
  vigueur) quand `NOYAU_L00_TRACE` est configuré.

## Service d'enveloppe — Phase 2 (D-N038)

Le point d'ancrage a reçu son mécanisme (PRD Phase 2 du 13e, design
signé de Clara) : point de contrôle unique à la frontière du daemon.

- `POST /envelope/claim` — émission d'un bail (mapping CLAIMED → Lease) :
  fencing token strictement croissant par mission, émis par le service
  seul ; un seul bail vivant par portée de mission ; la politique L05
  est **lue au moment de la décision** (`NOYAU_POLICY_DIR`) et sa
  version est **épinglée** dans l'enveloppe (`policy_ref`).
- `POST /envelope/effect` — présentation d'une mutation au point
  d'effet : forme stricte d'abord, puis bail vivant, fencing courant,
  budget par effet (la classe `paid_fallback` est plafonnée à 0 par
  mécanique — design §0.6), politique relue, promotion vivante si la
  capacité l'exige. La preuve est **liée au `head_sha`** : une nouvelle
  tête marque l'ancienne preuve OBSOLETE, jamais portée en silence.
- `GET /envelope/proof`, `GET /envelope/state` — preuve et état.
- **Trace avant effet** : chaque émission, refus, expiration et retrait
  écrit sa trace au store (`authority_traces`) avant l'effet ou le
  retour du refus ; un refus non tracé est une violation de design.
- **Activation honnête** : `noyau anchor-activate` (acte d'opérateur,
  tracé au store) ne se pose qu'après les preuves H-P2 — jamais avant.

Les **profils des cinq Apps** (matrice V4 réécrite contre les surfaces
réelles, design signé de Clara) vivent sous `authority/` : état désiré
versionné (`authority_profiles.json`), compilateur deny-by-default
(`compile_profiles.py`), réconciliateur fail-closed
(`reconcile_profiles.py`), vérification H-PA (`check_profiles.py`) et
réconciliation de l'inventaire des promotions
(`INVENTORY-RECONCILIATION.md`, H-P2-10). L'existence live des Apps
reste un **inconnu nommé** : aucun enrôlement n'est supposé ni exécuté
dans cette phase.

## WorkGraph type — Phase 3 (D-N039)

Le daemon tient l'**état d'exécution** des missions (PRD Phase 3 du
13e, design signé de Clara) — le registre des missions reste l'objet de
Nardole ; le WorkGraph prouve où chaque exécution en est, hop par hop.
Le modèle est à **liste de champs fermée** : aucun champ de dispatch
n'existe ici, l'assignation vit au registre.

- `POST /workgraph/transition` — (état, événement, enveloppe) → état' :
  création de hop (8 `kind` fermés par le PRD), statut, tentative
  d'effet, mise à jour d'`envelope_ref` — elle-même une transition,
  présentée sous le bail successeur : la succession se prouve dans le
  graphe. Toute mutation passe par le point de contrôle unique de la
  Phase 2 (`authorize()`) et par le store, qui refuse un token fencé à
  l'écriture. Idempotence `(mission, hop, transition_key)` : une
  redélivrance est un no-op détecté, jamais un second effet.
  `recorded_at` n'est **jamais** un champ d'entrée : le store le pose.
- **Sémantique UNKNOWN** : un effet tenté non observé est
  `ATTEMPTED_UNKNOWN` ; ses dépendants sont bloqués **à la transition**
  autant qu'à la liste. Il ne se résout que par
  `POST /workgraph/reconcile`, sur l'état réel, avec un `source_read`
  **typé** `{system, locator, observed_value}` — un récit en texte
  libre est refusé à l'écriture, par la forme. Ni le timeout, ni
  l'hypothèse, ni le récit de l'agent, ni la succession de bail ne
  résolvent un UNKNOWN.
- `GET /workgraph/mission`, `/eligible`, `/lease-holder`,
  `/pending-reconciliations` (âge des UNKNOWN exposé, **sans seuil** —
  l'exploitation appartient au watchdog de la Phase 4),
  `/assertion` (charges utiles en JSON opaque : le contrat #64 est
  respecté par construction).
- **Rejeu honnête** : l'état vit au store (schéma v3) ; tuer le daemon
  à n'importe quelle frontière de hop et redémarrer rend un état
  canonique identique, et un effet en cours à l'arrêt lit
  `ATTEMPTED_UNKNOWN` — jamais promu, jamais réinitialisé.

## Configuration (environnement)

| Variable | Défaut | Rôle |
|---|---|---|
| `NOYAU_DATA_DIR` | `./noyau-data` | volume du daemon (store, journal, reçus) |
| `NOYAU_API_BIND` | `127.0.0.1:7474` | liaison de l'API |
| `NOYAU_MCP_BIND` | `127.0.0.1:7475` | liaison du serveur MCP |
| `NOYAU_BOARD_ID` | `amd64-generic` | matériel de référence porté aux reçus |
| `NOYAU_L00_TRACE` | — | chemin de la trace L00 intérimaire (battements) |
| `NOYAU_REGISTRY_PATH` | — | fichier d'enregistrement au registre H-04 |
| `NOYAU_POLICY_DIR` | — | policy store L05 lu au moment de la décision (Phase 2) ; absent ou hors vigueur = claims refusés (fail-closed) |
| `NOYAU_HEARTBEAT_SECS` | `30` | période du battement |

Le boot complet se déroule **réseau coupé** : aucune étape n'attend un
service externe, une clé ou une résolution de nom. Le daemon refuse de
démarrer si son store vise une strate non persistante détectable
(tmpfs/ramfs) — reçu `FAILED(stratum)` consigné au journal de boot.

## Preuves

`scripts/run_holdouts.py` exécute la batterie des hold-outs H-P1-1 → H-P1-12
et dépose les évidences (reçus, mesures, journaux) dans un répertoire
horodaté. `scripts/run_holdouts_phase2.py` exécute les preuves H-P2 de la
Phase 2 (les quatre preuves de l'issue #2 en exécution + complétude du
modèle) et `authority/check_profiles.py` les hold-outs H-PA-1 → H-PA-4.
`scripts/run_holdouts_phase3.py` exécute les preuves H-P3 du WorkGraph
(15 preuves : les dix du PRD, les trois extensions de Clara, la
succession dans le graphe et la file d'observation), dont le cycle
complet de mission en namespace réseau isolé. Les chiffres de boot publiés sont **mesurés** (étiquette « mesuré »,
matériel de référence `amd64-generic`), jamais déclarés : la cible « T1 < 3 s »
est une cible de design, pas un fait.

## Provenance

Code original écrit pour ce programme (clean-room) : aucune reprise de code
d'un daemon tiers, aucun identifiant de marque tierce dans les sources ni
dans les artefacts livrés. Dépendances : crates publiques Rust (voir le
registre des licences du mono-repo, `licenses/`).
