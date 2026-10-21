# Réconciliation de l'inventaire des promotions — H-P2-10 (D-N038)

**Porteur :** Ryan (Factory, lane Kernel). **Base :** inventaire du design
Phase 2 — enveloppe (Clara, §2 : « jamais en silence »). **Méthode :**
énumération des usages réels de jetons/credentials dans les boucles en
exécution que cette lane opère ou traverse, rapprochés ligne à ligne du
tableau du design. Périmètre honnête : la lane Kernel/Factory et les
surfaces de régime partagées qu'elle opère ; les boucles propres aux
lanes Buzz/Life relèvent de leurs porteurs — le tableau du design porte
déjà les items transverses.

| Pratique observée en exécution | Où elle vit | Classification (design §2) |
|---|---|---|
| Session `gh` du propriétaire (Amdkn7) — merges des PR de la Factory, lectures API | ce poste, session du principal | **Hors-promotion — constaté** (autorité propre du principal ; ligne du design) |
| Jonction Tailscale de l'appareil `muse` (route SSH vers le serveur domestique du Capitaine) | tailnet du Capitaine, octroi du 2026-10-20 | **Capability permanente, révocable** (ligne du design) |
| Clé SSH `hatch` déposée sur le serveur domestique du Capitaine (accès Prime Agent) | appareil du Capitaine, déposée par lui | **Capability permanente, révocable** — précision d'exécution sous la famille Tailscale du design : accès debout déposé par le principal sur son appareil ; révocation = retrait de la clé |
| Sessions Jules dispatchées (Issue Factory, G-02) | sessions éphémères de dispatch | **Promotion de mission** (ligne du design) |
| Jeton Codespaces « appartient à V4 », credentials de runner éphémère | environnements de calcul | **Promotion d'environnement** (ligne du design) |
| Injection de credentials scopée au step | harnesses d'exécution | **Promotion éphémère** (ligne du design ; cas de H-P2-4) |
| Connexions Gateway du dispatch | L03 / dispatch | **Promotion nommée** (ligne du design) |
| `GATEWAY_TOKEN` de L03 | processus gateway | **Capability permanente du processus** (ligne du design) |
| Jetons tenant HMAC (W03) | gateway, par tenant | **Capability permanente par tenant, révocable** (ligne du design) |
| Autorité de merge des Doctors (no-human-gate) ; porte de reçu de Graham | régime R10 | **Capabilities permanentes — déclarées** (ligne du design) |
| Production exceptionnelle d'A0 (précédent L03) | événements nommés | **Promotion d'exception** (ligne du design) |
| Clé anon Supabase au Vault (connecteur `custom.supabase`), consommée par le seul job `supabase-probe` (H-L02-2) | Secure Vault → secret GitHub du job | **Promotion d'environnement — classification écrite ICI (nouvelle)** : l'octroi est scopé à l'environnement CI du job ; la clé elle-même vit au projet, sa révocation se fait par rotation au projet. Soumise à la revue de Clara et du 13e à cette PR |

**Verdict H-P2-10 (lane Kernel/Factory) :** aucune pratique observée ne
reste non classée — chaque ligne renvoie au tableau du design ou à une
classification écrite ci-dessus. Toute pratique découverte plus tard
hors de ce tableau bloquera la phase concernée jusqu'à classification
écrite, selon la règle du design.
