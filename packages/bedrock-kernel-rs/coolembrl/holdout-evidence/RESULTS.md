# Resultats des hold-outs H-P1 — daemon noyau (Phase 1, D-N037)

Chaque ligne est un resultat MESURE contre le binaire reel, evidences dans ce repertoire.

- **H-P1-7** : PASS — store sur /dev/shm (tmpfs) -> exit 2, recu FAILED(stratum)
- **H-P1-2** : PASS — 10 boots : d_t1_ms median=196.0 max=205 (cible < 3000, MESURE sur cette machine amd64)
- **H-P1-9** : PASS — t2_ready_at null sur 11 recus ; watchdog NOT_INSTALLED verifie a /state
- **H-P1-1** : PASS — serie de recus au journal : 11 T1 + 1 FAILED provoque (stratum)
- **H-P1-3** : PASS — mutation-sonde pendant T0 : refusee ; en T1 : POST /anchor -> 403/403 types ; outil mutant MCP : isError=True
- **H-P1-8** : PASS — requete en forme de bail + demande d'octroi -> refus types ANCHOR_DENY_ALL ; mechanism ABSENT, capabilities_granted 0 ; aucune reponse en forme de bail
- **H-P1-9b** : PASS — etat rapporte : T1, watchdog veille stade A (ombre, non autoritaire), t2 null
- **H-P1-4** : PASS — kill -9 repete pendant le boot ; boot suivant : anomalie de recuperation au recu, rejeu 5 hops / 1 inconnu COMPTE (non resolu), etat canonique identique (5 lignes)
- **H-P1-12** : PASS — port MCP occupe -> exit 2, recu FAILED(probe-mcp), aucun T1 declare
- **H-P1-10** : PASS — 3 battements au format interimaire dans la trace ; daemon arrete -> plus aucun battement (absence lisible)
- **H-P1-6** : PASS — boot complet T0->T1 dans un namespace reseau isole (loopback seule)
- **H-P1-5** : PASS — grep de l'arbre source du paquet : 0 identifiant interdit []
- **H-P1-11** : PASS — artefacts livres : binaire (strings) 0 occurrence ; unit, manifeste et routes verifies dans le meme grep d'arbre
