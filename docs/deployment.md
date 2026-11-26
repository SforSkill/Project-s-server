# Déploiement du serveur dédié

Le serveur est un binaire Linux x86-64 autonome : `projet-s-server`, avec les
dossiers `configs/` et `maps/`. Il n'a besoin ni de GPU ni de bibliothèque
graphique. Il consomme environ 1 cœur pour un match 5v5 à 128 Hz ; en mesure,
un tick prend en moyenne 40 µs pour 2 clients et 4 bots (`VALIDATION_REPORT.md`).

| Port | Protocole | Exposition |
|---|---|---|
| 27015 | UDP | public : trafic de jeu |
| 27016 | TCP | **local uniquement** : API d'administration HTTP |

Aucune des commandes ci-dessous ne touche aux autres services de la machine :
le serveur a son propre utilisateur, son propre dossier et son propre port.
Vérifier avant tout que 27015/UDP et 27016/TCP sont libres :

```sh
ss -lunp | grep 27015; ss -ltnp | grep 27016   # aucune sortie attendue
```

## Option 1 — systemd (recommandé sur un VPS)

```sh
# 1. Installer les fichiers
tar xzf Project-S-Dedicated-Server-Linux-x64.tar.gz
sudo install -d /opt/projet-s /etc/projet-s
sudo cp -r projet-s-server/projet-s-server projet-s-server/configs projet-s-server/maps /opt/projet-s/
sudo cp projet-s-server/deploy/server.example.toml /etc/projet-s/server.toml
sudo cp projet-s-server/deploy/systemd/projet-s-server.service /etc/systemd/system/

# 2. Jeton d'administration (jamais versionné, lisible par root seul)
sudo sh -c 'umask 077; printf "PROJET_S_ADMIN_TOKEN=%s\n" "$(openssl rand -hex 24)" > /etc/projet-s/admin.env'

# 3. Vérifier la configuration, puis démarrer
sudo /opt/projet-s/projet-s-server --data /opt/projet-s --config /etc/projet-s/server.toml --check
sudo systemctl daemon-reload
sudo systemctl enable --now projet-s-server
systemctl status projet-s-server
journalctl -u projet-s-server -f

# 4. Pare-feu : ouvrir uniquement le port de jeu (exemple ufw)
sudo ufw allow 27015/udp
```

Mise à jour : remplacer le binaire, `configs/` et `maps/` dans `/opt/projet-s`,
puis lancer `sudo systemctl restart projet-s-server`. Le match en cours est
perdu ; son replay est conservé jusqu'à la dernière trame complète.

## Option 2 — Docker

```sh
# Depuis le dépôt
printf "PROJET_S_ADMIN_TOKEN=%s\n" "$(openssl rand -hex 24)" > deploy/docker/.env   # non versionné
chmod 600 deploy/docker/.env
docker compose -f deploy/docker/compose.yaml --env-file deploy/docker/.env up -d --build
docker compose -f deploy/docker/compose.yaml ps            # état « healthy » attendu
docker compose -f deploy/docker/compose.yaml logs -f
```

Le conteneur tourne avec l'utilisateur 10001, un système de fichiers en lecture
seule et `cap_drop: ALL`. Il est limité à 2 CPU et 512 Mo. Les replays sont
écrits dans le volume `replays`. Pour changer de carte ou de mode, surcharger
`command:` dans `compose.yaml` ; le serveur accepte les options listées par
`--help`.

## Option 3 — Kubernetes (squelette)

`deploy/helm/projet-s` déploie un pod par match, avec un Service UDP pour le
port de jeu. L'API d'administration n'est pas exposée hors du pod.

```sh
kubectl create secret generic projet-s-admin --from-literal=PROJET_S_ADMIN_TOKEN="$(openssl rand -hex 24)"
helm install ps1 deploy/helm/projet-s --set adminTokenSecret=projet-s-admin
```

Ce chart **n'a pas été validé sur un cluster réel** : seuls `helm lint` et
`helm template` ont été exécutés, sans erreur. Pour une flotte de serveurs avec allocation, voir `deploy/agones/README.md`.
Le SDK Agones n'est pas intégré dans l'alpha.

## Administration

```sh
TOKEN=$(sudo sed -n 's/^PROJET_S_ADMIN_TOKEN=//p' /etc/projet-s/admin.env)
curl -s localhost:27016/health                     # 200 « ok »
curl -s localhost:27016/metrics                    # format Prometheus/OpenMetrics
curl -s -H "Authorization: Bearer $TOKEN" localhost:27016/status
curl -s -H "Authorization: Bearer $TOKEN" -d "bots 4" localhost:27016/command
```

Commandes acceptées par `/command`, une ligne de 200 caractères au plus :
- bots : `bots N [attack|defense]`, `kickbots`, `difficulty X` ;
- match : `start`, `restart`, `map NOM [profil]`, `mode PROFIL` ;
- joueurs : `say TEXTE`, `kick NOM|slot` ;
- `status`.

Journaux : définir `PROJET_S_LOG_JSON=1` pour des journaux JSON ; le niveau se
règle avec `RUST_LOG`, par exemple `info` ou `game_server=debug`.

## Se connecter

Depuis le client : **Rejoindre → `IP_DU_VPS:27015`**, ou
`Project-S.exe --connect IP_DU_VPS:27015`.

## Ce qui a été vérifié

- La **même image Docker** a tourné en local, sous Docker Desktop (Linux) :
  - `/health` répond 200 ;
  - `/status` sans jeton répond 401 ;
  - deux clients Windows se sont connectés en UDP et ont joué contre 4 bots,
    sans perte de snapshot ni tick en retard.
- `--check` valide `deploy/server.example.toml`.
- Non vérifié :
  - un VPS réel, sur Internet ;
  - l'unité systemd sur une distribution réelle (le fichier est écrit, mais il
    n'a pas été démarré) ;
  - Helm et Agones sur un cluster (seuls `helm lint` et `helm template` sont passés).
