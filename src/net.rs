use crate::config::Target;
use crate::protocol::{
    base_url, GossipBody, NodeInfo, PeerView, ReversePoll, ReversePollResp, ReverseResult,
    ReverseTask, TunnelRequest, TunnelResponse,
};
use crate::route;
use crate::state::{now_ms, AppState, PeerEntry, REVERSE_POLL_SECS, REVERSE_STALE_MS};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROBE_TIMEOUT_MS: u64 = 800;
const STALE_MS: u64 = 120_000;

type Auth = Option<(String, String)>;

fn apply_auth(rb: reqwest::RequestBuilder, auth: &Auth) -> reqwest::RequestBuilder {
    match auth {
        Some((u, p)) => rb.basic_auth(u, Some(p)),
        None => rb,
    }
}

/// Probe a node, returning the failure reason on error.
pub async fn probe(state: &AppState, ip: &str, port: u16, auth: &Auth) -> Result<NodeInfo, String> {
    let url = format!("{}/api/self", base_url(ip, port));
    let rb = state
        .control
        .get(url)
        .timeout(Duration::from_millis(PROBE_TIMEOUT_MS));
    let resp = apply_auth(rb, auth)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "连接超时(端口未开放,或被防火墙/安全组拦截)".to_string()
            } else if e.is_connect() {
                "无法建立连接(端口未开放,或被防火墙/安全组拦截)".to_string()
            } else {
                format!("连接失败: {}", e)
            }
        })?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("认证失败(账号或密码不对)".to_string());
    }
    if !status.is_success() {
        return Err(format!("对方返回 HTTP {}", status.as_u16()));
    }
    resp.json::<NodeInfo>()
        .await
        .map_err(|e| format!("响应解析失败: {}", e))
}

/// Query a candidate node's identity.
pub async fn fetch_self(state: &AppState, ip: &str, port: u16, auth: &Auth) -> Option<NodeInfo> {
    probe(state, ip, port, auth).await.ok()
}

/// Probe the configured ports on an IP to find a sibling tiny-station,
/// ignoring ourselves.
pub async fn scan_ports(
    state: &AppState,
    ip: &str,
    ports: &[u16],
    auth: &Auth,
) -> Option<(u16, NodeInfo, u64)> {
    for port in ports {
        let start = Instant::now();
        if let Some(info) = fetch_self(state, ip, *port, auth).await {
            if info.id == state.self_info.id {
                continue;
            }
            let latency = start.elapsed().as_millis() as u64;
            return Some((*port, info, latency));
        }
    }
    None
}

async fn upsert_direct(state: &AppState, info: NodeInfo, latency_ms: u64, auth: Auth, dial: (String, u16)) {
    if info.id == state.self_info.id {
        return;
    }
    let mut peers = state.peers.write().await;
    let id = info.id.clone();
    let entry = peers.entry(id.clone()).or_insert_with(|| PeerEntry {
        info: info.clone(),
        links: vec![],
        proxies: vec![],
        reachable: vec![],
        internet: false,
        auth: None,
        dial: None,
        configured: false,
        directly_reachable: false,
        latency_ms: None,
        last_error: None,
        last_seen_ms: 0,
    });
    entry.info = info;
    entry.auth = auth;
    entry.dial = Some(dial);
    entry.configured = true;
    entry.directly_reachable = true;
    entry.latency_ms = Some(latency_ms);
    entry.last_error = None;
    entry.last_seen_ms = now_ms();
    tracing::debug!("direct link confirmed to {}", id);
}

async fn mark_unreachable(state: &AppState, id: &str, reason: String) {
    // A failed direct probe is expected for a NAT'd peer that only exposes a
    // reverse channel; never demote it while that channel is still alive.
    if state.reverse_fresh(id).await {
        return;
    }
    let mut peers = state.peers.write().await;
    if let Some(e) = peers.get_mut(id) {
        e.directly_reachable = false;
        e.latency_ms = None;
        e.last_error = Some(reason);
    }
}

async fn reachable_ip(state: &AppState, ip: &str) -> bool {
    state
        .peers
        .read()
        .await
        .values()
        .any(|e| e.directly_reachable && e.info.ip == ip)
}

/// A cheap TCP connect probe used to verify that a declared `reachable`
/// service is actually up from this node right now.
async fn probe_tcp(host: &str, port: u16) -> bool {
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    {
        Ok(Ok(_)) => true,
        _ => false,
    }
}

/// Can this node reach `host:port` — either directly or through one of its live
/// egress proxies? Returns `Some(None)` for a direct hit, `Some(Some(name))`
/// when only reachable via egress proxy `name`, or `None` when unreachable.
pub async fn probe_target(state: &AppState, host: &str, port: u16) -> Option<Option<String>> {
    if probe_tcp(host, port).await {
        return Some(None);
    }
    for p in state.live_proxies().await {
        if crate::proxy::probe_via(&p.url, host, port).await {
            return Some(Some(p.name));
        }
    }
    None
}

/// Probe the endpoint of every configured egress proxy and auto-detect any
/// local proxy (Clash / v2ray) listening on a common port.
async fn proxies_round(state: &AppState) {
    let cfg = state.config().await;
    let mut live = Vec::new();
    for p in &cfg.proxies {
        match crate::config::proxy_addr(&p.url) {
            Some((host, port)) if probe_tcp(&host, port).await => {
                live.push(p.name.clone());
            }
            _ => tracing::debug!("出口代理不可达,暂不广播: {}", p.name),
        }
    }
    *state.proxies_live.write().await = live;

    let mut auto = Vec::new();
    if cfg.auto_proxy_scan() {
        for port in crate::config::AUTO_PROXY_PORTS {
            // Skip ports already covered by an explicit proxy config.
            if cfg
                .proxies
                .iter()
                .any(|p| crate::config::proxy_addr(&p.url) == Some(("127.0.0.1".to_string(), port)))
            {
                continue;
            }
            if probe_tcp("127.0.0.1", port).await {
                auto.push(crate::protocol::ProxyInfo {
                    name: format!("local:{}", port),
                    kind: "http".to_string(),
                    url: format!("http://127.0.0.1:{}", port),
                });
            }
        }
    }
    if !auto.is_empty() {
        tracing::debug!("自动发现本机出口代理: {:?}", auto.iter().map(|p| &p.name).collect::<Vec<_>>());
    }
    *state.proxies_auto.write().await = auto;
}

/// Actively probe every `reachable` entry and advertise only the ones we can
/// actually reach. `host:port` uses a direct TCP probe; a bare host/domain is
/// probed on 443 then 80, directly or through a live proxy. Non-probeable
/// patterns (CIDR / wildcard / `*`) are kept as static claims.
async fn services_round(state: &AppState) {
    let cfg = state.config().await;
    let mut live = Vec::new();
    for pat in &cfg.reachable {
        if let Some((host, port)) = route::pattern_probe_addr(pat) {
            if probe_target(state, &host, port).await.is_some() {
                live.push(pat.clone());
            } else {
                tracing::debug!("服务不可达,暂不广播: {}", pat);
            }
        } else if let Some(host) = route::pattern_probe_host(pat) {
            if probe_target(state, &host, 443).await.is_some()
                || probe_target(state, &host, 80).await.is_some()
            {
                live.push(pat.clone());
            } else {
                tracing::debug!("域名不可达,暂不广播: {}", pat);
            }
        } else {
            live.push(pat.clone());
        }
    }
    *state.reachable_live.write().await = live;
}

async fn discover_targets(state: &AppState) {
    let cfg = state.config().await;
    let ports = cfg.ports();
    for Target {
        address,
        port,
        auth,
    } in cfg.targets()
    {
        if address == state.self_info.ip && port == Some(state.listen_port) {
            continue;
        }
        // Peers may be given a friendly name in config; remember it.
        let friendly = cfg
            .peers
            .iter()
            .find(|p| crate::config::parse_seed(&p.address).0 == address)
            .and_then(|p| p.name.clone())
            .unwrap_or_else(|| address.clone());

        let (reachable, error, latency) = discover_one(state, &address, port, &auth, &ports).await;

        let mut targets = state.targets.write().await;
        targets.insert(
            address.clone(),
            crate::state::TargetStatus {
                name: friendly,
                address: address.clone(),
                ip: address.clone(),
                port,
                reachable,
                error,
                latency_ms: latency,
            },
        );
    }
}

/// Try to reach one configured target. Returns (reachable, error, latency).
async fn discover_one(
    state: &AppState,
    address: &str,
    port: Option<u16>,
    auth: &Auth,
    ports: &[u16],
) -> (bool, Option<String>, Option<u64>) {
    match port {
        Some(p) => {
            let start = Instant::now();
            match probe(state, address, p, auth).await {
                Ok(info) if info.id == state.self_info.id => (false, Some("指向本机自身的端口".into()), None),
                Ok(info) => {
                    let lat = start.elapsed().as_millis() as u64;
                    upsert_direct(state, info, lat, auth.clone(), (address.to_string(), p)).await;
                    (true, None, Some(lat))
                }
                Err(e) => (false, Some(e), None),
            }
        }
        None => {
            if reachable_ip(state, address).await {
                return (true, None, None);
            }
            let mut last_err = Some("未在扫描端口上发现服务(可能被防火墙/安全组拦截)".to_string());
            for p in ports {
                let start = Instant::now();
                match probe(state, address, *p, auth).await {
                    Ok(info) if info.id == state.self_info.id => continue,
                    Ok(info) => {
                        let lat = start.elapsed().as_millis() as u64;
                        upsert_direct(state, info, lat, auth.clone(), (address.to_string(), *p)).await;
                        return (true, None, Some(lat));
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            (false, last_err, None)
        }
    }
}

async fn health_round(state: &AppState) {
    let snapshot: Vec<(String, NodeInfo, Auth, Option<(String, u16)>)> = state
        .peers
        .read()
        .await
        .iter()
        .filter(|(_, v)| v.configured)
        .map(|(k, v)| {
            (
                k.clone(),
                v.info.clone(),
                v.auth.clone(),
                v.dial.clone(),
            )
        })
        .collect();
    let ports = state.config().await.ports();

    for (id, info, auth, dial) in snapshot {
        // Always dial the address that actually worked (config address), never
        // the peer's self-advertised address which may be a private/NAT IP.
        let (pip, pport) = dial.clone().unwrap_or((info.ip.clone(), info.port));
        let start = Instant::now();
        match probe(state, &pip, pport, &auth).await {
            Ok(fresh) => {
                let latency = start.elapsed().as_millis() as u64;
                {
                    let mut peers = state.peers.write().await;
                    if let Some(e) = peers.get_mut(&id) {
                        e.info = fresh.clone();
                        e.directly_reachable = true;
                        e.latency_ms = Some(latency);
                        e.last_error = None;
                        e.last_seen_ms = now_ms();
                    }
                }
                pull_view(state, &base_url(&pip, pport), &auth).await;
            }
            Err(reason) => {
                // The port may have changed; try scanning the configured ports.
                if let Some((found_port, fresh, latency)) =
                    scan_ports(state, &pip, &ports, &auth).await
                {
                    if fresh.id == id || (fresh.id != state.self_info.id && !is_known(state, &fresh.id).await) {
                        {
                            let mut peers = state.peers.write().await;
                            if let Some(e) = peers.get_mut(&id) {
                                e.info = fresh.clone();
                                e.dial = Some((pip.clone(), found_port));
                                e.directly_reachable = true;
                                e.latency_ms = Some(latency);
                                e.last_error = None;
                                e.last_seen_ms = now_ms();
                            }
                        }
                        pull_view(state, &base_url(&pip, found_port), &auth).await;
                    }
                } else {
                    mark_unreachable(state, &id, reason).await;
                }
            }
        }
    }
}

async fn is_known(state: &AppState, id: &str) -> bool {
    state.peers.read().await.contains_key(id)
}

async fn pull_view(state: &AppState, base: &str, auth: &Auth) {
    let url = format!("{}/api/peers", base);
    let rb = state.control.get(url);
    if let Ok(resp) = apply_auth(rb, auth).send().await {
        if let Ok(body) = resp.json::<GossipBody>().await {
            merge_view(state, body.view).await;
        }
    }
}

async fn gossip_round(state: &AppState) {
    let view = build_view(state).await;
    let targets: Vec<(String, Auth)> = state
        .peers
        .read()
        .await
        .values()
        .filter(|e| e.directly_reachable && e.dial.is_some())
        .map(|e| (e.addr_url(), e.auth.clone()))
        .collect();
    for (base, auth) in targets {
        let url = format!("{}/api/gossip", base);
        let rb = state
            .control
            .post(&url)
            .json(&GossipBody { view: view.clone() })
            .timeout(Duration::from_secs(4));
        if let Ok(resp) = apply_auth(rb, &auth).send().await {
            if let Ok(body) = resp.json::<GossipBody>().await {
                merge_view(state, body.view).await;
            }
        }
    }
}

async fn prune(state: &AppState) {
    let now = now_ms();
    let mut peers = state.peers.write().await;
    peers.retain(|_, e| e.directly_reachable || now.saturating_sub(e.last_seen_ms) < STALE_MS * 5);
}

pub async fn build_view(state: &AppState) -> Vec<PeerView> {
    let cfg = state.config().await;
    let peers = state.peers.read().await;
    let links: Vec<String> = peers
        .iter()
        .filter(|(_, e)| e.directly_reachable)
        .map(|(k, _)| k.clone())
        .collect();
    let live_proxies = state.live_proxies().await;
    let mut out = vec![PeerView {
        info: state.self_info.clone(),
        links,
        proxies: live_proxies.clone(),
        reachable: state.live_reachable().await,
        internet: cfg.internet || !live_proxies.is_empty(),
        latency_ms: None,
        last_seen_ms: now_ms(),
    }];
    for e in peers.values() {
        out.push(PeerView {
            info: e.info.clone(),
            links: e.links.clone(),
            proxies: e.proxies.clone(),
            reachable: e.reachable.clone(),
            internet: e.internet,
            latency_ms: e.latency_ms,
            last_seen_ms: e.last_seen_ms,
        });
    }
    out
}

pub async fn merge_view(state: &AppState, list: Vec<PeerView>) {
    let mut peers = state.peers.write().await;
    for pv in list {
        if pv.info.id == state.self_info.id {
            continue;
        }
        peers
            .entry(pv.info.id.clone())
            .and_modify(|e| {
                e.info = pv.info.clone();
                e.links = pv.links.clone();
                e.proxies = pv.proxies.clone();
                e.reachable = pv.reachable.clone();
                e.internet = pv.internet;
                e.last_seen_ms = now_ms();
            })
            .or_insert_with(|| PeerEntry {
                info: pv.info.clone(),
                links: pv.links.clone(),
                proxies: pv.proxies.clone(),
                reachable: pv.reachable.clone(),
                internet: pv.internet,
                auth: None,
                dial: None,
                configured: false,
                directly_reachable: false,
                latency_ms: None,
                last_error: None,
                last_seen_ms: now_ms(),
            });
    }
}

#[derive(serde::Deserialize)]
struct ReachResp {
    #[serde(default)]
    reachable: bool,
    #[serde(default)]
    egress: Option<String>,
}

/// On-demand discovery: ask directly-connected peers whether they can reach
/// `host:port` (directly or via an egress proxy) and return the first owner.
/// This lets a request for another node's `127.0.0.1:39181` find that node
/// without any manual `reachable` configuration. Results are cached briefly.
pub async fn discover_owner(
    state: &AppState,
    host: &str,
    port: u16,
) -> Option<(String, Option<String>)> {
    let key = format!("{}:{}", host, port);
    let now = now_ms();
    {
        let cache = state.reach_cache.read().await;
        if let Some((node, egress, ts)) = cache.get(&key) {
            if now.saturating_sub(*ts) < 20_000 {
                return node.clone().map(|n| (n, egress.clone()));
            }
        }
    }

    let targets: Vec<(String, String, Auth)> = state
        .peers
        .read()
        .await
        .iter()
        .filter(|(_, e)| e.directly_reachable && e.dial.is_some())
        .map(|(id, e)| (id.clone(), e.addr_url(), e.auth.clone()))
        .collect();

    let client = state.control.clone();
    let mut set = tokio::task::JoinSet::new();
    for (id, base, auth) in targets {
        let client = client.clone();
        let url = format!("{}/api/reach?host={}&port={}", base, host, port);
        set.spawn(async move {
            let rb = client.get(url).timeout(Duration::from_secs(3));
            let resp = apply_auth(rb, &auth).send().await.ok()?;
            let j: ReachResp = resp.json().await.ok()?;
            if j.reachable {
                Some((id, j.egress))
            } else {
                None
            }
        });
    }

    let mut found = None;
    while let Some(res) = set.join_next().await {
        if let Ok(Some(hit)) = res {
            found = Some(hit);
            set.abort_all();
            break;
        }
    }

    {
        let mut cache = state.reach_cache.write().await;
        cache.insert(
            key,
            (
                found.as_ref().map(|f| f.0.clone()),
                found.as_ref().and_then(|f| f.1.clone()),
                now_ms(),
            ),
        );
    }
    found
}

pub async fn graph(state: &AppState) -> HashMap<String, Vec<String>> {
    let peers = state.peers.read().await;
    let mut g: HashMap<String, Vec<String>> = HashMap::new();
    let self_links: Vec<String> = peers
        .iter()
        .filter(|(_, e)| e.directly_reachable)
        .map(|(k, _)| k.clone())
        .collect();
    g.insert(state.self_info.id.clone(), self_links);
    for (id, e) in peers.iter() {
        let known: Vec<String> = e
            .links
            .iter()
            .filter(|l| **l == state.self_info.id || peers.contains_key(*l))
            .cloned()
            .collect();
        g.insert(id.clone(), known);
    }
    g
}

pub async fn find_path(state: &AppState, dest: &str) -> Option<Vec<String>> {
    let g = graph(state).await;
    let start = state.self_info.id.clone();
    if start == dest {
        return Some(vec![start]);
    }
    let mut queue = VecDeque::new();
    let mut prev: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(start.clone());
    queue.push_back(start.clone());

    while let Some(cur) = queue.pop_front() {
        if let Some(neigh) = g.get(&cur) {
            for n in neigh {
                if !seen.insert(n.clone()) {
                    continue;
                }
                prev.insert(n.clone(), cur.clone());
                if *n == *dest {
                    let mut path = vec![dest.to_string()];
                    let mut c = dest.to_string();
                    while let Some(p) = prev.get(&c) {
                        path.push(p.clone());
                        if *p == start {
                            break;
                        }
                        c = p.clone();
                    }
                    path.reverse();
                    return Some(path);
                }
                queue.push_back(n.clone());
            }
        }
    }
    None
}

/// The next directly-reachable hop toward `dest`, with its credential and the
/// address that actually works.
pub async fn next_hop(state: &AppState, dest: &str) -> Option<PeerEntry> {
    let path = find_path(state, dest).await?;
    if path.len() < 2 {
        return None;
    }
    let hop = &path[1];
    state.peers.read().await.get(hop).cloned()
}

// ------------------------- reverse channel (NAT traversal) -----------------

/// Mark a node as reachable through a reverse channel (it polled us). We cannot
/// dial it, but we can hand it work over the open channel.
pub async fn upsert_reverse_peer(state: &AppState, info: NodeInfo) {
    if info.id == state.self_info.id {
        return;
    }
    let mut peers = state.peers.write().await;
    let id = info.id.clone();
    let entry = peers.entry(id).or_insert_with(|| PeerEntry {
        info: info.clone(),
        links: vec![],
        proxies: vec![],
        reachable: vec![],
        internet: false,
        auth: None,
        dial: None,
        configured: false,
        directly_reachable: false,
        latency_ms: None,
        last_error: None,
        last_seen_ms: now_ms(),
    });
    entry.info = info;
    entry.directly_reachable = true;
    entry.last_error = None;
    entry.last_seen_ms = now_ms();
}

/// Deliver a tunnel request to a NAT'd node over its reverse channel and await
/// the response. Returns an error response when no channel exists.
pub async fn reverse_call(state: &AppState, node_id: &str, req: TunnelRequest) -> TunnelResponse {
    // Only ever deliver over a live reverse channel — we must not dial the
    // node's advertised IP as a substitute.
    if !state.reverse_fresh(node_id).await {
        return TunnelResponse::error(503, format!("no reverse channel to {}", node_id));
    }
    let slot = state.reverse.read().await.get(node_id).cloned();
    let Some(slot) = slot else {
        return TunnelResponse::error(503, format!("no reverse channel to {}", node_id));
    };
    let req_id = uuid::Uuid::new_v4().to_string();
    let (otx, orx) = tokio::sync::oneshot::channel();
    state
        .reverse_pending
        .write()
        .await
        .insert(req_id.clone(), otx);
    if slot
        .tx
        .send(ReverseTask {
            req_id: req_id.clone(),
            request: req,
        })
        .is_err()
    {
        state.reverse_pending.write().await.remove(&req_id);
        return TunnelResponse::error(503, format!("reverse channel to {} closed", node_id));
    }
    match tokio::time::timeout(Duration::from_secs(90), orx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => TunnelResponse::error(502, "reverse result dropped"),
        Err(_) => {
            state.reverse_pending.write().await.remove(&req_id);
            TunnelResponse::error(504, format!("reverse call to {} timed out", node_id))
        }
    }
}

/// Long-poll loop run by a leaf toward each configured, directly-reachable peer
/// so the peer (a hub) can send it work even when it is behind NAT.
async fn reverse_poll_loop(state: Arc<AppState>, peer_id: String) {
    let mut carry: Option<ReverseResult> = None;
    loop {
        let endpoint = {
            let peers = state.peers.read().await;
            match peers.get(&peer_id) {
                Some(e) if e.directly_reachable => Some((e.addr_url(), e.auth.clone())),
                _ => None,
            }
        };
        let Some((base, auth)) = endpoint else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        let url = format!("{}/api/reverse/poll", base);
        let poll = ReversePoll {
            node: state.self_info.clone(),
            result: carry.take(),
        };
        let rb = state
            .tunnel
            .post(&url)
            .json(&poll)
            .timeout(Duration::from_secs(REVERSE_POLL_SECS + 15));
        match apply_auth(rb, &auth).send().await {
            Ok(resp) => match resp.json::<ReversePollResp>().await {
                Ok(body) => {
                    if let Some(task) = body.task {
                        let mut tr = task.request;
                        if tr.dest.is_empty() {
                            tr.dest = state.self_info.id.clone();
                        }
                        let response = route::route_request(&state, tr).await;
                        carry = Some(ReverseResult {
                            req_id: task.req_id,
                            response,
                        });
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            },
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

/// Start a reverse poll loop for every configured, directly-reachable peer that
/// doesn't have one yet.
async fn ensure_reverse_loops(state: &Arc<AppState>) {
    let ids: Vec<String> = state
        .peers
        .read()
        .await
        .values()
        .filter(|e| e.configured && e.directly_reachable)
        .map(|e| e.info.id.clone())
        .collect();
    for id in ids {
        if state.reverse_loops.write().await.insert(id.clone()) {
            tracing::debug!("为 {} 启动反向通道轮询", id);
            tokio::spawn(reverse_poll_loop(state.clone(), id));
        }
    }
}

/// Drop reverse channels whose leaf stopped polling, and mark those peers down.
async fn reap_reverse(state: &AppState) {
    let now = now_ms();
    let stale: Vec<String> = {
        let map = state.reverse.read().await;
        map.iter()
            .filter(|(_, n)| {
                now.saturating_sub(n.last_seen_ms.load(std::sync::atomic::Ordering::Relaxed))
                    > REVERSE_STALE_MS
            })
            .map(|(k, _)| k.clone())
            .collect()
    };
    for id in stale {
        state.reverse_drop(&id).await;
        let mut peers = state.peers.write().await;
        if let Some(e) = peers.get_mut(&id) {
            if !e.configured {
                e.directly_reachable = false;
                e.last_error = Some("反向通道已断开".to_string());
            }
        }
    }
}

pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            proxies_round(&state).await;
            services_round(&state).await;
            discover_targets(&state).await;
            health_round(&state).await;
            ensure_reverse_loops(&state).await;
            gossip_round(&state).await;
            prune(&state).await;
            reap_reverse(&state).await;
            let interval = Duration::from_secs(state.config().await.gossip_interval());
            tokio::time::sleep(interval).await;
        }
    });
}
