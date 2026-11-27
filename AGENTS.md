# Project-s-server (contexte agents)

Serveur dédié autoritaire de Project S. Lire [`SCOPE.md`](SCOPE.md).

- Headless : jamais de Bevy, d'audio ni de code client (`boundaries.toml`). Le client embarque `ps-server` **sans** la feature `admin`.
- La boucle 128 Hz (`run.rs`) n'attend jamais d'E/S externe ; tout appel futur au Core passe par une file hors du tick.
- Mode DEEP : `ps-visibility`, `server.rs` (validation, lag compensation), déploiement (`deploy/`), workflows de release.
- Gros fichier à lire par plage : `crates/ps-server/src/server.rs` (~1 480 lignes).
- Aucun secret, adresse ou chemin de serveur réel dans le dépôt (public).

Vérification : `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --all-features --release --locked && cargo run --release --locked -p ps-server -- --config deploy/server.example.toml --check` avec Project-s-content voisin.
