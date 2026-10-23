# Resultats des hold-outs H-P3 — WorkGraph type (Phase 3, D-N039)

- **H-P3-11** : PASS — schema ferme : mission ['envelope_ref', 'hops', 'mission_id', 'observed_at', 'recorded_at', 'work_id'] ; hop ['effect', 'evidence_refs', 'fencing_token', 'hop_id', 'kind', 'lease_holder', 'status'] ; vocabulaire de dispatch present : aucun (la moitie registre — citation par pointeur — releve de la premiere ligne integree du 12e)
- **H-P3-4** : PASS — charge d'assertion dict (forme #64) stockee en JSON opaque et relue a l'identique, sans rejet de type
- **H-P3-3** : PASS — recorded_at fourni par l'appelant -> 400 RECORDED_AT_NOT_AN_INPUT ; les recorded_at lus sont poses par le store (horloge reelle, aucun antidatage)
- **H-P3-7** : PASS — meme (hop, transition_key) redelivre -> noop:true, etat canonique inchange, transitions wgA = 2 (aucun double effet)
- **H-P3-8** : PASS — eligible = ['h1'] : le dependant d'un ATTEMPTED_UNKNOWN est absent de list_eligible_hops()
- **H-P3-13** : PASS — transition directe sur le dependant (sans passer par la liste) -> 409 DEPENDENCY_UNKNOWN, refus trace
- **H-P3-6** : PASS — resolution par recit d'agent refusee ; re-tentative d'effet refusee (pas de retry aveugle) ; l'etat reste ATTEMPTED_UNKNOWN — seul un Reconciliation trace tranche
- **H-P3-12** : PASS — source_read en texte libre -> 400 RECONCILIATION_FORM a l'ecriture ; seule une reference typee {system, locator, observed_value} ecrit un Reconciliation
- **H-P3-9** : PASS — effet applique en externe mais non rapporte -> reconciliation le lit CONFIRMED sur l'etat reel ; la mission reprend (h2 avance) SANS re-executer l'effet (tentatives h1 = 1)
- **H-P3-2** : PASS — transition presentee sous l'ancien bail apres succession -> 403 FENCED ; aucune ligne de transition ecrite (le store refuse a l'ecriture)
- **H-P3-2b** : PASS — envelope_ref mis a jour COMME une transition sous le bail successeur ; current_lease_holder lit h2-successeur, token 2
- **H-P3-5b** : PASS — list_pending_reconciliations expose l'UNKNOWN wgP/p1 avec son age (0 s) — donnee exposee, aucun seuil ni notification attaches (design 0.5)
- **H-P3-1** : PASS — kill -9 a chaque frontiere de hop (4 frontieres) -> redemarrage -> etat canonique identique a chaque fois, aucun hop perdu
- **H-P3-10** : PASS — apres reboot : b2 (effet tente a l'arret) lit ATTEMPTED_UNKNOWN — jamais promu CONFIRMED, jamais reinitialise ; b1 reste CONFIRMED : {'b1': 'CONFIRMED', 'b2': 'ATTEMPTED_UNKNOWN'}
- **H-P3-5** : PASS — cycle complet de mission (claim, hops, UNKNOWN, reconciliation typee, reprise) dans un namespace reseau isole, loopback seule : noyau: T1 atteint — boot boot-20261023181508-3fb1369f, d_t1_ms=368 (mesure), ancrage DENY_ALL (mecanisme ABSENT, Phase 2
