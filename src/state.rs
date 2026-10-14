use crate::config::Config;
use crate::protocol::{base_url, NodeInfo, ProxyInfo};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct PeerEntry {
    pub info: NodeInfo,
    /// Node ids this peer claims to reach directly.
    pub links: Vec<String>,
    pub proxies: Vec<ProxyInfo>,
    pub reachable: Vec<String>,
    pub internet: bool,
    /// Credential we use to talk to this peer.
    pub auth: Option<(String, String)>,
    /// The address (ip, port) we actually reached this peer at. This is the
    /// configured address, which may differ from the peer's self-advertised
    /// address (e.g. a cloud node behind NAT advertises a private IP).
    pub dial: Option<(String, u16)>,
    /// True when this node is one of our own configured join targets (we hold
    /// its credential). Gossip-only nodes are not probed directly.
    pub configured: bool,
    /// Measured by us with a live health probe.
    pub directly_reachable: bool,
    pub latency_ms: Option<u64>,
    /// Reason the last probe failed (for diagnostics in the UI).
    pub last_error: Option<String>,
    /// Last time we heard anything (gossip or health probe).
    pub last_seen_ms: u64,
}

/// Status of a configured join target (may be unreachable / not yet seen).
#[derive(Clone, Debug)]
pub struct TargetStatus {
    pub name: String,
    pub address: String,
    pub ip: String,
    pub port: Option<u16>,
    pub reachable: bool,
    pub error: Option<String>,
    pub latency_ms: Option<u64>,
}

impl PeerEntry {
    /// URL to reach this peer: use the address we actually dialed when known.
    pub fn addr_url(&self) -> String {
        match &self.dial {
            Some((ip, port)) => base_url(ip, *port),
            None => self.info.base_url(),
        }
    }

    /// Address to display in the UI (prefer the dialed address).
    pub fn display_addr(&self) -> (String, u16) {
        match &self.dial {
            Some((ip, port)) => (ip.clone(), *port),
            None => (self.info.ip.clone(), self.info.port),
        }
    }
}

pub struct AppState {
    pub cfg: RwLock<Config>,
    pub config_path: PathBuf,
    pub self_info: NodeInfo,
    pub listen_port: u16,
    pub started: Instant,
    pub peers: RwLock<HashMap<String, PeerEntry>>,
    /// Last probe result per configured target address.
    pub targets: RwLock<HashMap<String, TargetStatus>>,
    /// The subset of our configured `reachable` entries that a live TCP probe
    /// has confirmed we can actually connect to right now. This is what we
    /// advertise to the mesh.
    pub reachable_live: RwLock<Vec<String>>,
    /// Names of our configured egress proxies whose endpoint is actually
    /// listening (live TCP probe). Only these are advertised / used.
    pub proxies_live: RwLock<Vec<String>>,
    /// Egress proxies auto-detected on common local ports (Clash etc).
    pub proxies_auto: RwLock<Vec<ProxyInfo>>,
    /// Cache of on-demand reachability discovery: target `host:port` ->
    /// (owning node id, egress, timestamp). `None` node means "nobody can".
    pub reach_cache: RwLock<HashMap<String, (Option<String>, Option<String>, u64)>>,
    /// Plain client for control-plane traffic (no proxy).
    pub control: reqwest::Client,
    /// Client used to forward tunnel requests between nodes.
    pub tunnel: reqwest::Client,
}

impl AppState {
    pub fn new(cfg: Config, config_path: PathBuf, self_info: NodeInfo, listen_port: u16) -> Self {
        let control = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()
            .expect("control client");
        let tunnel = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("tunnel client");
        AppState {
            cfg: RwLock::new(cfg),
            config_path,
            self_info,
            listen_port,
            started: Instant::now(),
            peers: RwLock::new(HashMap::new()),
            targets: RwLock::new(HashMap::new()),
            reachable_live: RwLock::new(Vec::new()),
            proxies_live: RwLock::new(Vec::new()),
            proxies_auto: RwLock::new(Vec::new()),
            reach_cache: RwLock::new(HashMap::new()),
            control,
            tunnel,
        }
    }

    pub async fn config(&self) -> Config {
        self.cfg.read().await.clone()
    }

    /// Reachable entries confirmed by the latest live probe.
    pub async fn live_reachable(&self) -> Vec<String> {
        self.reachable_live.read().await.clone()
    }

    /// Names of egress proxies confirmed alive by the latest probe.
    pub async fn live_proxy_names(&self) -> Vec<String> {
        let mut names = self.proxies_live.read().await.clone();
        for p in self.proxies_auto.read().await.iter() {
            if !names.contains(&p.name) {
                names.push(p.name.clone());
            }
        }
        names
    }

    /// Resolve a proxy name to its definition (configured first, then auto).
    pub async fn find_proxy(&self, name: &str) -> Option<ProxyInfo> {
        if let Some(p) = self
            .cfg
            .read()
            .await
            .proxies
            .iter()
            .find(|p| p.name == name)
        {
            return Some(ProxyInfo {
                name: p.name.clone(),
                kind: p.kind.clone(),
                url: p.url.clone(),
            });
        }
        self.proxies_auto
            .read()
            .await
            .iter()
            .find(|p| p.name == name)
            .cloned()
    }

    /// Advertised egress proxies (live configured ones plus auto-detected).
    pub async fn live_proxies(&self) -> Vec<ProxyInfo> {
        let live = self.proxies_live.read().await.clone();
        let mut out: Vec<ProxyInfo> = self
            .cfg
            .read()
            .await
            .proxies
            .iter()
            .filter(|p| live.contains(&p.name))
            .map(|p| ProxyInfo {
                name: p.name.clone(),
                kind: p.kind.clone(),
                url: p.url.clone(),
            })
            .collect();
        for p in self.proxies_auto.read().await.iter() {
            if !out.iter().any(|o| o.name == p.name) {
                out.push(p.clone());
            }
        }
        out
    }

    pub async fn set_config(&self, cfg: Config) {
        let mut guard = self.cfg.write().await;
        *guard = cfg;
    }

    pub async fn api_auth(&self) -> Option<(String, String)> {
        self.cfg.read().await.api_auth()
    }

    pub async fn admin_auth(&self) -> Option<(String, String)> {
        self.cfg.read().await.admin_auth()
    }
}
