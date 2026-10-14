use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub port: u16,
    pub version: String,
}

impl NodeInfo {
    pub fn base_url(&self) -> String {
        base_url(&self.ip, self.port)
    }
}

pub fn base_url(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        format!("http://[{}]:{}", ip, port)
    } else {
        format!("http://{}:{}", ip, port)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProxyInfo {
    pub name: String,
    pub kind: String,
    pub url: String,
}

/// A node's advertised view of itself (or of a sibling). Gossiped around the
/// mesh so every node can compute routes without talking to everyone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerView {
    pub info: NodeInfo,
    /// IDs of nodes this node can reach directly.
    pub links: Vec<String>,
    pub proxies: Vec<ProxyInfo>,
    pub reachable: Vec<String>,
    pub internet: bool,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub last_seen_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct GossipBody {
    pub view: Vec<PeerView>,
}

/// Empty payload for request/response pairs that carry no data of their own
/// (identity probe, peer-view pull). Still timestamped + sealed on the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ping;

/// `POST /api/reach` request: ask a peer whether it can reach `host:port`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReachQuery {
    pub host: String,
    pub port: u16,
}

/// `POST /api/reach` reply.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReachAnswer {
    pub reachable: bool,
    #[serde(default)]
    pub egress: Option<String>,
}

// ---------------- secure channel (enrollment + rotation) ----------------

/// Handshake request, sealed with the root handshake key (PSK). Carries the
/// sender's static identity, its current ephemeral X25519 key and its current
/// cluster epoch/key so the peer can adopt a newer rotation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub node: NodeInfo,
    pub id_pub: [u8; 32],
    pub eph_pub: [u8; 32],
    pub epoch: u64,
    pub cluster: [u8; 32],
}

/// Handshake reply, sealed with the root handshake key. `cluster`/`epoch` carry
/// the peer's (possibly newer) cluster key so a late joiner syncs it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloAck {
    pub node: NodeInfo,
    pub id_pub: [u8; 32],
    pub eph_pub: [u8; 32],
    pub epoch: u64,
    pub cluster: [u8; 32],
}

/// A cluster-key rotation, broadcast across the mesh (sealed with the pairwise
/// session key). Receivers adopt it when `epoch` is newer and re-propagate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyRotate {
    pub epoch: u64,
    pub key: [u8; 32],
}

/// A request travelling across the mesh. `dest` is the ultimate node that must
/// execute it (because only that node can reach the target / owns the egress).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TunnelRequest {
    pub dest: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body_b64: String,
    /// Egress proxy name to use on the executing node.
    #[serde(default)]
    pub egress: Option<String>,
    /// Nodes already visited, used to detect loops.
    #[serde(default)]
    pub hops: Vec<String>,
    /// Optional upstream timeout in milliseconds. Forwarded unchanged across
    /// hops; only the executing node applies it to the outbound request.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TunnelResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body_b64: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub served_by: String,
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
}

impl TunnelResponse {
    pub fn error(status: u16, msg: impl Into<String>) -> Self {
        TunnelResponse {
            status,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body_b64: String::new(),
            error: Some(msg.into()),
            served_by: String::new(),
            path: vec![],
            elapsed_ms: 0,
        }
    }
}

// ---------------- reverse channel (NAT traversal) ----------------

/// A unit of work handed to a NAT'd node over its reverse channel. The node
/// executes the enclosed tunnel request locally and returns the response on its
/// next poll. Used when a hub cannot dial the node directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReverseTask {
    pub req_id: String,
    pub request: TunnelRequest,
}

/// A leaf asking its hub for work: "here is the result of your previous task
/// (if any) — do you have more for me?"
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReversePoll {
    pub node: NodeInfo,
    #[serde(default)]
    pub result: Option<ReverseResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReverseResult {
    pub req_id: String,
    pub response: TunnelResponse,
}

/// The hub's reply: a task to execute, or `null` to keep the channel warm.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ReversePollResp {
    #[serde(default)]
    pub task: Option<ReverseTask>,
}

// ---------------- status / dashboard payloads ----------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeStatus {
    pub info: NodeInfo,
    pub is_self: bool,
    pub directly_reachable: bool,
    pub latency_ms: Option<u64>,
    pub last_seen_ms: u64,
    pub links: Vec<String>,
    pub proxies: Vec<ProxyInfo>,
    pub reachable: Vec<String>,
    pub internet: bool,
    /// Why the last health probe failed (shown in red in the UI).
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteEntry {
    pub dest_id: String,
    pub dest_name: String,
    pub next_hop_id: String,
    pub next_hop_name: String,
    pub hops: usize,
    pub path: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capability {
    pub node_id: String,
    pub node_name: String,
    pub kind: String,
    pub name: String,
    pub url: String,
    pub internet: bool,
}

/// Liveness of a locally configured egress proxy (Clash etc).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EgressStatus {
    pub name: String,
    pub kind: String,
    pub url: String,
    pub alive: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub self_node: NodeInfo,
    pub version: String,
    pub listen_port: u16,
    #[serde(default)]
    pub pinned_port: Option<u16>,
    pub ports: Vec<u16>,
    /// Current cluster-key epoch (0 = never rotated).
    #[serde(default)]
    pub cluster_epoch: u64,
    pub uptime_secs: u64,
    pub nodes: Vec<NodeStatus>,
    pub routes: Vec<RouteEntry>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub egress: Vec<EgressStatus>,
    /// Hot-swappable kernel status.
    #[serde(default)]
    pub kernel: KernelStatus,
}

/// Status of the hot-swappable Lua/Luau kernel (for the dashboard).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct KernelStatus {
    pub loaded: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub rev: u64,
    #[serde(default)]
    pub workers: usize,
    #[serde(default)]
    pub source: String,
    /// Module paths in the installed kernel (e.g. `main.lua`, `lib/util.lua`).
    #[serde(default)]
    pub files: Vec<String>,
}
