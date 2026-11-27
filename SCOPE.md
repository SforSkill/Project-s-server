# Périmètre de Project-s-server

## 1. Mission
Exécuter les parties de manière autoritaire : serveur dédié headless Linux à 128 Hz, et même code en serveur local embarqué par le client.

## 2. Responsabilités
| Crate | Contenu |
|---|---|
| `ps-server` | `Server` sans E/S : connexions, validation des commandes, snapshots, compensation de latence, priorités d'envoi, bots, rounds, bombe, économie, replays, statistiques ; `run` : socket UDP et boucle fixe 128 Hz ; feature `admin` : API HTTP locale (`/health`, `/metrics`, `/status`, `/command`) et métriques Prometheus ; binaire `projet-s-server` |
| `ps-visibility` | Gestion prédictive de la pertinence (PIM) : qui peut voir qui, d'après la géométrie, pour ne pas divulguer les positions cachées (anti-wallhack) |
| `deploy/` | Image Docker, `compose.yaml` mono-serveur, unité systemd durcie, `server.example.toml` |
| `tools/package.py` | Archive de release (binaire + contenu épinglé + licences tierces) |

## 3. Ne contient jamais
Rendu, audio, menu, HUD, code client, accès direct à PostgreSQL, logique de matchmaking ou de classement, clé privée de signature.

## 4. Dépendances autorisées
Project-s-simulation (`ps-sim`, `ps-sim-net` et leurs briques), Project-s-netcode (`ps-transport`), `tokio`/`axum`/`prometheus-client` derrière la feature `admin` (jamais dans la boucle de simulation).

## 5. Dépendances interdites
Bevy et toute pile graphique ou audio, Project-s-engine, le client, clients SQL. `boundaries.toml` en CI ; le client compile `ps-server` sans la feature `admin`.

## 6. Interfaces publiques
`Server`, `ServerConfig`, `run::{start, spawn_local, ServerHandle}`, `find_data_dir`, `stats`.

## 7. Contrats
- Joueurs : messages `ps-sim-net::proto` sur `ps-transport`.
- Contenu : dossier Project-s-content à la version de `.content-version` ; empreinte des données annoncée au client.
- Back-end (prévu, mode officiel) : `ps-core-protocol` (tickets, heartbeats, résultats, sanctions), en dehors de la boucle de simulation.
- Exploitation : API d'administration locale ([`docs/deployment.md`](docs/deployment.md)).

## 8. Performance
Boucle fixe 128 Hz ; la simulation n'attend jamais une E/S HTTP, SQL, disque ou réseau tiers. L'admin et l'écriture des replays sont hors du tick. Mesure de référence : ~40 µs par tick pour 2 clients et 4 bots.

## 9. Sécurité
Serveur autoritaire ; transport chiffré ; PIM contre le wallhack ; API d'administration sur la boucle locale, protégée par `PROJET_S_ADMIN_TOKEN` (variable d'environnement, jamais versionnée) ; unité systemd et conteneur durcis. Modèle de menace : [`docs/security.md`](docs/security.md).

## 10. Compilation
`cargo build --release -p ps-server` (sans Bevy, donc rapide).

## 11. Tests
`cargo test --workspace --all-features --release` ; `cargo run --release -p ps-server -- --config deploy/server.example.toml --check` ; workflow `Docker` (image, health check, compose, systemd). Les tests de bout en bout client ↔ serveur sont dans `ps-netclient` (Project-s).

## 12. Versionnement
SemVer pré-release (`0.2.0-alpha.N`), tag `vX.Y.Z`, `Cargo.lock` versionné. Le serveur et le client doivent partager `PROTOCOL_VERSION` et l'empreinte du contenu.

## 13. Déploiement
Archive de release (systemd) ou image Docker : [`docs/deployment.md`](docs/deployment.md). Kubernetes et Agones : Project-s-infra.

## 14. Limites connues
- Mode officiel non implémenté : pas de vérification de ticket, de sanction ni de remontée de résultats au Core (contrats définis dans `ps-core-protocol`). Le mode actuel correspond au mode communautaire (hébergement libre, administration locale, aucun résultat officiel).
- Pas de reconnexion à un match en cours ni de tests unitaires dans `ps-server` (couvert par les tests de bout en bout de `ps-netclient`).
