# Sécurité et modèle de menace

## Ce qu'on protège

| Bien | Menace | Mesure en place (0.1.0-alpha) |
|---|---|---|
| Équité du match | client modifié (speedhack, téléportation, tir impossible) | serveur autoritaire : le client n'envoie que des entrées. Une commande par tick, consommée au tick exact : en envoyer plus ne fait pas aller plus vite. Commandes trop en avance (plus de 64 ticks) ou avec trop d'événements : ignorées. Dispersion, recul et dégâts calculés sur le serveur |
| Information cachée | wallhack, ESP | visibilité prédictive (PIM) : les ennemis invisibles ne sont pas envoyés, fumées comprises. Les volumes de fumée sont calculés par le serveur ; la ligne de vue est symétrique, donc pas de fumée « à sens unique » |
| Compensation de latence | client qui ment sur `view_time` pour tirer dans le passé | rembobinage borné à 200 ms, avec une tolérance de 50 ms autour de la valeur attendue (RTT + interp + tampon) ; au-delà, la valeur est ramenée dans la fenêtre |
| Trafic de jeu | lecture, falsification, rejeu, usurpation de session | X25519 éphémère + HKDF-SHA256 + ChaCha20-Poly1305, séquences 64 bits et fenêtre anti-rejeu |
| Serveur | amplification, épuisement mémoire par fausses connexions | poignée de main avec cookie sans état ; requêtes complétées à 512 octets, réponses plus petites ; nombre de clients borné (`max_clients`) ; paquets limités à 1200 octets |
| API d'administration | prise de contrôle | écoute par défaut sur 127.0.0.1. `/status` et `/command` exigent `Authorization: Bearer` avec `PROJET_S_ADMIN_TOKEN` (16 caractères au minimum, comparaison en temps constant) et sont désactivés sans jeton. `/health` et `/metrics` ne révèlent rien de sensible |
| Secrets | fuite par le dépôt | aucun secret versionné. Jeton lu dans l'environnement (fichier `EnvironmentFile` en mode 0600 pour systemd, `.env` non versionné pour Docker, `Secret` Kubernetes). `.gitignore` exclut `.env`, `*.pem`, `*.key` |
| Hôte | évasion du processus serveur | Docker : utilisateur non root, système de fichiers en lecture seule, `cap_drop: ALL`, `no-new-privileges`. systemd : `DynamicUser`, `ProtectSystem=strict`, `MemoryDenyWriteExecute`, familles d'adresses limitées à IPv4/IPv6. Kubernetes : `runAsNonRoot`, `readOnlyRootFilesystem`, seccomp `RuntimeDefault` |
| Code | `unsafe` | `unsafe_code = "deny"` dans tout l'espace de travail |

## Faiblesses connues (acceptées pour l'alpha)

1. **Le serveur n'est pas authentifié.** Il n'y a pas de clé longue durée ni de
   certificat : un attaquant placé sur le chemin pendant la poignée de main peut
   se faire passer pour le serveur. À faire : clé publique du serveur publiée
   avec son adresse (liste de serveurs signée), vérifiée par le client.
2. **Aucun anti-triche côté client** ni détection statistique : aimbot et
   triggerbot ne sont pas détectés. Le PIM limite seulement le wallhack.
3. **Pas de limite de débit par adresse** avant la poignée de main, au-delà du
   cookie : un flot de HELLO consomme du CPU (X25519 n'est calculé qu'après le
   cookie, ce qui limite le coût).
4. **L'API d'administration est en HTTP sans TLS.** Il faut la garder sur la
   boucle locale ou un réseau privé, ou la placer derrière un proxy TLS.
5. **Les exécutables ne sont pas signés** (SmartScreen sous Windows).
6. **Les replays ne sont pas signés** : un fichier modifié est rejoué tel quel.
   Seule l'intégrité de chaque trame est vérifiée (FNV-1a, contre la corruption,
   pas contre la falsification).

## Signaler un problème

Ouvrir une issue privée (security advisory) sur le dépôt GitHub. Ne pas publier
d'exploit.
