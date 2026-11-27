#!/usr/bin/env python3
"""Builds the dedicated server release archive from the compiled binary.

    python tools/package.py linux-server --content ../Project-s-content [--bin-dir target/release]
    python tools/package.py third-party  # writes THIRD_PARTY_LICENSES.md only

The archive goes to dist/ with a .sha256 file. The content (configs/, maps/,
manifests/) sits next to the executable, where the server looks for it first.
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
VERSION = (ROOT / "version.txt").read_text(encoding="utf-8").strip()

KINDS = {
    "linux-server": {
        "archive": "Project-S-Dedicated-Server-Linux-x64.tar.gz",
        "dir": "projet-s-server",
        "bins": ["projet-s-server"],
        "packages": ["ps-server"],
        "extra": ["deploy/server.example.toml", "deploy/systemd/projet-s-server.service"],
    },
}


def third_party(packages, out: Path):
    """Lists every crate linked into the given packages (normal dependencies)."""
    meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT))
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [p["id"] for p in meta["packages"] if p["name"] in packages and p["source"] is None]
    seen, stack = set(), list(roots)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
                stack.append(d["pkg"])
    rows = sorted((by_id[i]["name"], by_id[i]["version"], by_id[i].get("license") or "voir le dépôt", by_id[i].get("repository") or "") for i in seen if by_id[i]["source"] is not None)
    lines = [
        "# Licences tierces — Project S server " + VERSION,
        "",
        "Généré par `python tools/package.py third-party` depuis `cargo metadata` "
        f"pour : {', '.join(packages)}. {len(rows)} crates.",
        "",
        "Le texte complet de chaque licence est dans le dépôt de la crate (colonne Source)",
        "et dans `~/.cargo/registry/src/*/<crate>-<version>/`. Les licences MIT et Apache-2.0",
        "demandent de conserver la notice de copyright : ce fichier en tient lieu avec ces liens.",
        "",
        "Aucun asset (modèle, texture, son, carte) ne provient d'un jeu existant : la géométrie",
        "est générée par `tools/ps-mapgen` (Project-s-content).",
        "",
        "| Crate | Version | Licence | Source |",
        "|---|---|---|---|",
    ]
    lines += [f"| {n} | {v} | {lic} | {repo} |" for n, v, lic, repo in rows]
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return out


def sha256(path: Path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    digest = h.hexdigest()
    Path(str(path) + ".sha256").write_text(f"{digest}  {path.name}\n", encoding="utf-8")
    return digest


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("kind", choices=[*KINDS, "third-party"])
    ap.add_argument("--bin-dir", default="target/release")
    ap.add_argument("--content", default="Project-s-content", help="Project-s-content checkout at the pinned version")
    a = ap.parse_args()
    if a.kind == "third-party":
        print(third_party(["ps-server"], ROOT / "THIRD_PARTY_LICENSES.md"))
        return
    k = KINDS[a.kind]
    bin_dir = (ROOT / a.bin_dir).resolve()
    stage = DIST / "stage" / a.kind / k["dir"]
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    for b in k["bins"]:
        src = bin_dir / b
        if not src.is_file():
            sys.exit(f"binaire manquant : {src} (compiler d'abord en release)")
        shutil.copy2(src, stage / b)
    content = (ROOT / a.content).resolve()
    if not (content / "manifests" / "content.toml").is_file():
        sys.exit(f"contenu introuvable : {content} (cloner Project-s-content à la version de .content-version)")
    shutil.copytree(content / "configs", stage / "configs", ignore=shutil.ignore_patterns("local*"))
    shutil.copytree(content / "maps", stage / "maps")
    shutil.copytree(content / "manifests", stage / "manifests")
    for f in ["README.md", "LICENSE"]:
        shutil.copy2(ROOT / f, stage / f)
    third_party(k["packages"], stage / "THIRD_PARTY_LICENSES.md")
    for f in k["extra"]:
        dst = stage / f  # keep deploy/... so the docs' paths hold
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / f, dst)
    (stage / "VERSION").write_text(VERSION + "\n", encoding="utf-8")
    DIST.mkdir(exist_ok=True)
    out = DIST / k["archive"]
    if out.suffix == ".zip":
        with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for p in sorted(stage.rglob("*")):
                z.write(p, p.relative_to(stage.parent))
    else:
        def norm(ti):
            ti.uid = ti.gid = 0
            ti.uname = ti.gname = "root"
            if ti.isfile() and Path(ti.name).name in k["bins"]:
                ti.mode = 0o755
            return ti
        with tarfile.open(out, "w:gz") as t:
            t.add(stage, arcname=k["dir"], filter=norm)
    digest = sha256(out)
    print(f"{out} ({out.stat().st_size / 1e6:.1f} Mo) sha256 {digest}")


if __name__ == "__main__":
    main()
