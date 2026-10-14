use crate::config::Config;
use crate::dashboard;
use crate::net;
use crate::protocol::{
    Capability, GossipBody, NodeInfo, NodeStatus, RouteEntry, StatusResponse, TunnelRequest,
    TunnelResponse,
};
use crate::route;
use crate::state::{now_ms, AppState};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(dashboard_handler))
        .route("/api/self", get(api_self))
        .route("/api/peers", get(api_peers))
        .route("/api/gossip", post(api_gossip))
        .route("/api/status", get(api_status))
        .route("/api/config", get(api_get_config).post(api_put_config))
        .route("/api/tunnel", post(api_tunnel))
        .route("/api/request", post(api_request))
        .route("/api/reach", get(api_reach))
        .route("/proxy", any(proxy_handler))
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .with_state(state)
}

// ------------------------- auth -------------------------

fn authorized(headers: &HeaderMap, creds: &Option<(String, String)>) -> bool {
    match creds {
        None => true,
        Some((user, pass)) => {
            let Some(v) = headers.get("authorization") else {
                return false;
            };
            let Ok(s) = v.to_str() else {
                return false;
            };
            let Some(b64) = s.strip_prefix("Basic ") else {
                return false;
            };
            let Ok(decoded) = STANDARD.decode(b64.trim()) else {
                return false;
            };
            let Ok(decoded) = String::from_utf8(decoded) else {
                return false;
            };
            decoded == format!("{}:{}", user, pass)
        }
    }
}

fn challenge() -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("www-authenticate", "Basic realm=\"tiny-station\"")
        .body(Body::from("authentication required"))
        .unwrap()
}

async fn mesh_ok(headers: &HeaderMap, s: &AppState) -> bool {
    authorized(headers, &s.api_auth().await)
}

async fn admin_ok(headers: &HeaderMap, s: &AppState) -> bool {
    let admin = s.admin_auth().await;
    if authorized(headers, &admin) {
        return true;
    }
    // A peer node may also use its (possibly identical) api credential.
    let api = s.api_auth().await;
    admin != api && authorized(headers, &api)
}

// ------------------------- mesh endpoints -------------------------

async fn api_self(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    Json(s.self_info.clone()).into_response()
}

async fn api_peers(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    Json(GossipBody {
        view: net::build_view(&s).await,
    })
    .into_response()
}

async fn api_gossip(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<GossipBody>,
) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    net::merge_view(&s, body.view).await;
    Json(GossipBody {
        view: net::build_view(&s).await,
    })
    .into_response()
}

async fn api_tunnel(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<TunnelRequest>,
) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    Json(route::route_request(&s, req).await).into_response()
}

// ------------------------- public data plane -------------------------

/// Standardized request envelope for external services (`POST /api/request`).
///
/// Unlike `/proxy`, every parameter travels in the JSON body — never in the
/// query string — so target URLs, headers and metadata are not length limited.
#[derive(Debug, Deserialize)]
pub struct RequestEnvelope {
    /// Absolute target URL, e.g. `https://api.example.com/v1/items?page=2`.
    pub url: String,
    /// HTTP method. Defaults to `GET`.
    #[serde(default)]
    pub method: Option<String>,
    /// Headers forwarded to the target. An `authorization` entry here goes to
    /// the *target*, independently of the Basic auth used to reach this node.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Request body. A JSON string is sent verbatim (`body_encoding=utf8`) or
    /// base64-decoded (`body_encoding=base64`); any other JSON value is
    /// serialized to JSON.
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    /// `utf8` (default) | `base64`.
    #[serde(default)]
    pub body_encoding: Option<String>,
    /// Where to route the request: `auto` (default) | node name | node id |
    /// egress proxy name | `self`.
    #[serde(default)]
    pub via: Option<String>,
    /// Force a specific egress proxy name on the executing node.
    #[serde(default)]
    pub egress: Option<String>,
    /// Upstream timeout in milliseconds (default 90000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// `json` (default) returns the envelope below; `raw` returns the upstream
    /// status/headers/body unchanged.
    #[serde(default)]
    pub mode: Option<String>,
}

/// Standardized response envelope for `POST /api/request` (unless `mode=raw`).
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope {
    /// `true` when the mesh executed the request (any upstream status);
    /// `false` on a routing/transport error, see `error`.
    pub ok: bool,
    pub status: u16,
    /// Response headers, lower-cased (duplicate values collapse to the last).
    pub headers: BTreeMap<String, String>,
    /// All `Set-Cookie` values, duplicates preserved.
    pub cookies: Vec<String>,
    /// Response body, base64-encoded.
    pub body: String,
    pub body_encoding: &'static str,
    /// Id of the node that actually executed the request.
    pub served_by: String,
    /// Node ids the request traversed, from the entry node to `served_by`.
    pub path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<String>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<TunnelResponse> for ResponseEnvelope {
    fn from(r: TunnelResponse) -> Self {
        let mut headers = BTreeMap::new();
        let mut cookies = Vec::new();
        let mut egress = None;
        for (k, v) in &r.headers {
            let lower = k.to_ascii_lowercase();
            if lower == "set-cookie" {
                cookies.push(v.clone());
            } else if lower == "x-tiny-station-egress" {
                egress = Some(v.clone());
            } else {
                headers.insert(lower, v.clone());
            }
        }
        ResponseEnvelope {
            ok: r.error.is_none(),
            status: r.status,
            headers,
            cookies,
            body: r.body_b64,
            body_encoding: "base64",
            served_by: r.served_by,
            path: r.path,
            egress,
            elapsed_ms: r.elapsed_ms,
            error: r.error,
        }
    }
}

/// Decode the request body according to `body_encoding`.
fn encode_body(
    body: Option<&serde_json::Value>,
    encoding: Option<&str>,
) -> Result<Vec<u8>, String> {
    let Some(v) = body else {
        return Ok(Vec::new());
    };
    match encoding.unwrap_or("utf8") {
        "base64" => {
            let s = v.as_str().ok_or_else(|| {
                "body must be a JSON string when body_encoding=base64".to_string()
            })?;
            STANDARD
                .decode(s.trim())
                .map_err(|e| format!("invalid base64 body: {}", e))
        }
        "utf8" | "text" | "plain" | "" => match v {
            serde_json::Value::String(s) => Ok(s.as_bytes().to_vec()),
            other => serde_json::to_vec(other).map_err(|e| e.to_string()),
        },
        other => Err(format!("unknown body_encoding '{}'", other)),
    }
}

/// `POST /api/request` — the standardized data-plane API.
///
/// Authenticate with the mesh credential via HTTP Basic auth, send a JSON
/// [`RequestEnvelope`] as the body. The node resolves the target through the
/// mesh and returns a JSON [`ResponseEnvelope`] (or the raw response when
/// `mode` is `raw`).
async fn api_request(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<RequestEnvelope>,
) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let body = match encode_body(req.body.as_ref(), req.body_encoding.as_deref()) {
        Ok(b) => b,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    let method = req
        .method
        .as_deref()
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "GET".to_string());
    let (dest, resolved_egress) = route::resolve(&s, &req.url, req.via.as_deref()).await;
    let egress = req.egress.or(resolved_egress);
    let mut target_headers = req.headers;
    // A structured JSON body (object / array / number ...) is serialized to
    // JSON; advertise that unless the caller already set a content-type.
    if matches!(req.body.as_ref(), Some(v) if !v.is_string())
        && !target_headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("content-type"))
    {
        target_headers.insert("content-type".to_string(), "application/json".to_string());
    }
    let tr = TunnelRequest {
        dest,
        method,
        url: req.url,
        headers: target_headers.into_iter().collect(),
        body_b64: STANDARD.encode(&body),
        egress,
        hops: vec![],
        timeout_ms: req.timeout_ms,
    };
    let resp = route::route_request(&s, tr).await;
    if req.mode.as_deref() == Some("raw") {
        tunnel_to_response(resp)
    } else {
        Json(ResponseEnvelope::from(resp)).into_response()
    }
}

/// Liveness probe used by peers to discover who can reach a given target.
/// `GET /api/reach?host=127.0.0.1&port=39181` ->
/// `{"reachable":true,"egress":null}` (direct) or `"egress":"clash"`.
async fn api_reach(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let host = q.get("host").cloned().unwrap_or_default();
    let port = q.get("port").and_then(|p| p.parse::<u16>().ok()).unwrap_or(0);
    if host.is_empty() || port == 0 {
        return Json(serde_json::json!({"reachable": false})).into_response();
    }
    match net::probe_target(&s, &host, port).await {
        Some(egress) => {
            Json(serde_json::json!({"reachable": true, "egress": egress})).into_response()
        }
        None => Json(serde_json::json!({"reachable": false})).into_response(),
    }
}

// ------------------------- admin endpoints -------------------------

async fn api_status(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    Json(build_status(&s).await).into_response()
}

async fn api_get_config(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    Json(s.config().await).into_response()
}

async fn api_put_config(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(cfg): Json<Config>,
) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    if let Err(e) = cfg.save(&s.config_path) {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, &format!("save failed: {}", e));
    }
    s.set_config(cfg).await;
    Json(serde_json::json!({"ok": true, "saved_to": s.config_path})).into_response()
}

async fn dashboard_handler(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let status = build_status(&s).await;
    let cfg = s.config().await;
    Html(dashboard::render(&status, &cfg)).into_response()
}

// ------------------------- data plane -------------------------

async fn proxy_handler(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    method: Method,
    body: Bytes,
) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let target = q
        .get("target")
        .cloned()
        .or_else(|| header_str(&headers, "x-tiny-station-target"));
    let Some(target) = target else {
        return plain(
            StatusCode::BAD_REQUEST,
            "missing ?target=<url> or X-Tiny-Station-Target header",
        );
    };
    let via = q
        .get("via")
        .cloned()
        .or_else(|| header_str(&headers, "x-tiny-station-via"))
        .unwrap_or_else(|| "auto".to_string());
    let (dest, egress) = route::resolve(&s, &target, Some(&via)).await;

    let mut hdrs = Vec::new();
    for (k, v) in headers.iter() {
        if k == "authorization"
            || k == "host"
            || k == "x-tiny-station-target"
            || k == "x-tiny-station-via"
        {
            continue;
        }
        if let Ok(vs) = v.to_str() {
            hdrs.push((k.as_str().to_string(), vs.to_string()));
        }
    }

    let req = TunnelRequest {
        dest,
        method: method.as_str().to_string(),
        url: target,
        headers: hdrs,
        body_b64: STANDARD.encode(&body),
        egress,
        hops: vec![],
        timeout_ms: None,
    };
    tunnel_to_response(route::route_request(&s, req).await)
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn plain(status: StatusCode, msg: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(msg.to_string()))
        .unwrap()
}

fn tunnel_to_response(r: TunnelResponse) -> Response {
    let bytes = STANDARD.decode(&r.body_b64).unwrap_or_default();
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = Response::builder().status(status);
    for (k, v) in &r.headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }
    builder = builder.header("x-tiny-station-served-by", r.served_by.clone());
    builder = builder.header("x-tiny-station-path", r.path.join(" -> "));
    if let Some(err) = &r.error {
        builder = builder.header("x-tiny-station-error", err.clone());
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub async fn build_status(state: &AppState) -> StatusResponse {
    let cfg = state.config().await;
    let self_links: Vec<String>;
    let mut nodes: Vec<NodeStatus>;
    {
        let peers = state.peers.read().await;
        self_links = peers
            .iter()
            .filter(|(_, e)| e.directly_reachable)
            .map(|(k, _)| k.clone())
            .collect();
        nodes = Vec::with_capacity(peers.len() + 1);
        let live = state.live_reachable().await;
        let self_reachable: Vec<String> = cfg
            .reachable
            .iter()
            .map(|pat| {
                if live.contains(pat) {
                    pat.clone()
                } else if route::pattern_probe_addr(pat).is_some() {
                    format!("{} (探测不可达)", pat)
                } else {
                    pat.clone()
                }
            })
            .collect();
        let live_proxies = state.live_proxies().await;
        nodes.push(NodeStatus {
            info: state.self_info.clone(),
            is_self: true,
            directly_reachable: true,
            latency_ms: Some(0),
            last_seen_ms: now_ms(),
            links: self_links.clone(),
            proxies: live_proxies.clone(),
            reachable: self_reachable,
            internet: cfg.internet || !live_proxies.is_empty(),
            last_error: None,
        });
        for e in peers.values() {
            // Show the address we actually dial (the peer may advertise a
            // private/NAT IP that is useless from here).
            let mut info = e.info.clone();
            if e.directly_reachable {
                let (dip, dport) = e.display_addr();
                info.ip = dip;
                info.port = dport;
            }
            nodes.push(NodeStatus {
                info,
                is_self: false,
                directly_reachable: e.directly_reachable,
                latency_ms: e.latency_ms,
                last_seen_ms: e.last_seen_ms,
                links: e.links.clone(),
                proxies: e.proxies.clone(),
                reachable: e.reachable.clone(),
                internet: e.internet,
                last_error: e.last_error.clone(),
            });
        }
    }

    let mut capabilities = Vec::new();
    for n in &nodes {
        for p in &n.proxies {
            capabilities.push(Capability {
                node_id: n.info.id.clone(),
                node_name: n.info.name.clone(),
                kind: p.kind.clone(),
                name: p.name.clone(),
                url: p.url.clone(),
                internet: n.internet,
            });
        }
    }

    // Show configured-but-unreachable targets in red, with the failure reason.
    let peer_ips: HashSet<String> = nodes
        .iter()
        .filter(|n| !n.is_self)
        .map(|n| n.info.ip.clone())
        .collect();
    {
        let targets = state.targets.read().await;
        for t in targets.values() {
            if t.reachable || peer_ips.contains(&t.ip) {
                continue;
            }
            nodes.push(NodeStatus {
                info: NodeInfo {
                    id: format!("target:{}", t.address),
                    name: t.name.clone(),
                    ip: t.ip.clone(),
                    port: t.port.unwrap_or(0),
                    version: "-".to_string(),
                },
                is_self: false,
                directly_reachable: false,
                latency_ms: t.latency_ms,
                last_seen_ms: 0,
                links: vec![],
                proxies: vec![],
                reachable: vec![],
                internet: false,
                last_error: t.error.clone(),
            });
        }
    }

    let mut routes = Vec::new();
    for n in &nodes {
        if n.is_self {
            continue;
        }
        if let Some(path) = net::find_path(state, &n.info.id).await {
            if path.len() >= 2 {
                let next_hop_id = path[1].clone();
                let next_hop_name = nodes
                    .iter()
                    .find(|x| x.info.id == next_hop_id)
                    .map(|x| x.info.name.clone())
                    .unwrap_or_else(|| next_hop_id.clone());
                routes.push(RouteEntry {
                    dest_id: n.info.id.clone(),
                    dest_name: n.info.name.clone(),
                    next_hop_id,
                    next_hop_name,
                    hops: path.len() - 1,
                    path,
                });
            }
        }
    }

    let egress = {
        let live = state.live_proxy_names().await;
        let mut list: Vec<crate::protocol::EgressStatus> = cfg
            .proxies
            .iter()
            .map(|p| crate::protocol::EgressStatus {
                name: p.name.clone(),
                kind: p.kind.clone(),
                url: p.url.clone(),
                alive: live.contains(&p.name),
            })
            .collect();
        for p in state.proxies_auto.read().await.iter() {
            if !list.iter().any(|e| e.name == p.name) {
                list.push(crate::protocol::EgressStatus {
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    url: p.url.clone(),
                    alive: true,
                });
            }
        }
        list
    };

    StatusResponse {
        self_node: state.self_info.clone(),
        version: crate::config::VERSION.to_string(),
        listen_port: state.listen_port,
        pinned_port: cfg.port,
        ports: cfg.ports(),
        uptime_secs: state.started.elapsed().as_secs(),
        nodes,
        routes,
        capabilities,
        egress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn body_utf8_string() {
        let v = json!("hello");
        assert_eq!(encode_body(Some(&v), None).unwrap(), b"hello".to_vec());
    }

    #[test]
    fn body_json_object_is_serialized() {
        let v = json!({"a": 1});
        let out = encode_body(Some(&v), Some("utf8")).unwrap();
        assert_eq!(out, br#"{"a":1}"#.to_vec());
    }

    #[test]
    fn body_base64_decoded() {
        let v = json!("aGVsbG8=");
        assert_eq!(
            encode_body(Some(&v), Some("base64")).unwrap(),
            b"hello".to_vec()
        );
    }

    #[test]
    fn body_base64_requires_string() {
        let v = json!({"a": 1});
        assert!(encode_body(Some(&v), Some("base64")).is_err());
    }

    #[test]
    fn body_absent_is_empty() {
        assert!(encode_body(None, None).unwrap().is_empty());
    }
}
