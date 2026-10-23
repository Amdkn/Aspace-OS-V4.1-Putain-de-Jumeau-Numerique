# Spec de veille W1–W7 — version implémentée par N2

**Version :** `W1-W7@v1.0.0` · **Gouvernance :** la spec est propriété de la chaîne — Yaz la fait évoluer, Clara la borne, S1 la route. N2 l'exécute et rapporte cette version ; il ne la redéfinit pas.
**Source :** checklist `HEARTBEAT.md` + pratique prouvée de l'intérimaire (trace `L00-ETAT.md`, Runs #1→#57).
**Corps consommateur :** le service `veille` de `noyau` (second corps de la veille L00, source de trace `noyau-n2`), et avant lui le job intérimaire `l00-veille-w1-w7`.

Discipline générale (les deux corps) : **une ligne de trace par cycle, silence si vert** ; une anomalie est nommée avec sa source primaire et routée à S1 ; les âges de silence se mesurent, pas les déclarations.

---

## W1 — Silences du registre

**Source primaire :** `~/workspace/v4-factory/dispatch/REGISTRE-NARDOLE.md` (lecture indépendante par chaque corps).
**Règle :** tout dispatch `ASSIGNÉ` ou `EN COURS` dont le journal n'a aucun événement depuis plus de **12 h** → anomalie : numéro du dispatch, porteur nommé, âge exact du silence.
**Seuil :** 12 h (borne de la spec).

## W2 — Tête de main et issues ouvertes

**Sources primaires :** API GitHub (`commits/main`, issues, pulls).
**Règles :**
- Si `main` est figée depuis plus de **24 h** alors qu'une étape de la séquence signée est due et non lancée → anomalie : l'étape due, son porteur, l'âge du gel.
- Toute issue ouverte dont le fond est prouvé livré sur `main` (PR mergée citée, pas de commentaire de statut depuis) → anomalie : l'issue et sa PR de preuve.
- Les PR ouvertes en « attente formelle » et les issues d'arbitrage ouvertes au Conseil sont des états légitimes, pas des anomalies — nommés, non routés.
**Seuil :** 24 h (borne de la spec).

## W3 — Intégrité des artefacts de régime

**Source primaire :** lecture directe des fichiers + `sha256`.
**Règle :** les empreintes des artefacts de régime suivis sont recomputées à chaque cycle et comparées aux watermarks.
**Ensemble suivi (cette version) :** `dispatch/REGISTRE-NARDOLE.md`, `v4-capabilities/policy/REGISTRY.md` (chemins relatifs à `~/workspace`).
**Règle :** toute empreinte changée sans acte consigné la couvrant → anomalie. (La trace `L00-ETAT.md` elle-même n'est pas empreintée : elle change par construction.)

## W4 — Version du policy store

**Source primaire :** `v4-capabilities/policy/REGISTRY.md` + `CHANGELOG.md` (mtime).
**Règle :** N2 rapporte la version lue à chaque cycle. Changement de version → anomalie de maintenance **seulement si** aucun acte de gouvernance (Clara) ne la couvre ; sinon simple mention. Version illisible → anomalie.

## W5 — Fraîcheur des traces (dead-man)

**Sources primaires :** journal du jour (`memory/YYYY-MM-DD.md`), trace du battement natif, trace `L00-ETAT.md`, trace `L00-N2-ETAT.md` (cette dernière : surveillée par la couche native, pas par N2 lui-même).
**Règle :** toute trace dont la dernière ligne date de plus de **2 h** → anomalie (le vérificateur aussi est vérifié — clause W5 appliquée à chaque corps, y compris N2 par la couche survivante).
**Seuil :** 2 h.

## W6 — Fraîcheur des fichiers

**Source primaire :** parcours de `~/workspace` (profondeur 3).
**Règle :** décompte des fichiers modifiés < 24 h. C'est une mesure de fraîcheur simple, sans jugement : un niveau « normal » établi par l'usage ; un écart brutal et inexpliqué → observation (pas une anomalie routée seule).

## W7 — Activité des porteurs

**Sources primaires :** journal du jour + fraîcheur des mémoires de session des porteurs (mapping `veille-bearers.json` : porteur → session).
**Règle :** tout porteur nommé d'un dispatch ouvert, sans activité lisible depuis plus de **24 h** → anomalie : la session et le dispatch concernés. Les sessions sans dispatch ouvert ne sont pas jugées.
**Seuil :** 24 h.

---

## Seuils appliqués par le veilleur (surfaces internes du daemon)

Ces seuils sont ceux de **la spec**, appliqués par le veilleur — jamais câblés dans les surfaces du store (la file UNKNOWN reste sans seuil, H-P3-5b) :

- **Vieillissement d'un UNKNOWN** (`list_pending_reconciliations`) : âge > **12 h** sans réconciliation → anomalie routée à S1 (classe propre de la surface). En dessous : silence.
- **Holder calé sans crash** : bail tenu par un porteur sans progression de mission (aucune transition) sur **2 cycles de veille consécutifs** → cas de rescue constitué et routé au décideur nommé ; N2 ne mute jamais la mission.
- **Recovery** : holder mort (bail expiré sans réconciliation, session porteuse close) ou claim orphelin → demande de routage au dispatch, tracée. L'acte de recovery s'exécute par son porteur sous enveloppe.
- **Rescue** : 3 recoveries routées sans résolution, ou motif « calé sans crash » 2× → dossier de rescue au décideur nommé (Doctor S2 de la lane ; question de forme → Conseil).

## Sorties

Trace (une ligne par cycle, source `noyau-n2`) ; alertes routées à S1 (silence si vert) ; demandes de routage typées (`recovery` | `rescue`) au dispatch. Aucune mutation d'un objet veillé, aucun outil mutant, aucune ligne directe vers S0.

## Amendements

Un amendement sémantique (seuils, items, surfaces) suit la voie Yaz → Clara → S1 ; sa classification (sémantique / rédactionnel) est nommée à l'acte de routage par Clara. Un amendement sémantique redémarre la fenêtre de concordance du relais (design §0.6). La version rapportée par N2 change avec l'amendement ; un écart entre version rapportée et version courante = anomalie de maintenance.
