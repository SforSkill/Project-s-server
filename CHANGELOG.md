# Changelog

Toutes les versions de `Project-s-server`. Format : [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/), versions [SemVer](https://semver.org/lang/fr/).

## [0.2.0-alpha.1] - 2026-11-27

Premier tag de ce dépôt, extrait de `SforSkill/Projet-S` 0.1.0-alpha.1 (historique conservé). Comportement de jeu et format réseau inchangés.

### Modifié
- `game-server` → `ps-server`, `engine-visibility` → `ps-visibility` ; le binaire reste `projet-s-server` (unités systemd et images existantes inchangées).
- Le contenu vient de Project-s-content (`.content-version`) : `--data`, `PS_CONTENT_DIR` ou dépôt voisin.
- Release, image Docker et `compose.yaml` propres au serveur ; Helm et Agones passent dans Project-s-infra.
