# Project-s-server

Serveur dédié autoritaire de [Project S](https://github.com/SforSkill/Project-s-docs) : Linux, headless, 128 Hz, UDP chiffré.

```sh
git clone https://github.com/SforSkill/Project-s-content ../Project-s-content
cargo run --release -p ps-server -- --config deploy/server.example.toml
```

Installation (systemd, Docker) : [`docs/deployment.md`](docs/deployment.md). Sécurité : [`docs/security.md`](docs/security.md). Périmètre : [`SCOPE.md`](SCOPE.md). Licence : propriétaire ([`LICENSE`](LICENSE)).
