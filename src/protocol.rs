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
    pub uptime_secs: u64,
    pub nodes: Vec<NodeStatus>,
    pub routes: Vec<RouteEntry>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub egress: Vec<EgressStatus>,
}
