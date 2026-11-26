//! Server configuration (`server.toml` + command-line overrides).

use engine_hitreg::LagCompPolicy;
use engine_visibility::VisibilityConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub name: String,
    /// UDP game address.
    pub bind: String,
    /// HTTP admin/metrics address (keep it private: localhost or an internal network).
    pub admin_bind: Option<String>,
    pub map: String,
    /// Rules profile from `configs/rules.toml`.
    pub profile: String,
    pub tick_rate: u32,
    /// Snapshot every N ticks (1 = 128 Hz at 128 tick).
    pub snapshot_divider: u32,
    pub max_clients: usize,
    /// Bots added at start (filled up to the team size in competitive).
    pub bots: u32,
    pub bot_difficulty: f32,
    pub fill_with_bots: bool,
    /// Data directory (configs/ and maps/); empty = auto-detect.
    pub data_dir: Option<PathBuf>,
    /// Write deterministic replays here (one file per match).
    pub replay_dir: Option<PathBuf>,
    pub lag_compensation: LagCompPolicy,
    pub visibility: VisibilityConfig,
    pub motd: String,
    /// Local listen server: the loopback client may use host commands.
    pub listen_host: bool,
    pub seed: Option<u64>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "Projet S".into(),
            bind: "0.0.0.0:27015".into(),
            admin_bind: Some("127.0.0.1:27016".into()),
            map: "ps_citadel".into(),
            profile: "competitive".into(),
            tick_rate: 128,
            snapshot_divider: 1,
            max_clients: 16,
            bots: 0,
            bot_difficulty: 0.5,
            fill_with_bots: false,
            data_dir: None,
            replay_dir: None,
            lag_compensation: LagCompPolicy::default(),
            visibility: VisibilityConfig::default(),
            motd: "Bienvenue sur Projet S (alpha)".into(),
            listen_host: false,
            seed: None,
        }
    }
}

impl ServerConfig {
    pub fn from_toml(text: &str) -> Result<ServerConfig, String> {
        toml::from_str(text).map_err(|e| format!("server.toml: {e}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        engine_core::TickRate::new(self.tick_rate)?;
        if self.snapshot_divider == 0 || self.snapshot_divider > 8 {
            return Err("snapshot_divider doit être entre 1 et 8".into());
        }
        if self.max_clients == 0 || self.max_clients > 64 {
            return Err("max_clients doit être entre 1 et 64".into());
        }
        if !(0.0..=1.0).contains(&self.bot_difficulty) {
            return Err("bot_difficulty doit être entre 0 et 1".into());
        }
        Ok(())
    }

    /// Applies `--key value` style overrides.
    pub fn apply_arg(&mut self, key: &str, value: &str) -> Result<(), String> {
        let num = |v: &str| {
            v.parse::<u64>()
                .map_err(|_| format!("--{key}: nombre attendu"))
        };
        match key {
            "name" => self.name = value.into(),
            "bind" | "port" => {
                self.bind = if key == "port" {
                    format!("0.0.0.0:{}", num(value)?)
                } else {
                    value.into()
                };
            }
            "admin" => {
                self.admin_bind = if value == "off" {
                    None
                } else {
                    Some(value.into())
                }
            }
            "map" => self.map = value.into(),
            "profile" | "mode" => self.profile = value.into(),
            "tick" | "tickrate" => self.tick_rate = num(value)? as u32,
            "snapshot-divider" => self.snapshot_divider = num(value)? as u32,
            "max-clients" => self.max_clients = num(value)? as usize,
            "bots" => self.bots = num(value)? as u32,
            "bot-difficulty" => {
                self.bot_difficulty = value
                    .parse()
                    .map_err(|_| "--bot-difficulty: nombre attendu")?
            }
            "fill-bots" => self.fill_with_bots = value == "true" || value == "1",
            "data" => self.data_dir = Some(value.into()),
            "replays" => {
                self.replay_dir = if value == "off" {
                    None
                } else {
                    Some(value.into())
                }
            }
            "seed" => self.seed = Some(num(value)?),
            "lagcomp" => self.lag_compensation.enabled = value != "off",
            "pim" => self.visibility.enabled = value != "off",
            _ => return Err(format!("option inconnue --{key}")),
        }
        Ok(())
    }
}
