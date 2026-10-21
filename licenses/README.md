# Registre des licences — mono-repo CoolEmbrl 1.0

Règle du programme : **revue juridique avant toute distribution B2B**
(gate du MasterPlan). Ce registre est tenu par composant ; une entrée
absente vaut « non déclaré », jamais « libre ».

| Composant | Licence | Provenance / note |
|---|---|---|
| `packages/bedrock-kernel-rs/coolembrl` (daemon `noyau`) | Apache 2.0 | Code original du programme, écrit en clean-room : aucune reprise de code d'un daemon tiers ; la référence fonctionnelle du programme n'a fourni que des comportements à spécifier indépendamment, jamais de code. |
| Crates Rust du daemon (dépendances, non vendées) | MIT / Apache 2.0 (double licence usuelle de l'écosystème) | `rusqlite` (MIT), `libsqlite3-sys` (MIT), `serde` / `serde_json` (MIT OR Apache 2.0), `libc` (MIT OR Apache 2.0). SQLite amalgamé via `libsqlite3-sys` : domaine public (SQLite). Versions épinglées au `Cargo.lock` du paquet. |
| Coolify (référence de la voie d'hébergement) | Apache 2.0 | Voie actée du programme ; aucun code repris dans la Phase 1. |
| Umbrel (référence fonctionnelle uniquement) | PolyForm Noncommercial | **Jamais repris — ni en code, ni en marque, ni en identifiants.** Toute surface du Noyau est spécifiée indépendamment puis implémentée en clean-room (liste négative du programme). |
| Business OS (bridge, à importer) | Aucune licence déclarée à la source | Titulaire = l'auteur ; statut à déclarer par le titulaire avant toute distribution externe (gate, pas un détail). |

NOTICE : ce fichier est le registre vivant du mono-repo ; chaque import ou
fork ajouté au dépôt y reçoit son entrée avant son merge.
