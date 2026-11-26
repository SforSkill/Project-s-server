//! Projet S authoritative server.
//!
//! [`server::Server`] is sans-IO (tests drive it with simulated links);
//! [`run`] adds the UDP socket and the fixed 128 Hz loop. The client embeds the
//! same server for local play ([`run::spawn_local`]).

pub mod config;
pub mod run;
pub mod server;
pub mod stats;

pub use config::ServerConfig;
pub use run::{ServerHandle, spawn_local, start};
pub use server::Server;

use std::path::{Path, PathBuf};

/// Finds the game data directory (`configs/` + `maps/`): explicit path, next to
/// the executable, its parent, or the current directory and its parents.
pub fn find_data_dir(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let ok = |p: &Path| p.join("configs").join("weapons.toml").is_file() && p.join("maps").is_dir();
    if let Some(p) = explicit {
        return if ok(p) {
            Ok(p.to_path_buf())
        } else {
            Err(format!("{} ne contient pas configs/ et maps/", p.display()))
        };
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.extend(dir.ancestors().take(4).map(Path::to_path_buf));
        candidates.push(dir.join("data"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.extend(cwd.ancestors().take(4).map(Path::to_path_buf));
    }
    candidates.into_iter().find(|p| ok(p)).ok_or_else(|| {
        "données du jeu introuvables (configs/ et maps/) : utiliser --data <dossier>".into()
    })
}
