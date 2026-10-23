# Resultats des hold-outs H-P2 — service d'enveloppe (Phase 2, D-N038)

- **H-P2-1** : PASS — bail emis (token 1, policy_ref epingle L05@v1.0.0[P-001@v1,P-002@v1,P-003@v1,P-004@v1]) ; mutation autorisee -> effet #1 ; trace ISSUE avant AUTHORIZE
- **H-P2-2** : PASS — enveloppe autrefois valide presentee apres expires_at -> 403 LEASE_EXPIRED, zero effet porte
- **H-P2-5** : PASS — ancien holder, ancien token, apres bail successeur -> 403 FENCED au point d'effet
- **H-P2-3** : PASS — reconciliation sur l'etat reel (e1 present, non rejoue) ; e2 porte une fois ; effets m3 = 2 (1 + 1, aucun doublon)
- **H-P2-4** : PASS — cas step-credentials du PRD 3.3 : effet sous promotion vivante OK ; apres expiration -> 403 PROMOTION_EXPIRED, aucun nouvel effet (retrait effectif, prouve par execution)
- **H-P2-6** : PASS — plafond 1 consomme -> 403 BUDGET_EXCEEDED ; classe paid_fallback : plafond mecanique 0 malgre un cap declare de 99 (design 0.6)
- **H-P2-7** : PASS — mutation sur tete A ; la PR passe en tete B -> preuve de A marquee OBSOLETE (trace OBSOLETE), jamais portee en silence sur B
- **H-P2-8** : PASS — CLAIMED sans bail (enveloppe fabriquee) -> aucun effet (LEASE_UNKNOWN) ; forme refusee avant tout controle de bail (400) ; la reprise ne passe que par le vrai chemin (successeur + reconciliation, H-P2-3)
- **H-P2-9** : PASS — 9 refus du service, 9 traces REFUSE au store — correspondance 1:1, chaque refus trace avant son retour
- **H-P2-5b** : PASS — second claim initial pendant un bail vivant -> 409 LEASE_HELD (un seul bail vivant par portee de mission)
- **H-P2-11** : PASS — grep des sources : 0 constante de politique [] ; meme binaire (sha256 inchange) + store modifie (P-003 passee declarative, v1.1.0) -> claim refuse POLICY_NOT_IN_FORCE, version lue epinglee dans la reponse ET dans la trace R10
- **H-P2-0** : PASS — mecanisme rapporte ABSENT pendant toutes les preuves ; acte d'activation trace (ANCHOR_ACTIVATED) -> ACTIVE ; les baux survivent au redemarrage (etat au store, jamais en memoire seule)
