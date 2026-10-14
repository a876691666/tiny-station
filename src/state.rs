use crate::config::Config;
use crate::crypto;
use crate::identity::Identity;
use crate::keys::KeyStore;
use crate::protocol::{base_url, NodeInfo, ProxyInfo, ReverseTask, TunnelResponse};
use crate::replay::ReplayCache;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use x25519_dalek::{PublicKey, StaticSecret};

/// How long a hub holds a leaf's reverse poll open before answering "no work".
pub const REVERSE_POLL_SECS: u64 = 20;
/// A reverse channel is considered dead if the leaf hasn't polled within this.
pub const REVERSE_STALE_MS: u64 = 60_000;
/// How long a negotiated pairwise session key stays valid before re-handshake.
pub const SESSION_TTL_MS: u64 = 300_000;
/// How long this node's handshake ephemeral keypair is used before rotating.
/// Rotating it provides forward secrecy across rotations.
pub const EPHEMERAL_TTL_MS: u64 = 900_000;

/// Current handshake ephemeral keypair: secret, public bytes, creation time.
struct Ephemeral {
    secret: StaticSecret,
    public: [u8; 32],
    created_ms: u64,
}

/// A negotiated pairwise session: an ephemeral ECDH key mixed with the cluster
/// key, valid for a bounded time.
#[derive(Clone, Debug)]
pub struct Session {
    pub key: [u8; 32],
    /// The cluster epoch the peer reported at handshake time (used to decide
    /// whether to push a newer cluster key to it).
    pub peer_epoch: u64,
    pub expires_ms: u64,
}

/// Hub-side handle for one NAT'd leaf's reverse channel.
pub struct ReverseNode {
    /// Delivers tasks to the leaf's currently open (or next) poll.
    pub tx: mpsc::UnboundedSender<ReverseTask>,
    /// The leaf's task receiver; only one poll may be waiting on it at a time.
    pub rx: Mutex<mpsc::UnboundedReceiver<ReverseTask>>,
    /// Last time the leaf polled (for liveness / reaping).
    pub last_seen_ms: AtomicU64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_ephemeral() -> Ephemeral {
    let secret = StaticSecret::from(crypto::random_bytes::<32>());
    let public = PublicKey::from(&secret).to_bytes();
    Ephemeral {
        secret,
        public,
        created_ms: now_ms(),
    }
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
    /// This node's auto-generated X25519 identity.
    pub identity: Identity,
    /// Key that authenticates the enrollment handshake (derived from the root
    /// `mesh_key`). Stable across cluster-key rotations.
    pub handshake_key: [u8; 32],
    /// Rotatable, versioned cluster key shared by the whole mesh.
    pub keys: RwLock<KeyStore>,
    /// This node's *current* handshake ephemeral keypair (rotated periodically).
    eph: RwLock<Ephemeral>,
    /// Per-peer negotiated session keys, keyed by peer node id.
    pub sessions: RwLock<HashMap<String, Session>>,
    /// Rejects replayed sealed messages inside the clock-skew window.
    pub replay: ReplayCache,
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
    /// Hub-side reverse channels: node id -> channel to a NAT'd leaf.
    pub reverse: RwLock<HashMap<String, Arc<ReverseNode>>>,
    /// Reverse calls awaiting a result, keyed by request id.
    pub reverse_pending: RwLock<HashMap<String, oneshot::Sender<TunnelResponse>>>,
    /// Leaves we've already spawned a reverse poll loop for.
    pub reverse_loops: RwLock<HashSet<String>>,
    /// Persistent key/value store exposed to kernel scripts (`ctx.store`).
    pub kernel_store: Arc<crate::kernel::store::Store>,
    /// The active hot-swappable kernel, if one is loaded.
    pub kernel: RwLock<Option<Arc<crate::kernel::KernelHandle>>>,
    /// Kernel metrics exposed to scripts (`ctx.metrics`).
    pub kernel_metrics: Arc<crate::kernel::metrics::Metrics>,
    /// Anti-rollback / current-revision bookkeeping for kernel bundles.
    pub kernel_meta: RwLock<crate::kernel::bundle::KernelMeta>,
}

impl AppState {
    pub fn new(
        cfg: Config,
        config_path: PathBuf,
        self_info: NodeInfo,
        listen_port: u16,
    ) -> anyhow::Result<Self> {
        let root = cfg.mesh_key().unwrap_or_default().to_string();
        let identity = Identity::load_or_create(&crate::config::identity_file_for(&config_path))?;
        let keys = KeyStore::load_or_init(&crate::config::cluster_file_for(&config_path), &root);
        let handshake_key = crypto::handshake_key(root.as_bytes());
        let control = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()
            .expect("control client");
        let tunnel = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("tunnel client");
        let kernel_store = Arc::new(crate::kernel::store::Store::load(
            crate::config::kernel_store_file_for(&config_path),
        ));
        let kernel_meta = std::fs::read_to_string(crate::config::kernel_meta_file_for(&config_path))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Ok(AppState {
            cfg: RwLock::new(cfg),
            config_path,
            self_info,
            listen_port,
            started: Instant::now(),
            identity,
            handshake_key,
            keys: RwLock::new(keys),
            eph: RwLock::new(new_ephemeral()),
            sessions: RwLock::new(HashMap::new()),
            peers: RwLock::new(HashMap::new()),
            targets: RwLock::new(HashMap::new()),
            reachable_live: RwLock::new(Vec::new()),
            proxies_live: RwLock::new(Vec::new()),
            proxies_auto: RwLock::new(Vec::new()),
            reach_cache: RwLock::new(HashMap::new()),
            control,
            tunnel,
            reverse: RwLock::new(HashMap::new()),
            reverse_pending: RwLock::new(HashMap::new()),
            reverse_loops: RwLock::new(HashSet::new()),
            replay: ReplayCache::new(),
            kernel_store,
            kernel: RwLock::new(None),
            kernel_metrics: Arc::new(crate::kernel::metrics::Metrics::default()),
            kernel_meta: RwLock::new(kernel_meta),
        })
    }

    /// Get (creating if needed) the reverse-channel slot for a leaf node.
    pub async fn reverse_slot(&self, id: &str) -> Arc<ReverseNode> {
        if let Some(n) = self.reverse.read().await.get(id) {
            return n.clone();
        }
        let mut map = self.reverse.write().await;
        map.entry(id.to_string())
            .or_insert_with(|| {
                let (tx, rx) = mpsc::unbounded_channel();
                Arc::new(ReverseNode {
                    tx,
                    rx: Mutex::new(rx),
                    last_seen_ms: AtomicU64::new(0),
                })
            })
            .clone()
    }

    pub async fn reverse_touch(&self, id: &str) {
        if let Some(n) = self.reverse.read().await.get(id) {
            n.last_seen_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    /// True if a fresh reverse channel to `id` exists (so we can route via it).
    pub async fn reverse_fresh(&self, id: &str) -> bool {
        let now = now_ms();
        self.reverse
            .read()
            .await
            .get(id)
            .map(|n| now.saturating_sub(n.last_seen_ms.load(Ordering::Relaxed)) <= REVERSE_STALE_MS)
            .unwrap_or(false)
    }

    /// Drop a reverse channel (leaf went away).
    pub async fn reverse_drop(&self, id: &str) {
        self.reverse.write().await.remove(id);
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

    /// Return this node's current handshake ephemeral keypair, rotating it when
    /// it is older than `EPHEMERAL_TTL_MS`.
    pub async fn ephemeral(&self) -> (StaticSecret, [u8; 32]) {
        {
            let g = self.eph.read().await;
            if now_ms().saturating_sub(g.created_ms) < EPHEMERAL_TTL_MS {
                return (g.secret.clone(), g.public);
            }
        }
        let mut g = self.eph.write().await;
        if now_ms().saturating_sub(g.created_ms) >= EPHEMERAL_TTL_MS {
            *g = new_ephemeral();
            tracing::debug!("已轮换握手临时密钥");
        }
        (g.secret.clone(), g.public)
    }

    /// A live (non-expired) session with `id`, if any.
    pub async fn session(&self, id: &str) -> Option<Session> {
        let s = self.sessions.read().await.get(id).cloned();
        match s {
            Some(x) if x.expires_ms > now_ms() => Some(x),
            _ => None,
        }
    }

    pub async fn put_session(&self, id: String, s: Session) {
        self.sessions.write().await.insert(id, s);
    }

    pub async fn cluster_epoch(&self) -> u64 {
        self.keys.read().await.epoch()
    }

    pub async fn cluster_key(&self) -> [u8; 32] {
        self.keys.read().await.current_key()
    }

    /// Adopt a newer cluster key (from a handshake or a rotation broadcast).
    pub async fn accept_rotation(&self, epoch: u64, key: [u8; 32]) -> bool {
        self.keys.write().await.accept(epoch, key)
    }

    pub async fn api_auth(&self) -> Option<(String, String)> {
        self.cfg.read().await.api_auth()
    }

    pub async fn admin_auth(&self) -> Option<(String, String)> {
        self.cfg.read().await.admin_auth()
    }

    /// The active kernel, if any. Callers on the hot path should clone this once
    /// per request rather than holding the lock.
    pub async fn kernel(&self) -> Option<Arc<crate::kernel::KernelHandle>> {
        self.kernel.read().await.clone()
    }

    /// Install (or clear) the active kernel. Existing in-flight requests keep
    /// their own `Arc` and finish on the old kernel.
    pub async fn set_kernel(&self, k: Option<Arc<crate::kernel::KernelHandle>>) {
        *self.kernel.write().await = k;
    }

    pub fn kernel_meta_path(&self) -> PathBuf {
        crate::config::kernel_meta_file_for(&self.config_path)
    }

    pub fn kernel_prev_path(&self) -> PathBuf {
        crate::config::kernel_prev_file_for(&self.config_path)
    }

    pub async fn kernel_path(&self) -> PathBuf {
        self.cfg.read().await.kernel_path(&self.config_path)
    }

    /// The kernel module set currently installed on disk, if any.
    pub async fn kernel_module_set(&self) -> Option<crate::kernel::ModuleSet> {
        crate::kernel::disk::load(&self.kernel_path().await).ok()
    }

    pub async fn kernel_meta(&self) -> crate::kernel::bundle::KernelMeta {
        self.kernel_meta.read().await.clone()
    }

    /// Persist a new kernel revision record (atomic write).
    pub async fn set_kernel_meta(&self, m: crate::kernel::bundle::KernelMeta) {
        if let Ok(raw) = serde_json::to_string(&m) {
            if let Err(e) = crate::atomic::write(&self.kernel_meta_path(), raw.as_bytes()) {
                tracing::warn!("内核 meta 落盘失败: {}", e);
            }
        }
        *self.kernel_meta.write().await = m;
    }
}
