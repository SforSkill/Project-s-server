//! Sans-IO authoritative server: feed it datagrams and a clock, it returns
//! datagrams. The socket loop lives in [`crate::run`]; tests drive it directly.

use crate::config::ServerConfig;
use crate::stats::ServerStats;
use engine_core::cmd::UserCmd;
use engine_core::{MAX_PLAYERS, PlayerId, Team};
use engine_network::{Channel, MAX_MESSAGE, ServerEndpoint, ServerEvent};
use engine_visibility::{Target, Viewer, potentially_visible};
use game_rules::Mode;
use game_sim::proto::{
    self, BombNet, ClientMsg, DropQ, EntityQ, GrenadeQ, PlayerInfo, SelfState, ServerMsg, ShotNet,
    SmokeNet, Snapshot, Welcome, eflags,
};
use game_sim::record::{Recorder, ReplayHeader, SimInput, apply};
use game_sim::{BombState, Controller, GameData, GameEvent, Level, ShotReport, Sim, SimConfig};
use glam::Vec3;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::BufWriter;
use std::net::SocketAddr;
use std::sync::Arc;

/// Commands further ahead than this are refused (ticks).
const MAX_CMD_AHEAD: u32 = 64;
/// Snapshot baselines kept per client.
const BASELINES: usize = 64;
/// Seconds a disconnected player keeps their slot in round-based modes.
const RECONNECT_GRACE: f64 = 120.0;
const FOOTSTEP_RANGE: f32 = 1100.0;
const SHOT_RANGE: f32 = 3500.0;

pub type Datagrams = Vec<(SocketAddr, Vec<u8>)>;

struct Client {
    conn: u32,
    player: Option<PlayerId>,
    name: String,
    host: bool,
    joined: bool,
    cmds: BTreeMap<u32, UserCmd>,
    newest_cmd: u32,
    snapshot_ack: u32,
    interp_ticks: f32,
    baselines: VecDeque<(u32, HashMap<u8, EntityQ>)>,
    /// Last time each other player passed the visibility test.
    seen: HashMap<u8, f64>,
    token: u64,
    late_cmds: u64,
}

/// Slot kept for a disconnected player.
struct Reserved {
    player: PlayerId,
    token: u64,
    since: f64,
}

pub struct Server {
    pub cfg: ServerConfig,
    pub data: Arc<GameData>,
    pub sim: Sim,
    endpoint: ServerEndpoint,
    clients: HashMap<u32, Client>,
    reserved: Vec<Reserved>,
    recorder: Option<Recorder<BufWriter<File>>>,
    pub stats: ServerStats,
    last_players_sent: f64,
    ended_at: Option<f64>,
    pub log: Vec<String>,
    /// Datagrams produced outside `receive`/`tick` (admin kicks).
    pending: Datagrams,
}

fn build_sim(data: &Arc<GameData>, cfg: &ServerConfig, seed: u64) -> Result<Sim, String> {
    let def = data
        .maps
        .get(&cfg.map)
        .ok_or_else(|| {
            format!(
                "carte inconnue « {} » (disponibles : {})",
                cfg.map,
                data.maps.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?
        .clone();
    let level =
        Arc::new(Level::build(def, &data.materials, &data.movement).map_err(|e| e.to_string())?);
    let rules = data.rules_profile(&cfg.profile)?;
    let sim_cfg = SimConfig {
        tick_rate: cfg.tick_rate,
        lag_comp: cfg.lag_compensation,
        bot_difficulty: cfg.bot_difficulty,
        ..SimConfig::default()
    };
    Sim::new(data.clone(), level, rules, sim_cfg, seed)
}

fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("OS random generator");
    u64::from_le_bytes(b)
}

impl Server {
    pub fn new(cfg: ServerConfig, data: Arc<GameData>) -> Result<Server, String> {
        cfg.validate()?;
        let seed = cfg.seed.unwrap_or_else(random_u64);
        let sim = build_sim(&data, &cfg, seed)?;
        let endpoint = ServerEndpoint::new(cfg.max_clients);
        let mut s = Server {
            cfg,
            data,
            sim,
            endpoint,
            clients: HashMap::new(),
            reserved: Vec::new(),
            recorder: None,
            stats: ServerStats::default(),
            last_players_sent: 0.0,
            ended_at: None,
            log: Vec::new(),
            pending: Vec::new(),
        };
        s.start_recording();
        for _ in 0..s.cfg.bots {
            s.add_bot(None);
        }
        s.fill_bots();
        Ok(s)
    }

    fn note(&mut self, msg: String) {
        tracing::info!("{msg}");
        self.log.push(msg);
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }

    fn start_recording(&mut self) {
        let Some(dir) = self.cfg.replay_dir.clone() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("replays désactivés : {e}");
            return;
        }
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let header = ReplayHeader {
            game_version: env!("CARGO_PKG_VERSION").into(),
            map: self.cfg.map.clone(),
            profile: self.cfg.profile.clone(),
            rules: self.sim.rules.cfg.clone(),
            sim: self.sim.cfg.clone(),
            seed: self.sim.seed,
            data_fingerprint: self.data.fingerprint(),
            started_unix: unix,
        };
        let path = dir.join(format!(
            "{}_{}_{}.psrp",
            unix, self.cfg.map, self.cfg.profile
        ));
        match File::create(&path)
            .map_err(|e| e.to_string())
            .and_then(|f| Recorder::new(BufWriter::new(f), &header).map_err(|e| e.to_string()))
        {
            Ok(r) => {
                self.recorder = Some(r);
                self.note(format!("enregistrement du replay : {}", path.display()));
            }
            Err(e) => tracing::warn!("replay non enregistré : {e}"),
        }
    }

    /// Applies an input to the sim and records it.
    fn input(&mut self, i: SimInput) -> Result<Option<PlayerId>, String> {
        let r = apply(&mut self.sim, &i);
        if r.is_ok()
            && let Some(rec) = &mut self.recorder
        {
            let i = match (i, &r) {
                (
                    SimInput::Join {
                        name, bot, team, ..
                    },
                    Ok(Some(id)),
                ) => SimInput::Join {
                    slot: id.0,
                    name,
                    bot,
                    team,
                },
                (i, _) => i,
            };
            rec.input(self.sim.tick, i);
        }
        r
    }

    pub fn add_bot(&mut self, team: Option<Team>) -> Option<PlayerId> {
        let n = self
            .sim
            .iter_players()
            .filter(|p| p.controller == Controller::Bot)
            .count();
        let names = [
            "Ash", "Birch", "Cedar", "Dune", "Ember", "Flint", "Gale", "Haze", "Iris", "Jade",
            "Kite", "Lynx", "Moss", "Nova", "Onyx", "Pike",
        ];
        let name = format!("BOT {}", names[n % names.len()]);
        self.input(SimInput::Join {
            slot: u8::MAX,
            name,
            bot: true,
            team,
        })
        .ok()
        .flatten()
    }

    pub fn remove_bots(&mut self) {
        let bots: Vec<_> = self
            .sim
            .iter_players()
            .filter(|p| p.controller == Controller::Bot)
            .map(|p| p.id.0)
            .collect();
        for slot in bots {
            let _ = self.input(SimInput::Leave { slot });
        }
    }

    /// Fills teams with bots up to the profile's team size.
    fn fill_bots(&mut self) {
        if !self.cfg.fill_with_bots {
            return;
        }
        let size = self.sim.rules.cfg.team_size as usize;
        for t in [Team::Attack, Team::Defense] {
            while self.sim.team_count(t) < size {
                if self.add_bot(Some(t)).is_none() {
                    return;
                }
            }
            // Too many players: drop bots first.
            while self.sim.team_count(t) > size {
                let Some(b) = self
                    .sim
                    .iter_players()
                    .find(|p| p.team == t && p.controller == Controller::Bot)
                    .map(|p| p.id.0)
                else {
                    break;
                };
                let _ = self.input(SimInput::Leave { slot: b });
            }
        }
    }

    pub fn clients(&self) -> usize {
        self.clients.values().filter(|c| c.joined).count()
    }

    /// Feeds one received datagram.
    pub fn receive(&mut self, from: SocketAddr, pkt: &[u8], now: f64, out: &mut Datagrams) {
        self.stats.packets_in += 1;
        self.stats.bytes_in += pkt.len() as u64;
        let mut events = Vec::new();
        self.endpoint.receive(from, pkt, now, out, &mut events);
        self.handle_events(events, now, out);
    }

    fn handle_events(&mut self, events: Vec<ServerEvent>, now: f64, out: &mut Datagrams) {
        for e in events {
            match e {
                ServerEvent::Connected { addr, conn_id } => {
                    let host = self.cfg.listen_host
                        && addr.ip().is_loopback()
                        && !self.clients.values().any(|c| c.host);
                    self.clients.insert(
                        conn_id,
                        Client {
                            conn: conn_id,
                            player: None,
                            name: String::new(),
                            host,
                            joined: false,
                            cmds: BTreeMap::new(),
                            newest_cmd: 0,
                            snapshot_ack: 0,
                            interp_ticks: 0.0,
                            baselines: VecDeque::new(),
                            seen: HashMap::new(),
                            token: 0,
                            late_cmds: 0,
                        },
                    );
                }
                ServerEvent::Message {
                    conn_id,
                    channel,
                    data,
                } => {
                    if channel == Channel::Input || channel == Channel::Reliable {
                        match proto::decode::<ClientMsg>(&data) {
                            Ok(m) => self.handle_msg(conn_id, m, now, out),
                            Err(_) => self.stats.bad_messages += 1,
                        }
                    }
                }
                ServerEvent::Disconnected { conn_id, reason } => {
                    self.drop_client(conn_id, &reason, now)
                }
            }
        }
    }

    fn send(&mut self, conn: u32, ch: Channel, msg: &ServerMsg) {
        if let Some(c) = self.endpoint.connection_mut(conn) {
            let _ = c.send(ch, proto::encode(msg));
        }
    }

    fn broadcast(&mut self, ch: Channel, msg: &ServerMsg) {
        let ids: Vec<u32> = self
            .clients
            .values()
            .filter(|c| c.joined)
            .map(|c| c.conn)
            .collect();
        let bytes = proto::encode(msg);
        for id in ids {
            if let Some(c) = self.endpoint.connection_mut(id) {
                let _ = c.send(ch, bytes.clone());
            }
        }
    }

    fn drop_client(&mut self, conn: u32, reason: &str, now: f64) {
        let Some(c) = self.clients.remove(&conn) else {
            return;
        };
        let Some(id) = c.player else { return };
        self.note(format!("{} s'est déconnecté ({reason})", c.name));
        let round_based = matches!(self.sim.mode(), Mode::Competitive | Mode::Duel);
        if round_based && reason != "leave" {
            if let Some(p) = self.sim.player_mut(id) {
                p.connected = false;
                p.disconnected_at = Some(now);
            }
            self.reserved.push(Reserved {
                player: id,
                token: c.token,
                since: now,
            });
        } else {
            let _ = self.input(SimInput::Leave { slot: id.0 });
            self.fill_bots();
        }
        self.broadcast(
            Channel::Reliable,
            &ServerMsg::Notice(format!("{} a quitté la partie", c.name)),
        );
    }

    fn handle_msg(&mut self, conn: u32, m: ClientMsg, now: f64, out: &mut Datagrams) {
        let joined = self.clients.get(&conn).is_some_and(|c| c.joined);
        match m {
            ClientMsg::Join {
                name,
                protocol,
                data_fingerprint,
                version,
                reconnect,
            } => {
                if joined {
                    return;
                }
                if protocol != proto::GAME_PROTOCOL || data_fingerprint != self.data.fingerprint() {
                    let msg = format!(
                        "version incompatible (serveur {} protocole {}, client {version} protocole {protocol}) ou données de jeu différentes",
                        env!("CARGO_PKG_VERSION"),
                        proto::GAME_PROTOCOL
                    );
                    self.send(conn, Channel::Reliable, &ServerMsg::Kicked(msg));
                    self.endpoint.kick(conn, "incompatible", now, out);
                    self.clients.remove(&conn);
                    return;
                }
                let name: String = name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(24)
                    .collect::<String>()
                    .trim()
                    .to_string();
                let name = if name.is_empty() {
                    "Joueur".to_string()
                } else {
                    name
                };
                // Reconnection to a kept slot.
                let mut player = None;
                if let Some(tok) = reconnect
                    && let Some(i) = self.reserved.iter().position(|r| r.token == tok)
                {
                    let r = self.reserved.remove(i);
                    if let Some(p) = self.sim.player_mut(r.player) {
                        p.connected = true;
                        p.disconnected_at = None;
                        player = Some(r.player);
                    }
                }
                let token = random_u64();
                if player.is_none() {
                    // Make room by removing a bot when teams are full.
                    let auto = self.sim.auto_team();
                    let bot = self
                        .sim
                        .iter_players()
                        .find(|p| p.controller == Controller::Bot && p.team == auto)
                        .map(|p| p.id.0);
                    if self.cfg.fill_with_bots
                        && let Some(b) = bot
                    {
                        let _ = self.input(SimInput::Leave { slot: b });
                    }
                    player = self
                        .input(SimInput::Join {
                            slot: u8::MAX,
                            name: name.clone(),
                            bot: false,
                            team: None,
                        })
                        .ok()
                        .flatten();
                }
                let Some(id) = player else {
                    self.send(
                        conn,
                        Channel::Reliable,
                        &ServerMsg::Kicked("serveur complet".into()),
                    );
                    self.endpoint.kick(conn, "full", now, out);
                    self.clients.remove(&conn);
                    return;
                };
                let host = {
                    let c = self.clients.get_mut(&conn).expect("client");
                    c.player = Some(id);
                    c.name = name.clone();
                    c.joined = true;
                    c.token = token;
                    c.host
                };
                let w = Welcome {
                    you: id.0,
                    server_name: self.cfg.name.clone(),
                    tick_rate: self.sim.rate.hz(),
                    tick: self.sim.tick,
                    map: self.cfg.map.clone(),
                    profile: self.cfg.profile.clone(),
                    rules: self.sim.rules.cfg.clone(),
                    sim: self.sim.cfg.clone(),
                    data_fingerprint: self.data.fingerprint(),
                    host,
                    reconnect_token: token,
                };
                self.send(conn, Channel::Reliable, &ServerMsg::Welcome(Box::new(w)));
                let motd = self.cfg.motd.clone();
                self.send(conn, Channel::Reliable, &ServerMsg::Notice(motd));
                self.send_world_state(conn);
                self.note(format!("{name} a rejoint (slot {})", id.0));
                self.broadcast(
                    Channel::Reliable,
                    &ServerMsg::Notice(format!("{name} a rejoint la partie")),
                );
                self.fill_bots();
                self.last_players_sent = 0.0;
            }
            _ if !joined => {}
            ClientMsg::Cmds {
                cmds,
                snapshot_ack,
                interp_ticks,
            } => {
                let tick = self.sim.tick;
                let c = self.clients.get_mut(&conn).expect("client");
                if snapshot_ack > c.snapshot_ack && snapshot_ack <= tick {
                    c.snapshot_ack = snapshot_ack;
                }
                c.interp_ticks = interp_ticks.clamp(0.0, 64.0);
                // Clock feedback uses the newest command of the latest packet,
                // late or too early ones included: the client must learn either.
                if let Some(newest) = cmds.iter().map(|c| c.tick).max() {
                    c.newest_cmd = newest.clamp(tick.saturating_sub(1000), tick + 1000).max(1);
                }
                for cmd in cmds.into_iter().take(proto::CMD_REDUNDANCY * 2) {
                    if cmd.tick < tick {
                        if cmd.tick + 8 > tick && !c.cmds.contains_key(&cmd.tick) {
                            c.late_cmds += 1;
                        }
                        continue;
                    }
                    if cmd.tick > tick + MAX_CMD_AHEAD
                        || cmd.events.len() > engine_core::cmd::MAX_EVENTS_PER_CMD
                    {
                        continue;
                    }
                    c.cmds.entry(cmd.tick).or_insert(cmd);
                }
            }
            ClientMsg::Team(t) => {
                let Some(id) = self.clients[&conn].player else {
                    return;
                };
                if let Err(e) = self.input(SimInput::Team {
                    slot: id.0,
                    team: t,
                }) {
                    self.send(conn, Channel::Reliable, &ServerMsg::Notice(e));
                }
                self.fill_bots();
                self.last_players_sent = 0.0;
            }
            ClientMsg::Buy(item) => {
                let Some(id) = self.clients[&conn].player else {
                    return;
                };
                if item.len() > 32 {
                    return;
                }
                if let Err(e) = self.input(SimInput::Buy { slot: id.0, item }) {
                    self.send(conn, Channel::Reliable, &ServerMsg::Notice(e));
                }
            }
            ClientMsg::Chat { team_only, text } => {
                let Some(id) = self.clients[&conn].player else {
                    return;
                };
                let text: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
                if text.trim().is_empty() {
                    return;
                }
                let ev = GameEvent::Chat {
                    from: Some(id),
                    team_only,
                    text,
                };
                let team = self.sim.player(id).map(|p| p.team);
                let ids: Vec<u32> = self
                    .clients
                    .values()
                    .filter(|c| {
                        c.joined
                            && (!team_only
                                || c.player.and_then(|p| self.sim.player(p)).map(|p| p.team)
                                    == team)
                    })
                    .map(|c| c.conn)
                    .collect();
                let msg = ServerMsg::Events(vec![ev]);
                for i in ids {
                    self.send(i, Channel::Reliable, &msg);
                }
            }
            ClientMsg::Host(cmd) => {
                if !self.clients[&conn].host {
                    self.send(
                        conn,
                        Channel::Reliable,
                        &ServerMsg::Notice("commande réservée à l'hôte".into()),
                    );
                    return;
                }
                let r = self.command(&cmd);
                self.send(conn, Channel::Reliable, &ServerMsg::Notice(r));
            }
            ClientMsg::Leave => {
                self.endpoint.kick(conn, "leave", now, out);
                self.drop_client(conn, "leave", now);
            }
        }
    }

    /// Host / admin commands. Returns a human-readable result.
    pub fn command(&mut self, line: &str) -> String {
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("");
        let arg = it.next();
        match cmd {
            "bots" | "addbot" => {
                let n: u32 = arg.and_then(|a| a.parse().ok()).unwrap_or(1).min(32);
                let team = match it.next() {
                    Some("attack" | "a" | "t") => Some(Team::Attack),
                    Some("defense" | "d" | "ct") => Some(Team::Defense),
                    _ => None,
                };
                let added = (0..n).filter(|_| self.add_bot(team).is_some()).count();
                self.last_players_sent = 0.0;
                format!("{added} bot(s) ajouté(s)")
            }
            "kickbots" => {
                self.remove_bots();
                self.last_players_sent = 0.0;
                "bots retirés".into()
            }
            "difficulty" => match arg.and_then(|a| a.parse::<f32>().ok()) {
                Some(d) if (0.0..=1.0).contains(&d) => {
                    self.cfg.bot_difficulty = d;
                    self.sim.cfg.bot_difficulty = d;
                    format!("difficulté des bots : {d}")
                }
                _ => "usage : difficulty <0..1>".into(),
            },
            "start" => {
                let _ = self.input(SimInput::ForceStart);
                "partie lancée".into()
            }
            "restart" => self.change_map(None, None),
            "map" => match arg {
                Some(m) => self.change_map(Some(m.to_string()), it.next().map(str::to_string)),
                None => format!("cartes : {}", self.data.maps.keys().cloned().collect::<Vec<_>>().join(", ")),
            },
            "mode" | "profile" => match arg {
                Some(p) => self.change_map(None, Some(p.to_string())),
                None => format!("profils : {}", self.data.rules.profiles.keys().cloned().collect::<Vec<_>>().join(", ")),
            },
            "say" => {
                let text = line.trim_start_matches("say").trim().to_string();
                self.broadcast(Channel::Reliable, &ServerMsg::Notice(format!("[serveur] {text}")));
                "ok".into()
            }
            "kick" => {
                let Some(name) = arg else { return "usage : kick <nom|slot>".into() };
                let target = self.clients.values().find(|c| c.name == name || c.player.is_some_and(|p| p.0.to_string() == name)).map(|c| c.conn);
                match target {
                    Some(conn) => {
                        self.send(conn, Channel::Reliable, &ServerMsg::Kicked("exclu par l'administrateur".into()));
                        let mut out = Vec::new();
                        let now = self.stats.uptime;
                        self.endpoint.flush_one(conn, now, &mut out);
                        self.pending.extend(out);
                        self.endpoint.kick(conn, "kick", now, &mut self.pending);
                        self.drop_client(conn, "leave", now);
                        format!("{name} exclu")
                    }
                    None => format!("joueur « {name} » introuvable"),
                }
            }
            "status" => format!("{} | {} | tick {} | {} clients | {} joueurs", self.cfg.map, self.cfg.profile, self.sim.tick, self.clients(), self.sim.iter_players().filter(|p| !p.is_dummy()).count()),
            _ => "commandes : bots N [attack|defense], kickbots, difficulty X, start, restart, map NOM [profil], mode PROFIL, say TEXTE, kick NOM, status".into(),
        }
    }

    /// Rebuilds the simulation (map or profile change, restart). Humans are re-added.
    fn change_map(&mut self, map: Option<String>, profile: Option<String>) -> String {
        let mut cfg = self.cfg.clone();
        if let Some(m) = map {
            cfg.map = m;
        }
        if let Some(p) = profile {
            cfg.profile = p;
        }
        let sim = match build_sim(&self.data, &cfg, random_u64()) {
            Ok(s) => s,
            Err(e) => return e,
        };
        if let Some(r) = &mut self.recorder {
            let _ = r.finish(self.sim.tick);
        }
        self.recorder = None;
        let bots = self
            .sim
            .iter_players()
            .filter(|p| p.controller == Controller::Bot)
            .map(|p| p.team)
            .collect::<Vec<_>>();
        self.cfg = cfg;
        self.sim = sim;
        self.reserved.clear();
        self.ended_at = None;
        self.start_recording();
        let mut conns: Vec<u32> = self
            .clients
            .values()
            .filter(|c| c.joined)
            .map(|c| c.conn)
            .collect();
        conns.sort();
        for conn in conns {
            let name = self.clients[&conn].name.clone();
            let id = self
                .input(SimInput::Join {
                    slot: u8::MAX,
                    name,
                    bot: false,
                    team: None,
                })
                .ok()
                .flatten();
            let token = self.clients[&conn].token;
            let host = {
                let c = self.clients.get_mut(&conn).expect("client");
                c.player = id;
                c.cmds.clear();
                c.baselines.clear();
                c.snapshot_ack = 0;
                c.seen.clear();
                c.host
            };
            if let Some(id) = id {
                let w = Welcome {
                    you: id.0,
                    server_name: self.cfg.name.clone(),
                    tick_rate: self.sim.rate.hz(),
                    tick: self.sim.tick,
                    map: self.cfg.map.clone(),
                    profile: self.cfg.profile.clone(),
                    rules: self.sim.rules.cfg.clone(),
                    sim: self.sim.cfg.clone(),
                    data_fingerprint: self.data.fingerprint(),
                    host,
                    reconnect_token: token,
                };
                self.send(conn, Channel::Reliable, &ServerMsg::Welcome(Box::new(w)));
            }
        }
        for t in bots {
            self.add_bot(if self.sim.mode() == Mode::Deathmatch {
                None
            } else {
                Some(t)
            });
        }
        self.fill_bots();
        self.last_players_sent = 0.0;
        let msg = format!("carte {} — mode {}", self.cfg.map, self.cfg.profile);
        self.note(msg.clone());
        msg
    }

    fn send_world_state(&mut self, conn: u32) {
        let now = self.sim.now();
        let smokes: Vec<_> = self
            .sim
            .smokes
            .iter()
            .filter(|s| s.end > now)
            .map(smoke_net)
            .collect();
        for s in smokes {
            self.send(conn, Channel::Reliable, &ServerMsg::Smoke(s));
        }
        let fires: Vec<_> = self
            .sim
            .fires
            .iter()
            .filter(|f| f.end > now)
            .map(|f| ServerMsg::Fire {
                id: f.id,
                cells: f.cells.clone(),
                start: f.start,
                end: f.end,
            })
            .collect();
        for f in fires {
            self.send(conn, Channel::Reliable, &f);
        }
    }

    /// Runs one simulation tick and queues the outgoing traffic.
    pub fn tick(&mut self, now: f64, out: &mut Datagrams) {
        self.stats.uptime = now;
        out.append(&mut self.pending);
        let tick = self.sim.tick;
        // Commands for this tick.
        let mut cmds: Vec<Option<UserCmd>> = (0..MAX_PLAYERS).map(|_| None).collect();
        let mut buffers = Vec::new();
        for c in self.clients.values_mut() {
            let Some(id) = c.player else { continue };
            while let Some((&t, _)) = c.cmds.first_key_value() {
                if t < tick {
                    c.cmds.pop_first();
                } else {
                    break;
                }
            }
            match c.cmds.remove(&tick) {
                Some(cmd) => cmds[id.0 as usize] = Some(cmd),
                None => self.stats.missing_cmds += 1,
            }
            // No command yet: unknown (sent as i16::MIN), not a huge negative buffer.
            buffers.push((
                c.conn,
                if c.newest_cmd == 0 {
                    i64::MIN
                } else {
                    c.newest_cmd as i64 - tick as i64
                },
            ));
        }
        // Disconnected players stand still.
        for r in &self.reserved {
            if let Some(p) = self.sim.player(r.player) {
                cmds[r.player.0 as usize] = Some(UserCmd {
                    tick,
                    view_start: p.view,
                    view_end: p.view,
                    view_time: engine_core::SimTime::new(tick, 0.0),
                    ..Default::default()
                });
            }
        }
        // Expected view lag for lag compensation (ticks).
        // A command for tick T was produced about (RTT + buffer) ticks before T,
        // while the client was showing entities `interp` ticks in the past.
        for c in self.clients.values() {
            let Some(id) = c.player else { continue };
            let rtt = self
                .endpoint
                .connection(c.conn)
                .map(|k| k.rtt())
                .unwrap_or(0.0);
            let rate = self.sim.rate.hz() as f64;
            let buffer = (c.newest_cmd as f64 - tick as f64).clamp(0.0, 32.0);
            if let Some(p) = self.sim.player_mut(id) {
                p.expected_view_lag = rtt * rate + c.interp_ticks as f64 + buffer;
            }
        }
        if let Some(r) = &mut self.recorder
            && r.step(tick, &cmds).is_err()
        {
            tracing::warn!("écriture du replay impossible, enregistrement arrêté");
            self.recorder = None;
        }
        let t0 = std::time::Instant::now();
        self.sim.step(&mut cmds);
        let (events, shots) = self.sim.drain();
        self.stats.record_tick(t0.elapsed().as_secs_f64());

        // Grace period for disconnected players.
        let expired: Vec<u8> = self
            .reserved
            .iter()
            .filter(|r| now - r.since > RECONNECT_GRACE)
            .map(|r| r.player.0)
            .collect();
        self.reserved.retain(|r| now - r.since <= RECONNECT_GRACE);
        for slot in expired {
            let _ = self.input(SimInput::Leave { slot });
            self.fill_bots();
        }

        self.dispatch_events(events);
        self.dispatch_shots(shots);
        if tick.is_multiple_of(self.cfg.snapshot_divider) {
            self.send_snapshots(&buffers);
        }
        if now - self.last_players_sent > 0.5 {
            self.last_players_sent = now;
            self.send_players();
        }
        // Back to warmup some time after the end of a match.
        if self.sim.rules.phase == game_rules::Phase::GameOver {
            let at = *self.ended_at.get_or_insert(now);
            if now - at > 20.0 {
                let r = self.change_map(None, None);
                self.note(format!("nouveau match : {r}"));
            }
        }
        let mut events = Vec::new();
        self.endpoint.flush(now, out, &mut events);
        self.handle_events(events, now, out);
        self.stats.clients = self.clients() as u32;
        self.stats.players = self
            .sim
            .iter_players()
            .filter(|p| p.controller == Controller::Human)
            .count() as u32;
        self.stats.bots = self
            .sim
            .iter_players()
            .filter(|p| p.controller == Controller::Bot)
            .count() as u32;
        self.stats.tick = self.sim.tick;
        self.stats.rejected_packets = self.endpoint.rejected_packets;
        for (_, p) in out.iter() {
            self.stats.packets_out += 1;
            self.stats.bytes_out += p.len() as u64;
        }
    }

    fn viewer_of(&self, c: &Client) -> Option<(engine_core::PlayerId, Team)> {
        let id = c.player?;
        let p = self.sim.player(id)?;
        if p.alive {
            return Some((id, p.team));
        }
        // Dead: watch an alive team mate (or anyone in free-for-all/spectator).
        let mate = self.sim.iter_players().find(|o| {
            o.alive
                && !o.is_dummy()
                && (o.team == p.team || !p.team.is_playing() || self.sim.mode() == Mode::Deathmatch)
        });
        Some((mate.map(|m| m.id).unwrap_or(id), p.team))
    }

    fn dispatch_events(&mut self, events: Vec<GameEvent>) {
        if events.is_empty() {
            return;
        }
        for ev in &events {
            match ev {
                GameEvent::SmokeSpawned { id } => {
                    if let Some(s) = self.sim.smokes.iter().find(|s| s.id == *id).map(smoke_net) {
                        self.broadcast(Channel::Reliable, &ServerMsg::Smoke(s));
                    }
                }
                GameEvent::FireStarted { id } => {
                    if let Some(f) = self.sim.fires.iter().find(|f| f.id == *id) {
                        let m = ServerMsg::Fire {
                            id: f.id,
                            cells: f.cells.clone(),
                            start: f.start,
                            end: f.end,
                        };
                        self.broadcast(Channel::Reliable, &m);
                    }
                }
                GameEvent::Kill { killer, victim, .. } => {
                    let name = |id: Option<PlayerId>| {
                        id.and_then(|i| self.sim.player(i))
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| "le monde".into())
                    };
                    self.stats.kills += 1;
                    tracing::debug!("{} a éliminé {}", name(*killer), name(Some(*victim)));
                }
                _ => {}
            }
        }
        let conns: Vec<u32> = self
            .clients
            .values()
            .filter(|c| c.joined)
            .map(|c| c.conn)
            .collect();
        let data = self.data.clone();
        let mv = &data.movement;
        for conn in conns {
            let Some(c) = self.clients.get(&conn) else {
                continue;
            };
            let Some((viewer, team)) = self.viewer_of(c) else {
                continue;
            };
            let me = c.player;
            let eye = self
                .sim
                .player(viewer)
                .map(|p| p.mv.eye(mv))
                .unwrap_or(Vec3::ZERO);
            let mut urgent = Vec::new();
            let mut reliable = Vec::new();
            for ev in &events {
                let keep = match ev {
                    GameEvent::Footstep { pos, .. }
                    | GameEvent::Jump { pos, .. }
                    | GameEvent::Land { pos, .. } => pos.distance(eye) < FOOTSTEP_RANGE,
                    GameEvent::Shot {
                        origin, shooter, ..
                    } => Some(*shooter) == me || origin.distance(eye) < SHOT_RANGE,
                    GameEvent::Reload { player }
                    | GameEvent::DryFire { player }
                    | GameEvent::Zoom { player, .. } => self
                        .sim
                        .player(*player)
                        .is_some_and(|p| p.team == team || p.mv.origin.distance(eye) < 600.0),
                    GameEvent::Bought { player, .. } => {
                        self.sim.player(*player).is_some_and(|p| p.team == team)
                    }
                    GameEvent::PlantStarted { .. } | GameEvent::DefuseStarted { .. } => true,
                    GameEvent::Flashed { player, .. } => {
                        Some(*player) == me
                            || self.sim.player(*player).is_some_and(|p| p.team == team)
                    }
                    GameEvent::Damage {
                        attacker, victim, ..
                    } => *attacker == me || Some(*victim) == me || Some(*victim) == Some(viewer),
                    _ => true,
                };
                if !keep {
                    continue;
                }
                match ev {
                    GameEvent::Shot { .. }
                    | GameEvent::Damage { .. }
                    | GameEvent::Kill { .. }
                    | GameEvent::Flashed { .. }
                    | GameEvent::SmokeBlast { .. } => urgent.push(ev.clone()),
                    GameEvent::Footstep { .. }
                    | GameEvent::Jump { .. }
                    | GameEvent::Land { .. } => urgent.push(ev.clone()),
                    _ => reliable.push(ev.clone()),
                }
            }
            // Urgent messages must fit in one datagram.
            for chunk in chunk_events(urgent) {
                self.send(conn, Channel::Urgent, &ServerMsg::Events(chunk));
            }
            for chunk in chunk_events(reliable) {
                self.send(conn, Channel::Reliable, &ServerMsg::Events(chunk));
            }
        }
    }

    fn dispatch_shots(&mut self, shots: Vec<ShotReport>) {
        for s in shots {
            let Some(conn) = self
                .clients
                .values()
                .find(|c| c.player == Some(s.shooter))
                .map(|c| c.conn)
            else {
                continue;
            };
            let dt = self.sim.dt();
            let net = ShotNet {
                tick: s.tick,
                weapon: s.weapon,
                origin: s.origin,
                dir: s.dir,
                recoil: s.recoil,
                spread_deg: s.spread_deg,
                spread: s.spread,
                speed: s.speed,
                on_ground: s.on_ground,
                rewind_ms: ((s.tick as f64 - s.rewind_time) * dt * 1000.0) as f32,
                rewind_clamped: s.rewind_clamped,
                surfaces: s
                    .surfaces
                    .iter()
                    .take(8)
                    .map(|x| (x.material.clone(), x.thickness, x.damage_after, x.stopped))
                    .collect(),
                hit: s.hit.as_ref().map(|h| {
                    (
                        h.victim.0,
                        h.group,
                        h.distance,
                        h.health_damage,
                        h.armor_damage,
                        h.killed,
                    )
                }),
                end: s.end,
            };
            self.send(conn, Channel::Reliable, &ServerMsg::Shot(Box::new(net)));
        }
    }

    fn send_players(&mut self) {
        let conns: Vec<u32> = self
            .clients
            .values()
            .filter(|c| c.joined)
            .map(|c| c.conn)
            .collect();
        let ping: HashMap<u8, u16> = self
            .clients
            .values()
            .filter_map(|c| {
                Some((
                    c.player?.0,
                    (self.endpoint.connection(c.conn)?.rtt() * 1000.0) as u16,
                ))
            })
            .collect();
        for conn in conns {
            let team = self.clients[&conn]
                .player
                .and_then(|p| self.sim.player(p))
                .map(|p| p.team);
            let list: Vec<PlayerInfo> = self
                .sim
                .iter_players()
                .filter(|p| !p.is_dummy())
                .map(|p| PlayerInfo {
                    id: p.id.0,
                    name: p.name.clone(),
                    team: p.team,
                    kills: p.stats.kills,
                    deaths: p.stats.deaths,
                    assists: p.stats.assists,
                    score: p.stats.score,
                    mvps: p.stats.mvps,
                    headshots: p.stats.headshots,
                    damage: p.stats.damage,
                    money: (Some(p.team) == team && p.team.is_playing()).then_some(p.money),
                    ping_ms: if p.controller == Controller::Bot {
                        0
                    } else {
                        ping.get(&p.id.0).copied().unwrap_or(0)
                    },
                    bot: p.controller == Controller::Bot,
                    alive: p.alive,
                })
                .collect();
            self.send(conn, Channel::Reliable, &ServerMsg::Players(list));
        }
    }

    fn entity(&self, p: &game_sim::Player) -> EntityQ {
        let mut f = 0u16;
        let set = |f: &mut u16, c: bool, b: u16| {
            if c {
                *f |= b
            }
        };
        set(&mut f, p.alive, eflags::ALIVE);
        set(&mut f, p.mv.ducked, eflags::DUCKED);
        set(&mut f, p.mv.on_ground, eflags::ON_GROUND);
        set(&mut f, p.planting_since.is_some(), eflags::PLANTING);
        set(&mut f, p.defusing_since.is_some(), eflags::DEFUSING);
        set(&mut f, p.inv.bomb, eflags::HAS_BOMB);
        set(&mut f, p.helmet, eflags::HELMET);
        set(&mut f, p.hand.reload_until.is_some(), eflags::RELOADING);
        set(&mut f, p.hand.zoom > 0, eflags::ZOOMED);
        set(&mut f, p.controller == Controller::Bot, eflags::BOT);
        set(&mut f, p.is_dummy(), eflags::DUMMY);
        let w = p.active_weapon();
        set(&mut f, w.is_none(), eflags::NO_WEAPON);
        EntityQ {
            id: p.id.0,
            flags: f,
            pos: proto::q_pos(p.mv.origin),
            vel: proto::q_vel(p.mv.velocity),
            yaw: proto::q_yaw(p.view.yaw),
            pitch: (p.view.pitch * 100.0).round() as i16,
            duck: (p.mv.duck_amount.clamp(0.0, 1.0) * 255.0) as u8,
            team: p.team.to_u8(),
            weapon: w.map(|w| w.0).unwrap_or(0),
            health: p.health.clamp(0, 255) as u8,
            armor: p.armor,
        }
    }

    fn self_state(&self, id: PlayerId, spectating: Option<u8>) -> Option<SelfState> {
        let p = self.sim.player(id)?;
        Some(SelfState {
            alive: p.alive,
            team: p.team,
            mv: p.mv,
            health: p.health,
            armor: p.armor,
            helmet: p.helmet,
            defuse_kit: p.defuse_kit,
            money: p.money,
            active: p.active,
            primary: p.inv.primary,
            secondary: p.inv.secondary,
            knife: p.inv.knife,
            grenades: p.inv.grenades.clone(),
            grenade_index: p.grenade_index as u8,
            bomb: p.inv.bomb,
            hand: p.hand,
            flash: p.flash,
            planting_since: p.planting_since,
            defusing_since: p.defusing_since,
            can_buy: self.sim.can_buy_here(id),
            respawn_in: p.respawn_at.map(|t| (t - self.sim.now()).max(0.0) as f32),
            spectating,
        })
    }

    fn bomb_net(&self, team: Team) -> BombNet {
        let now = self.sim.now();
        match &self.sim.bomb {
            BombState::None => BombNet::default(),
            BombState::Carried(id) => {
                // Only the carrier's team knows where the bomb is.
                let p = self.sim.player(*id);
                if p.is_some_and(|p| p.team == team) {
                    BombNet {
                        kind: 1,
                        pos: p.map(|p| p.mv.origin).unwrap_or_default(),
                        carrier: id.0,
                        ..Default::default()
                    }
                } else {
                    BombNet::default()
                }
            }
            BombState::Dropped(pos) => {
                if team == Team::Attack || !team.is_playing() {
                    BombNet {
                        kind: 2,
                        pos: *pos,
                        ..Default::default()
                    }
                } else {
                    BombNet::default()
                }
            }
            BombState::Planted {
                pos,
                site,
                explode_at,
                defuser,
                ..
            } => BombNet {
                kind: 3,
                pos: *pos,
                carrier: 0,
                time_left: (*explode_at - now).max(0.0) as f32,
                defuse_left: defuser.map(|(_, t)| (t - now).max(0.0) as f32),
                site: *site,
            },
            BombState::Exploded(pos) => BombNet {
                kind: 4,
                pos: *pos,
                ..Default::default()
            },
            BombState::Defused(pos) => BombNet {
                kind: 5,
                pos: *pos,
                ..Default::default()
            },
        }
    }

    fn send_snapshots(&mut self, buffers: &[(u32, i64)]) {
        let now = self.sim.now();
        let tick = self.sim.tick;
        let data = self.data.clone();
        let mv = &data.movement;
        let vis = self.cfg.visibility.clone();
        let rate = self.sim.rate.hz() as f64;
        let grenades: Vec<GrenadeQ> = self
            .sim
            .grenades
            .iter()
            .map(|g| GrenadeQ {
                id: g.id,
                kind: g.kind,
                pos: g.pos,
            })
            .collect();
        let drops: Vec<DropQ> = self
            .sim
            .drops
            .iter()
            .take(64)
            .map(|d| DropQ {
                id: d.id,
                weapon: d.weapon,
                pos: d.pos,
            })
            .collect();
        let r = &self.sim.rules;
        let header = (
            r.phase,
            r.time_left(now) as f32,
            r.round,
            r.scores,
            r.sides[0],
        );
        let conns: Vec<u32> = self
            .clients
            .values()
            .filter(|c| c.joined && c.player.is_some())
            .map(|c| c.conn)
            .collect();
        for conn in conns {
            let c = &self.clients[&conn];
            let Some((viewer, team)) = self.viewer_of(c) else {
                continue;
            };
            let me = c.player.expect("joined");
            let rtt = self
                .endpoint
                .connection(conn)
                .map(|k| k.rtt())
                .unwrap_or(0.0);
            let delay = (rtt * 0.5 + c.interp_ticks as f64 / rate) as f32;
            let vp = self.sim.player(viewer);
            let v = Viewer {
                eye: vp.map(|p| p.mv.eye(mv)).unwrap_or_default(),
                velocity: vp.map(|p| p.mv.velocity).unwrap_or_default(),
                delay,
            };
            let mut seen = c.seen.clone();
            let mut entities = Vec::new();
            for p in self.sim.iter_players() {
                if p.id == me {
                    entities.push(self.entity(p));
                    continue;
                }
                let friendly =
                    p.team == team && team.is_playing() && self.sim.mode() != Mode::Deathmatch;
                let send = if friendly || !p.alive || !team.is_playing() || p.is_dummy() {
                    true
                } else {
                    let t = Target {
                        origin: p.mv.origin,
                        velocity: p.mv.velocity,
                        height: if p.mv.ducked { 54.0 } else { 72.0 },
                    };
                    let visible =
                        potentially_visible(&self.sim.level.world, &vis, &v, &t, &|a, b| {
                            game_sim::smoke::smoke_blocks(
                                &self.sim.smokes,
                                a,
                                b,
                                now,
                                &self.sim.cfg.smoke,
                            )
                        });
                    if visible {
                        seen.insert(p.id.0, now);
                        true
                    } else {
                        seen.get(&p.id.0)
                            .is_some_and(|t| now - t < vis.hold_time as f64)
                    }
                };
                if send {
                    entities.push(self.entity(p));
                } else {
                    self.stats.culled_entities += 1;
                }
            }
            let spectating = (viewer != me).then_some(viewer.0);
            let snap = Snapshot {
                tick,
                baseline: c.snapshot_ack,
                last_cmd: tick.saturating_sub(1),
                cmd_buffer: buffers
                    .iter()
                    .find(|b| b.0 == conn)
                    .map(|b| {
                        if b.1 == i64::MIN {
                            i16::MIN
                        } else {
                            b.1.clamp(-1000, 1000) as i16
                        }
                    })
                    .unwrap_or(i16::MIN),
                phase: header.0,
                time_left: header.1,
                round: header.2,
                scores: header.3,
                squad0_side: header.4,
                bomb: self.bomb_net(team),
                me: self.self_state(me, spectating),
                entities,
                grenades: grenades.clone(),
                drops: drops.clone(),
            };
            let base = c
                .baselines
                .iter()
                .find(|b| b.0 == c.snapshot_ack)
                .map(|b| &b.1);
            let mut bytes = proto::encode_snapshot(&snap, base);
            if bytes.len() > MAX_MESSAGE {
                // Too big: send a full snapshot without far-away drops and grenades.
                let mut s = snap.clone();
                s.drops.clear();
                s.grenades.truncate(16);
                bytes = proto::encode_snapshot(&s, None);
            }
            let map = proto::entity_map(&snap.entities);
            let c = self.clients.get_mut(&conn).expect("client");
            c.seen = seen;
            c.baselines.push_back((tick, map));
            while c.baselines.len() > BASELINES {
                c.baselines.pop_front();
            }
            self.stats.snapshot_bytes += bytes.len() as u64;
            self.stats.snapshots += 1;
            if let Some(k) = self.endpoint.connection_mut(conn) {
                let _ = k.send(Channel::Snapshot, bytes);
            }
        }
    }

    /// Ends the current replay cleanly (server shutdown).
    pub fn shutdown(&mut self, now: f64, out: &mut Datagrams) {
        if let Some(r) = &mut self.recorder {
            let _ = r.finish(self.sim.tick);
        }
        self.recorder = None;
        let ids: Vec<u32> = self.clients.keys().copied().collect();
        for id in ids {
            self.send(
                id,
                Channel::Reliable,
                &ServerMsg::Kicked("arrêt du serveur".into()),
            );
            self.endpoint.flush_one(id, now, out);
            self.endpoint.kick(id, "shutdown", now, out);
        }
    }

    pub fn status(&self) -> crate::stats::Status {
        crate::stats::Status {
            name: self.cfg.name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            map: self.cfg.map.clone(),
            profile: self.cfg.profile.clone(),
            tick_rate: self.sim.rate.hz(),
            tick: self.sim.tick,
            phase: format!("{:?}", self.sim.rules.phase),
            round: self.sim.rules.round,
            scores: self.sim.rules.scores,
            players: self
                .sim
                .iter_players()
                .filter(|p| !p.is_dummy())
                .map(|p| {
                    (
                        p.name.clone(),
                        format!("{:?}", p.team),
                        p.stats.kills,
                        p.stats.deaths,
                        p.controller == Controller::Bot,
                    )
                })
                .collect(),
            stats: self.stats.clone(),
        }
    }
}

fn smoke_net(s: &game_sim::SmokeVolume) -> SmokeNet {
    let mut bits = vec![0u8; s.filled.len().div_ceil(8)];
    for (i, f) in s.filled.iter().enumerate() {
        if *f {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    SmokeNet {
        id: s.id,
        center: s.center,
        origin: s.origin,
        dims: s.dims,
        voxel: s.voxel,
        bits,
        start: s.start,
        end: s.end,
    }
}

/// Splits events into messages that fit in one datagram.
fn chunk_events(events: Vec<GameEvent>) -> Vec<Vec<GameEvent>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut size = 8;
    for e in events {
        let n = proto::encode(&e).len();
        if n > MAX_MESSAGE - 16 {
            continue;
        }
        if size + n > MAX_MESSAGE - 16 && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            size = 8;
        }
        size += n;
        cur.push(e);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
