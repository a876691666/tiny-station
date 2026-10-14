use crate::net;
use crate::protocol::{TunnelRequest, TunnelResponse};
use crate::proxy;
use crate::state::AppState;
use std::net::Ipv4Addr;

const MAX_HOPS: usize = 16;

/// Decide which node should execute a user request and with which egress.
pub async fn resolve(
    state: &AppState,
    target: &str,
    via: Option<&str>,
) -> (String, Option<String>) {
    let cfg = state.config().await;
    let self_id = state.self_info.id.clone();
    if let Some(v) = via.map(str::trim).filter(|v| !v.is_empty() && *v != "auto") {
        match v {
            "self" | "direct" | "local" => return (self_id, None),
            _ => {}
        }
        {
            let peers = state.peers.read().await;
            if v == self_id || v == state.self_info.name {
                return (self_id, None);
            }
            if peers.contains_key(v) {
                return (v.to_string(), None);
            }
            if let Some((id, _)) = peers.iter().find(|(_, e)| e.info.name == v) {
                return (id.clone(), None);
            }
            if cfg.proxies.iter().any(|p| p.name == v) {
                return (self_id, Some(v.to_string()));
            }
            if state.find_proxy(v).await.is_some() {
                return (self_id, Some(v.to_string()));
            }
            if let Some((id, _)) = peers
                .iter()
                .find(|(_, e)| e.proxies.iter().any(|p| p.name == v))
            {
                return (id.clone(), Some(v.to_string()));
            }
        }
    }
    auto(state, &cfg, target).await
}

async fn auto(state: &AppState, cfg: &crate::config::Config, target: &str) -> (String, Option<String>) {
    let self_id = state.self_info.id.clone();
    if let Some(parts) = parts_of(target) {
        let host = parts.host.as_str();
        let hostport = parts.hostport();
        let path = parts.path.as_str();
        {
            let peers = state.peers.read().await;
            for (id, e) in peers.iter() {
                if e.reachable
                    .iter()
                    .any(|p| matches_target(p, host, &hostport, path))
                {
                    return (id.clone(), None);
                }
            }
        }
        if state
            .live_reachable()
            .await
            .iter()
            .any(|p| matches_target(p, host, &hostport, path))
        {
            return (self_id, None);
        }
        // Public target: an internet egress (this node or a peer) may serve it.
        // Loopback/private targets must NEVER go through a generic internet
        // egress — they are only meaningful on a specific machine.
        if !is_local_host(host) {
            let live = state.live_proxy_names().await;
            if cfg.internet || !live.is_empty() {
                return (self_id, live.first().cloned());
            }
            {
                let peers = state.peers.read().await;
                if let Some((id, e)) = peers.iter().find(|(_, e)| e.internet) {
                    let egress = e.proxies.first().map(|p| p.name.clone());
                    return (id.clone(), egress);
                }
            }
        }
        // Last resort: ask peers which of them can actually reach this target
        // (covers "the app runs on another node's 127.0.0.1:39181" with no
        // `reachable` config, and domains that only some nodes can reach).
        let default_port = if target.starts_with("https://") { 443 } else { 80 };
        if let Some((id, egress)) =
            net::discover_owner(state, host, parts.port.unwrap_or(default_port)).await
        {
            return (id, egress);
        }
    }
    (self_id, None)
}

/// Loopback / private / link-local / `localhost`, i.e. addresses that are only
/// meaningful relative to a particular machine.
fn is_local_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local()
        }
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

/// Execute a tunnel request here, or forward it toward the destination node.
pub async fn route_request(state: &AppState, mut tr: TunnelRequest) -> TunnelResponse {
    let self_id = state.self_info.id.clone();
    if tr.dest.is_empty() {
        tr.dest = self_id.clone();
    }
    if tr.dest == self_id {
        let mut r = proxy::execute(state, &tr).await;
        r.path.push(self_id);
        return r;
    }
    if tr.hops.contains(&self_id) || tr.hops.len() > MAX_HOPS {
        return TunnelResponse::error(508, "routing loop detected");
    }
    match net::next_hop(state, &tr.dest).await {
        Some(next) => {
            tr.hops.push(self_id.clone());
            // A peer we cannot dial (a NAT'd node whose link is configured only
            // in the other direction) is reachable *only* through its reverse
            // channel. Never fall back to dialing its advertised IP.
            if next.dial.is_none() {
                let mut r = net::reverse_call(state, &next.info.id, tr).await;
                r.path.insert(0, self_id);
                return r;
            }
            let url = format!("{}/api/tunnel", next.addr_url());
            let rb = state.tunnel.post(&url).json(&tr);
            let rb = match &next.auth {
                Some((u, p)) => rb.basic_auth(u, Some(p)),
                None => rb,
            };
            match rb.send().await {
                Ok(resp) => match resp.json::<TunnelResponse>().await {
                    Ok(mut r) => {
                        r.path.insert(0, self_id);
                        r
                    }
                    Err(e) => TunnelResponse::error(
                        502,
                        format!("下一跳 {} 返回无效响应: {}", next.info.name, e),
                    ),
                },
                Err(e) => TunnelResponse::error(
                    502,
                    format!("无法连接下一跳 {}: {}", next.info.name, e),
                ),
            }
        }
        None => TunnelResponse::error(502, format!("no route to node {}", tr.dest)),
    }
}

/// Host, optional explicit port and path of a target URL.
pub struct UrlParts {
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
}

impl UrlParts {
    pub fn hostport(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.host, p),
            None => self.host.clone(),
        }
    }
}

pub fn parts_of(url: &str) -> Option<UrlParts> {
    let rest = url.splitn(2, "://").nth(1).unwrap_or(url);
    let mut it = rest.splitn(2, ['/', '?', '#']);
    let authority = it.next().unwrap_or("");
    let path = match it.next() {
        Some(p) => format!("/{}", p.split(['?', '#']).next().unwrap_or(p)),
        None => "/".to_string(),
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    let (host, port) = if let Some(s) = authority.strip_prefix('[') {
        let h = s.split(']').next().unwrap_or("").to_string();
        let p = s.split("]:").nth(1).and_then(|x| x.parse::<u16>().ok());
        (h, p)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        match p.parse::<u16>() {
            Ok(port) => (h.to_string(), Some(port)),
            Err(_) => (authority.to_string(), None),
        }
    } else {
        (authority.to_string(), None)
    };
    if host.is_empty() {
        return None;
    }
    Some(UrlParts { host, port, path })
}

/// Match a `reachable` pattern against a request target. Supports:
/// - `host`                 (any port)
/// - `host:port`            (exact port — use this to disambiguate localhost)
/// - `*.example.com`        (wildcard subdomain)
/// - `192.168.1.0/24`       (CIDR)
/// - `host/path` / `host:port/prefix` (path prefix)
/// - `*`                    (catch-all)
pub fn matches_target(pattern: &str, host: &str, hostport: &str, path: &str) -> bool {
    let p = pattern.trim();
    if p.is_empty() {
        return false;
    }
    if p == "*" {
        return true;
    }
    // CIDR (e.g. 192.168.1.0/24): right side is all digits and left is an IP.
    if let Some((net, bits)) = p.split_once('/') {
        if !net.is_empty() && bits.chars().all(|c| c.is_ascii_digit()) && net.parse::<Ipv4Addr>().is_ok() {
            return cidr_match(p, host);
        }
        // host[:port]/path-prefix
        if !host_ok(net, host, hostport) {
            return false;
        }
        return path_ok(&p[net.len()..], path);
    }
    host_ok(p, host, hostport)
}

fn host_ok(p: &str, host: &str, hostport: &str) -> bool {
    if p.is_empty() {
        return true;
    }
    if let Some(sfx) = p.strip_prefix("*.") {
        return host == sfx || host.ends_with(&format!(".{}", sfx));
    }
    if p.contains(':') {
        // an explicit port is required to match
        return p == hostport;
    }
    p == host
}

fn path_ok(prefix: &str, path: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return true;
    }
    path == prefix || path.starts_with(&format!("{}/", prefix))
}

/// If a `reachable` pattern names a concrete host without a port (e.g. a public
/// domain like `esi.evetech.net`), return that host so the node can probe it on
/// the default web ports (443/80) — directly and through any egress proxy.
pub fn pattern_probe_host(pattern: &str) -> Option<String> {
    let p = pattern.trim();
    if p.is_empty()
        || p == "*"
        || p.starts_with("*.")
        || p.contains('/')
        || p.contains(':')
    {
        return None;
    }
    Some(p.to_string())
}

/// If a `reachable` pattern names a concrete `host:port` (optionally with a
/// path suffix), return that host+port so the node can actively TCP-probe it.
/// Returns `None` for patterns that can only be static claims.
pub fn pattern_probe_addr(pattern: &str) -> Option<(String, u16)> {
    let p = pattern.trim();
    if p.is_empty() || p == "*" || p.starts_with("*.") {
        return None;
    }
    let hostpart = match p.split_once('/') {
        Some((net, bits)) if !net.is_empty() && bits.chars().all(|c| c.is_ascii_digit()) && net.parse::<Ipv4Addr>().is_ok() => {
            // CIDR: not probeable
            return None;
        }
        Some((host, _path)) => host,
        None => p,
    };
    let (host, port) = hostpart.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

fn cidr_match(pattern: &str, host: &str) -> bool {
    let Some((net, bits)) = pattern.split_once('/') else {
        return false;
    };
    let (Ok(net), Ok(host)) = (net.parse::<Ipv4Addr>(), host.parse::<Ipv4Addr>()) else {
        return false;
    };
    let Ok(bits) = bits.parse::<u32>() else {
        return false;
    };
    if bits > 32 {
        return false;
    }
    let mask = if bits == 0 {
        0u32
    } else {
        (!0u32) << (32 - bits)
    };
    (u32::from(net) & mask) == (u32::from(host) & mask)
}
