//! Server counters (exported as Prometheus metrics and in `/status`).

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServerStats {
    pub uptime: f64,
    pub tick: u32,
    pub clients: u32,
    pub players: u32,
    pub bots: u32,
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub rejected_packets: u64,
    pub bad_messages: u64,
    pub missing_cmds: u64,
    pub snapshots: u64,
    pub snapshot_bytes: u64,
    pub culled_entities: u64,
    pub kills: u64,
    /// Ticks that started later than one tick period after their deadline.
    pub late_ticks: u64,
    /// Simulation step durations over the last window (seconds).
    pub tick_last: f64,
    pub tick_avg: f64,
    pub tick_max: f64,
    pub tick_p99: f64,
    /// All-time step-duration histogram buckets (upper bounds in ms: 0.25, 0.5, 1, 2, 4, 7.8, +inf).
    pub tick_hist: [u64; 7],
    pub tick_sum: f64,
    pub tick_count: u64,
    #[serde(skip)]
    window: Vec<f64>,
}

pub const TICK_BUCKETS_MS: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 7.8125];

impl ServerStats {
    pub fn record_tick(&mut self, secs: f64) {
        self.tick_last = secs;
        self.tick_sum += secs;
        self.tick_count += 1;
        let ms = secs * 1000.0;
        let b = TICK_BUCKETS_MS
            .iter()
            .position(|&u| ms <= u)
            .unwrap_or(TICK_BUCKETS_MS.len());
        self.tick_hist[b] += 1;
        self.window.push(secs);
        if self.window.len() >= 128 {
            let mut w = std::mem::take(&mut self.window);
            w.sort_by(f64::total_cmp);
            self.tick_avg = w.iter().sum::<f64>() / w.len() as f64;
            self.tick_max = *w.last().unwrap_or(&0.0);
            self.tick_p99 = w[((w.len() as f64 * 0.99) as usize).min(w.len() - 1)];
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub name: String,
    pub version: String,
    pub map: String,
    pub profile: String,
    pub tick_rate: u32,
    pub tick: u32,
    pub phase: String,
    pub round: u16,
    pub scores: [u16; 2],
    /// (name, team, kills, deaths, bot)
    pub players: Vec<(String, String, u32, u32, bool)>,
    pub stats: ServerStats,
}
