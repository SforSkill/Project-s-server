//! `projet-s-server`: headless dedicated server.
//!
//! ```text
//! projet-s-server [--config server.toml] [--map ps_citadel] [--mode competitive]
//!                 [--port 27015] [--bots 9] [--fill-bots 1] [--admin 127.0.0.1:27016|off]
//! ```
//! The admin token is read from the `PROJET_S_ADMIN_TOKEN` environment variable
//! (never from a versioned file). Without it, `/status` and `/command` are off.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use game_server::run::ServerHandle;
use game_server::stats::{Status, TICK_BUCKETS_MS};
use game_server::{ServerConfig, find_data_dir, start};
use game_sim::GameData;
use std::fmt::Write as _;
use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const HELP: &str = "projet-s-server — serveur dédié Projet S

Options (toutes facultatives) :
  --config FICHIER      fichier server.toml
  --data DOSSIER        dossier contenant configs/ et maps/
  --name NOM            nom du serveur
  --port N | --bind A   adresse UDP de jeu (défaut 0.0.0.0:27015)
  --admin ADRESSE|off   API HTTP d'administration (défaut 127.0.0.1:27016)
  --map NOM             ps_citadel, ps_duel_lab, ps_training_lab
  --mode PROFIL         competitive, competitive_mr12, competitive_mr16, duel, deathmatch, training
  --tick N              fréquence de simulation (défaut 128)
  --max-clients N       clients simultanés (défaut 16)
  --bots N              bots ajoutés au démarrage
  --fill-bots 1         compléter les équipes avec des bots
  --bot-difficulty X    0 à 1
  --replays DOSSIER|off enregistrer les replays
  --lagcomp off         désactiver la compensation de latence
  --pim off             désactiver le filtrage de visibilité
  --seed N              graine (tests reproductibles)
  --check               valider la configuration et les données puis quitter

Variables d'environnement :
  PROJET_S_ADMIN_TOKEN  jeton Bearer de /status et /command (désactivés sans lui)
  RUST_LOG              niveau de journalisation (info par défaut)
  PROJET_S_LOG_JSON=1   journaux JSON (conteneurs)

Commandes sur l'entrée standard : bots N, kickbots, start, restart, map NOM,
mode PROFIL, say TEXTE, kick NOM, status, quit";

fn main() {
    let json = std::env::var("PROJET_S_LOG_JSON").is_ok_and(|v| v == "1");
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    if let Err(e) = run() {
        eprintln!("erreur : {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("projet-s-server {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let check = args.iter().any(|a| a == "--check");
    let mut cfg = ServerConfig::default();
    if let Some(i) = args.iter().position(|a| a == "--config") {
        let path = args.get(i + 1).ok_or("--config : fichier attendu")?;
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path} : {e}"))?;
        cfg = ServerConfig::from_toml(&text)?;
    }
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--check" {
            i += 1;
            continue;
        }
        let key = a
            .strip_prefix("--")
            .ok_or_else(|| format!("argument inattendu « {a} » (voir --help)"))?;
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("--{key} : valeur attendue"))?;
        if key != "config" {
            cfg.apply_arg(key, value)?;
        }
        i += 2;
    }
    cfg.validate()?;
    let dir = find_data_dir(cfg.data_dir.as_deref())?;
    let data = Arc::new(GameData::load(&dir)?);
    tracing::info!(
        "données : {} (empreinte {:016x}), {} cartes",
        dir.display(),
        data.fingerprint(),
        data.maps.len()
    );
    if check {
        // Builds the server without opening sockets.
        game_server::Server::new(cfg.clone(), data)?;
        println!(
            "configuration valide : carte {}, mode {}, {} Hz",
            cfg.map, cfg.profile, cfg.tick_rate
        );
        return Ok(());
    }
    let admin_bind = cfg.admin_bind.clone();
    let handle = start(cfg, data)?;
    tracing::info!("UDP {}", handle.addr);
    let quit = Arc::new(AtomicBool::new(false));
    {
        let quit = quit.clone();
        ctrlc::set_handler(move || quit.store(true, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }
    let handle = Arc::new(handle);
    if let Some(bind) = admin_bind {
        let token = std::env::var("PROJET_S_ADMIN_TOKEN")
            .ok()
            .filter(|t| t.len() >= 16);
        if token.is_none() {
            tracing::warn!(
                "PROJET_S_ADMIN_TOKEN absent ou trop court (16 caractères min.) : /status et /command désactivés"
            );
        }
        let h = handle.clone();
        std::thread::Builder::new()
            .name("projet-s-admin".into())
            .spawn(move || admin(bind, token, h))
            .map_err(|e| e.to_string())?;
    }
    // Console commands (ignored when stdin is closed, e.g. under systemd).
    {
        let h = handle.clone();
        let quit = quit.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if line == "quit" || line == "exit" {
                    quit.store(true, Ordering::SeqCst);
                    break;
                }
                if let Some(r) = h.command(&line) {
                    println!("{r}");
                }
            }
        });
    }
    while !quit.load(Ordering::SeqCst) && handle.is_running() {
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::info!("arrêt demandé");
    match Arc::try_unwrap(handle) {
        Ok(h) => h.shutdown(),
        Err(h) => h.stop.store(true, Ordering::SeqCst),
    }
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

#[derive(Clone)]
struct AdminState {
    token: Option<String>,
    server: Arc<ServerHandle>,
}

fn authorized(st: &AdminState, headers: &HeaderMap) -> bool {
    let Some(token) = &st.token else { return false };
    let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(given) = v.strip_prefix("Bearer ") else {
        return false;
    };
    // Constant-time comparison.
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn snapshot(st: &AdminState) -> Option<Status> {
    st.server.status.lock().ok()?.clone()
}

async fn health(State(st): State<AdminState>) -> impl IntoResponse {
    if st.server.is_running() {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "stopped")
    }
}

async fn status(State(st): State<AdminState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers) {
        return (StatusCode::UNAUTHORIZED, "unauthorized".to_string());
    }
    match snapshot(&st) {
        Some(s) => (
            StatusCode::OK,
            serde_json::to_string_pretty(&s).unwrap_or_default(),
        ),
        None => (StatusCode::SERVICE_UNAVAILABLE, "no status".into()),
    }
}

async fn command(
    State(st): State<AdminState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if !authorized(&st, &headers) {
        return (StatusCode::UNAUTHORIZED, "unauthorized".to_string());
    }
    let line: String = body.chars().take(200).collect();
    let server = st.server.clone();
    let r = tokio::task::spawn_blocking(move || server.command(line.trim()))
        .await
        .ok()
        .flatten();
    match r {
        Some(r) => (StatusCode::OK, r),
        None => (StatusCode::SERVICE_UNAVAILABLE, "server busy".into()),
    }
}

/// Prometheus text exposition, built with `prometheus-client`.
async fn metrics(State(st): State<AdminState>) -> impl IntoResponse {
    use prometheus_client::encoding::text::encode;
    use prometheus_client::metrics::counter::Counter;
    use prometheus_client::metrics::gauge::Gauge;
    use prometheus_client::registry::Registry;
    let Some(s) = snapshot(&st) else {
        return (StatusCode::SERVICE_UNAVAILABLE, String::new());
    };
    let x = &s.stats;
    let mut reg = Registry::with_prefix("projet_s");
    let gauge = |reg: &mut Registry, name: &str, help: &str, v: f64| {
        let g = Gauge::<f64, std::sync::atomic::AtomicU64>::default();
        g.set(v);
        reg.register(name, help, g);
    };
    let counter = |reg: &mut Registry, name: &str, help: &str, v: u64| {
        let c = Counter::<u64>::default();
        c.inc_by(v);
        reg.register(name, help, c);
    };
    gauge(&mut reg, "uptime_seconds", "Server uptime", x.uptime);
    gauge(
        &mut reg,
        "tick_rate_hz",
        "Simulation rate",
        s.tick_rate as f64,
    );
    gauge(&mut reg, "tick", "Current simulation tick", x.tick as f64);
    gauge(&mut reg, "clients", "Connected clients", x.clients as f64);
    gauge(&mut reg, "players_human", "Human players", x.players as f64);
    gauge(&mut reg, "players_bot", "Bots", x.bots as f64);
    gauge(
        &mut reg,
        "tick_duration_avg_seconds",
        "Average step duration (last 128 ticks)",
        x.tick_avg,
    );
    gauge(
        &mut reg,
        "tick_duration_p99_seconds",
        "p99 step duration (last 128 ticks)",
        x.tick_p99,
    );
    gauge(
        &mut reg,
        "tick_duration_max_seconds",
        "Max step duration (last 128 ticks)",
        x.tick_max,
    );
    counter(&mut reg, "packets_in", "Datagrams received", x.packets_in);
    counter(&mut reg, "packets_out", "Datagrams sent", x.packets_out);
    counter(&mut reg, "bytes_in", "Bytes received", x.bytes_in);
    counter(&mut reg, "bytes_out", "Bytes sent", x.bytes_out);
    counter(
        &mut reg,
        "rejected_packets",
        "Packets rejected by the transport",
        x.rejected_packets,
    );
    counter(
        &mut reg,
        "bad_messages",
        "Undecodable game messages",
        x.bad_messages,
    );
    counter(
        &mut reg,
        "missing_cmds",
        "Ticks simulated without a fresh client command",
        x.missing_cmds,
    );
    counter(
        &mut reg,
        "late_ticks",
        "Ticks started more than one period late",
        x.late_ticks,
    );
    counter(&mut reg, "snapshots", "Snapshots sent", x.snapshots);
    counter(
        &mut reg,
        "snapshot_bytes",
        "Snapshot payload bytes",
        x.snapshot_bytes,
    );
    counter(
        &mut reg,
        "culled_entities",
        "Enemies withheld by predictive visibility",
        x.culled_entities,
    );
    counter(&mut reg, "kills", "Kills", x.kills);
    let mut text = String::new();
    if encode(&mut text, &reg).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, String::new());
    }
    // Step-duration histogram (cumulative buckets, Prometheus convention).
    let eof = text.rfind("# EOF").unwrap_or(text.len());
    text.truncate(eof);
    let _ = writeln!(
        text,
        "# HELP projet_s_tick_duration_seconds Simulation step duration.\n# TYPE projet_s_tick_duration_seconds histogram"
    );
    let mut acc = 0;
    for (i, b) in TICK_BUCKETS_MS.iter().enumerate() {
        acc += x.tick_hist[i];
        let _ = writeln!(
            text,
            "projet_s_tick_duration_seconds_bucket{{le=\"{}\"}} {acc}",
            b / 1000.0
        );
    }
    acc += x.tick_hist[6];
    let _ = writeln!(
        text,
        "projet_s_tick_duration_seconds_bucket{{le=\"+Inf\"}} {acc}"
    );
    let _ = writeln!(
        text,
        "projet_s_tick_duration_seconds_sum {}\nprojet_s_tick_duration_seconds_count {}\n# EOF",
        x.tick_sum, x.tick_count
    );
    (StatusCode::OK, text)
}

fn admin(bind: String, token: Option<String>, server: Arc<ServerHandle>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("API admin : {e}");
            return;
        }
    };
    rt.block_on(async move {
        let st = AdminState { token, server };
        let app = Router::new()
            .route("/health", get(health))
            .route("/metrics", get(metrics))
            .route("/status", get(status))
            .route("/command", post(command))
            .with_state(st);
        match tokio::net::TcpListener::bind(&bind).await {
            Ok(l) => {
                tracing::info!(
                    "API admin sur http://{bind} (/health, /metrics, /status, /command)"
                );
                if let Err(e) = axum::serve(l, app).await {
                    tracing::error!("API admin : {e}");
                }
            }
            Err(e) => tracing::error!("API admin {bind} : {e}"),
        }
    });
}
