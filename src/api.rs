use crate::config::Config;
use crate::crypto;
use crate::net;
use crate::protocol::{
    Capability, GossipBody, Hello, HelloAck, KeyRotate, NodeInfo, NodeStatus, Ping, ReachAnswer,
    ReachQuery, ReversePoll, ReversePollResp, RouteEntry, StatusResponse, TunnelRequest,
    TunnelResponse,
};
use crate::route;
use crate::replay::ReplayCache;
use crate::kernel::bundle::{KernelBundle, KernelMeta};
use crate::kernel::{KernelHttpReq, KernelHttpResp};
use crate::state::{now_ms, AppState, Session, REVERSE_POLL_SECS, SESSION_TTL_MS};
use crate::wire::{self, Kind};
use std::time::Duration;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/handshake", post(api_handshake))
        .route("/api/peers", post(api_peers))
        .route("/api/gossip", post(api_gossip))
        .route("/api/status", get(api_status))
        .route("/api/config", get(api_get_config).post(api_put_config))
        .route("/api/tunnel", post(api_tunnel))
        .route("/api/request", post(api_request))
        .route("/api/reverse/poll", post(api_reverse_poll))
        .route("/api/reach", post(api_reach))
        .route("/api/keyrotate", post(api_keyrotate))
        .route("/api/rotate", post(api_rotate))
        .route(
            "/api/kernel",
            get(api_kernel_get).post(api_kernel_put).delete(api_kernel_delete),
        )
        .route("/api/kernel/reload", post(api_kernel_reload))
        .route("/api/kernel/rollback", post(api_kernel_rollback))
        .route("/api/kernel/source", get(api_kernel_source))
        .route("/api/kernel/distribute", post(api_kernel_distribute))
        .route("/api/kernel/sync", post(api_kernel_sync))
        .route("/x/*path", any(kernel_endpoint))
        .route("/proxy", any(proxy_handler))
        .fallback(spa_handler)
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

// ------------------------- sealed mesh transport -------------------------

/// Open a sealed, privately-encoded request body of the expected `kind` under
/// an explicit key. When `guard` is set, replayed message ids are rejected.
/// Returns the decoded value or a rejection reason.
fn unseal_key<T: DeserializeOwned>(
    key: &[u8; 32],
    kind: Kind,
    bytes: &[u8],
    guard: Option<&ReplayCache>,
) -> Result<T, String> {
    wire::unpack::<T>(key, kind, bytes, guard).map_err(|e| format!("报文校验失败: {}", e))
}

/// Seal a reply body under an explicit key.
fn sealed_key<T: Serialize>(key: &[u8; 32], kind: Kind, value: &T) -> Response {
    let payload = wire::pack(key, kind, value);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .body(Body::from(payload))
        .unwrap()
}

/// The sender node id carried by every session-sealed request.
fn peer_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-ts-node")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Open a session-sealed request body: look up the sender's pairwise session by
/// its advertised node id, then decrypt. Returns the decoded value and the
/// session (whose key is reused to seal the reply).
async fn open_session<T: DeserializeOwned>(
    s: &AppState,
    headers: &HeaderMap,
    kind: Kind,
    body: &[u8],
) -> Result<(T, Session), String> {
    let id = peer_id(headers).ok_or_else(|| "缺少 x-ts-node 头".to_string())?;
    let sess = s
        .session(&id)
        .await
        .ok_or_else(|| format!("与 {} 无有效会话,需先握手", id))?;
    let v = unseal_key::<T>(&sess.key, kind, body, Some(&s.replay))?;
    Ok((v, sess))
}

// ------------------------- mesh endpoints -------------------------

/// `POST /api/handshake` — enrollment + session negotiation.
///
/// Sealed with the root handshake key (PSK), so only holders of the shared
/// `mesh_key` can complete it. Also reconciles the cluster key so late joiners
/// (and nodes behind on a rotation) sync the current one.
async fn api_handshake(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let hello = match unseal_key::<Hello>(&s.handshake_key, Kind::Handshake, &body, Some(&s.replay)) {
        Ok(h) => h,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    // A peer may carry a newer cluster key; the `(epoch, key)` total order below
    // decides the winner so concurrent rotations converge.
    let (eph, eph_pub) = s.ephemeral().await;
    let (epoch, cluster) = {
        let ks = s.keys.read().await;
        (ks.epoch(), ks.current_key())
    };
    // Converge on the newer cluster key under the `(epoch, key)` total order.
    let (win_epoch, win_key) = crate::keys::newer((epoch, cluster), (hello.epoch, hello.cluster));
    if (win_epoch, win_key) != (epoch, cluster) {
        s.accept_rotation(win_epoch, win_key).await;
    }
    let key = crypto::derive_session(
        &eph,
        s.identity.secret(),
        hello.eph_pub,
        hello.id_pub,
        &win_key,
    );
    s.put_session(
        hello.node.id.clone(),
        Session {
            key,
            peer_epoch: hello.epoch,
            expires_ms: now_ms() + SESSION_TTL_MS,
        },
    )
    .await;
    let ack = HelloAck {
        node: s.self_info.clone(),
        id_pub: s.identity.pubkey(),
        eph_pub,
        epoch: win_epoch,
        cluster: win_key,
    };
    sealed_key(&s.handshake_key, Kind::Handshake, &ack)
}

async fn api_peers(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (_ping, sess) = match open_session::<Ping>(&s, &headers, Kind::Peers, &body).await {
        Ok(v) => v,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    sealed_key(
        &sess.key,
        Kind::Peers,
        &GossipBody {
            view: net::build_view(&s).await,
        },
    )
}

async fn api_gossip(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (body, sess) = match open_session::<GossipBody>(&s, &headers, Kind::Gossip, &body).await {
        Ok(v) => v,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    net::merge_view(&s, body.view).await;
    sealed_key(
        &sess.key,
        Kind::Gossip,
        &GossipBody {
            view: net::build_view(&s).await,
        },
    )
}

async fn api_tunnel(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (req, sess) = match open_session::<TunnelRequest>(&s, &headers, Kind::Tunnel, &body).await {
        Ok(v) => v,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    sealed_key(&sess.key, Kind::Tunnel, &route::route_request(&s, req).await)
}

/// `POST /api/reverse/poll` — a NAT'd node's long-poll to its hub.
///
/// The poll both **returns the result of the previous task** and **waits for the
/// next one**, so a hub can reach a leaf it cannot dial. Sealed with the
/// pairwise session key established during the leaf's handshake.
async fn api_reverse_poll(
    State(s): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (body, sess) =
        match open_session::<ReversePoll>(&s, &headers, Kind::ReversePoll, &body).await {
            Ok(v) => v,
            Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
        };
    let node = body.node;
    if node.id == s.self_info.id {
        return sealed_key(&sess.key, Kind::ReversePoll, &ReversePollResp::default());
    }
    // 1) Deliver the previous task's result to whoever is waiting.
    if let Some(res) = body.result {
        if let Some(tx) = s.reverse_pending.write().await.remove(&res.req_id) {
            let _ = tx.send(res.response);
        }
    }
    // 2) Register/liveness: this leaf is now reachable through the reverse link.
    net::upsert_reverse_peer(&s, node.clone()).await;
    let slot = s.reverse_slot(&node.id).await;
    s.reverse_touch(&node.id).await;

    // 3) Hand out the next task, waiting up to REVERSE_POLL_SECS for one.
    let mut rx = slot.rx.lock().await;
    match tokio::time::timeout(Duration::from_secs(REVERSE_POLL_SECS), rx.recv()).await {
        Ok(Some(task)) => {
            sealed_key(&sess.key, Kind::ReversePoll, &ReversePollResp { task: Some(task) })
        }
        _ => sealed_key(&sess.key, Kind::ReversePoll, &ReversePollResp::default()),
    }
}

/// `POST /api/keyrotate` — receive a cluster-key rotation from a peer. Adopt it
/// when newer and persist; it is re-propagated to the rest of the mesh during
/// the next gossip round.
async fn api_keyrotate(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (rot, sess) =
        match open_session::<KeyRotate>(&s, &headers, Kind::KeyRotate, &body).await {
            Ok(v) => v,
            Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
        };
    if s.accept_rotation(rot.epoch, rot.key).await {
        tracing::info!("已接收并采用新的 cluster key(epoch {})", rot.epoch);
    }
    sealed_key(&sess.key, Kind::KeyRotate, &Ping)
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
/// `POST /api/reach` with a sealed `ReachQuery { host, port }`; replies with a
/// sealed `ReachAnswer { reachable, egress }`.
async fn api_reach(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (q, sess) = match open_session::<ReachQuery>(&s, &headers, Kind::Reach, &body).await {
        Ok(v) => v,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    if q.host.is_empty() || q.port == 0 {
        return sealed_key(
            &sess.key,
            Kind::Reach,
            &ReachAnswer {
                reachable: false,
                egress: None,
            },
        );
    }
    let answer = match net::probe_target(&s, &q.host, q.port).await {
        Some(egress) => ReachAnswer {
            reachable: true,
            egress,
        },
        None => ReachAnswer {
            reachable: false,
            egress: None,
        },
    };
    sealed_key(&sess.key, Kind::Reach, &answer)
}

// ------------------------- admin endpoints -------------------------

/// Request body for `POST /api/rotate`. A new cluster key is derived from
/// `mesh_key` when given, or generated randomly when omitted.
#[derive(Debug, Deserialize, Default)]
pub struct RotateRequest {
    #[serde(default)]
    pub mesh_key: Option<String>,
}

/// `POST /api/rotate` — rotate the mesh's cluster key and broadcast it.
///
/// Any node's admin may initiate. The caller's own key is advanced immediately;
/// the new key is then pushed to every reachable peer and propagates across the
/// whole mesh via the gossip rounds. Late-joining nodes receive the current key
/// automatically during enrollment (handshake).
async fn api_rotate(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<RotateRequest>,
) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let new_epoch = s.cluster_epoch().await + 1;
    let new_key = crypto::cluster_key_from_passphrase(req.mesh_key.as_deref());
    s.accept_rotation(new_epoch, new_key).await;
    tracing::info!("已轮换 cluster key 到 epoch {}", new_epoch);
    let st = s.clone();
    tokio::spawn(async move {
        net::broadcast_rotation(&st, new_epoch, new_key).await;
    });
    Json(serde_json::json!({"ok": true, "epoch": new_epoch})).into_response()
}

// ------------------------- kernel (hot-swappable Lua/Luau) -------------------------

/// `GET /api/kernel` — current kernel status.
async fn api_kernel_get(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let meta = s.kernel_meta().await;
    let metrics = s.kernel_metrics.snapshot();
    let files: Vec<String> = s
        .kernel_module_set()
        .await
        .map(|ms| ms.files.keys().cloned().collect())
        .unwrap_or_default();
    match s.kernel().await {
        Some(k) => {
            let i = k.info();
            Json(serde_json::json!({
                "loaded": true,
                "abi": i.abi,
                "version": i.version,
                "workers": i.workers,
                "source": i.source,
                "rev": meta.floor_rev,
                "metrics": metrics,
                "files": files,
            }))
            .into_response()
        }
        None => Json(serde_json::json!({
            "loaded": false,
            "rev": meta.floor_rev,
            "metrics": metrics,
            "files": files,
        }))
        .into_response(),
    }
}

/// A multi-module upload body for `POST /api/kernel` (JSON).
#[derive(Debug, Deserialize)]
struct MultiFileSpec {
    #[serde(default)]
    entry: Option<String>,
    files: std::collections::BTreeMap<String, String>,
}

/// `POST /api/kernel` — install a kernel (hot-swapped in, no restart).
///
/// Body is auto-detected:
/// * a **kernel package** (`.zip`, magic `PK\x03\x04`, or `content-type` zip) →
///   the whole module set replaces the current one;
/// * `application/json` (or a body starting with `{`) →
///   `{ entry?, files: { "main.lua": "...", "lib/a.lua": "..." } }`;
/// * anything else → treated as the `main.lua` module (keeps other modules).
///
/// `?broadcast=true` also pushes the new version to every node in the mesh.
/// This is the upload path used by the web UI.
async fn api_kernel_put(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let broadcast = q.get("broadcast").map(|v| v != "false").unwrap_or(false);
    let bytes = body.to_vec();
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let set = if ctype.contains("zip") || bytes.starts_with(b"PK\x03\x04") {
        match crate::kernel::package::from_zip(&bytes) {
            Ok(set) => set,
            Err(e) => return plain(StatusCode::BAD_REQUEST, &format!("内核包解析失败: {}", e)),
        }
    } else {
        let text = match String::from_utf8(bytes) {
            Ok(c) => c,
            Err(_) => {
                return plain(
                    StatusCode::BAD_REQUEST,
                    "内核必须是 UTF-8 文本、JSON 或内核包(.zip)",
                )
            }
        };
        if ctype.contains("application/json") || text.trim_start().starts_with('{') {
            match serde_json::from_str::<MultiFileSpec>(&text) {
                Ok(spec) => crate::kernel::ModuleSet {
                    entry: spec.entry.unwrap_or_else(crate::kernel::default_entry),
                    files: spec.files,
                },
                Err(e) => {
                    return plain(StatusCode::BAD_REQUEST, &format!("解析多文件内核失败: {}", e))
                }
            }
        } else {
            // Plain body: update the entry module, keep any existing modules.
            let mut set = s
                .kernel_module_set()
                .await
                .unwrap_or_else(|| crate::kernel::ModuleSet::single(""));
            set.entry = crate::kernel::default_entry();
            set.files.insert(set.entry.clone(), text);
            set
        }
    };
    match install_and_broadcast(&s, &set, broadcast).await {
        Ok((rev, version)) => Json(serde_json::json!({
            "ok": true,
            "rev": rev,
            "version": version,
            "broadcast": broadcast,
            "modules": set.files.len(),
        }))
        .into_response(),
        Err((status, msg)) => plain(status, &msg),
    }
}

/// Install a module set, bump the revision, persist it, and optionally broadcast
/// it to the mesh (unsigned — peers trust the authenticated mesh session unless
/// a `release_pubkey` is configured).
async fn install_and_broadcast(
    s: &Arc<AppState>,
    set: &crate::kernel::ModuleSet,
    broadcast: bool,
) -> Result<(u64, String), (StatusCode, String)> {
    let path = s.kernel_path().await;
    save_prev(s).await;
    let k = crate::kernel::install(s, set, &path.display().to_string())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("内核加载失败: {}", e)))?;
    if let Err(e) = crate::kernel::disk::save(&path, set) {
        tracing::warn!("内核落盘失败: {}", e);
    }
    let rev = s.kernel_meta().await.floor_rev + 1;
    let version = k.info().version.clone();
    let sha256 = set.hash();
    s.set_kernel_meta(KernelMeta {
        floor_rev: rev,
        version: version.clone(),
        sha256: sha256.clone(),
    })
    .await;
    if broadcast {
        let bundle = KernelBundle::new(set, crate::kernel::SUPPORTED_ABI, rev, &version);
        let st = s.clone();
        tokio::spawn(async move {
            net::broadcast_kernel(&st, &bundle).await;
        });
    }
    Ok((rev, version))
}

/// `GET /api/kernel/source` — the current entry module text (for the UI editor).
async fn api_kernel_source(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let set = match s.kernel_module_set().await {
        Some(ms) => ms,
        None => return plain(StatusCode::NOT_FOUND, "尚无内核"),
    };
    match set.files.get(&set.entry) {
        Some(src) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from(src.clone()))
            .unwrap(),
        None => plain(StatusCode::NOT_FOUND, "入口模块不存在"),
    }
}

/// `POST /api/kernel/reload` — reload the kernel from disk.
async fn api_kernel_reload(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let path = s.kernel_path().await;
    let set = match crate::kernel::disk::load(&path) {
        Ok(set) => set,
        Err(e) => {
            return plain(
                StatusCode::NOT_FOUND,
                &format!("读取内核失败 {}: {}", path.display(), e),
            )
        }
    };
    match crate::kernel::install(&s, &set, &path.display().to_string()).await {
        Ok(k) => Json(serde_json::json!({ "ok": true, "version": k.info().version })).into_response(),
        Err(e) => plain(StatusCode::BAD_REQUEST, &format!("内核加载失败: {}", e)),
    }
}

/// `DELETE /api/kernel` — unload the running kernel (revert to built-in behaviour).
async fn api_kernel_delete(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    s.set_kernel(None).await;
    Json(serde_json::json!({ "ok": true, "loaded": false })).into_response()
}

/// Capture the current on-disk kernel set so `POST /api/kernel/rollback` can
/// restore it.
async fn save_prev(s: &Arc<AppState>) {
    if let Some(ms) = s.kernel_module_set().await {
        if let Ok(raw) = serde_json::to_string(&ms) {
            if let Err(e) = crate::atomic::write(&s.kernel_prev_path(), raw.as_bytes()) {
                tracing::warn!("保存上一版内核失败: {}", e);
            }
        }
    }
}

/// Validate + install a bundle, persist it, record the revision, and optionally
/// broadcast it to the mesh. Shared by `distribute` and `sync`.
async fn apply_bundle(
    s: &Arc<AppState>,
    bundle: KernelBundle,
    broadcast: bool,
) -> Result<u64, (StatusCode, String)> {
    let cfg = s.config().await;
    let meta = s.kernel_meta().await;
    if bundle.rev <= meta.floor_rev {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "rev {} 不高于已接受的 {}（拒绝回退重放）",
                bundle.rev, meta.floor_rev
            ),
        ));
    }
    let set = bundle.modules();
    set.validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("内核不合法: {}", e)))?;
    match cfg.release_pubkey() {
        Some(pk) => bundle
            .verify(pk)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("签名校验失败: {}", e)))?,
        None if cfg.kernel_allow_unsigned() => {
            // No pinned key: trust the authenticated mesh session (the push is
            // sealed under the pairwise session key, gated by the mesh_key
            // handshake) — the same trust model as cluster-key rotation.
            if bundle.sha256.to_ascii_lowercase() != set.hash() {
                return Err((StatusCode::BAD_REQUEST, "模块内容哈希与清单不符".to_string()));
            }
        }
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                "未配置 release_pubkey 且 kernel_allow_unsigned=false,拒绝网络内核".to_string(),
            ))
        }
    }

    let kpath = cfg.kernel_path(&s.config_path);
    save_prev(s).await;
    crate::kernel::install(s, &set, &kpath.display().to_string())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("内核加载失败: {}", e)))?;
    if let Err(e) = crate::kernel::disk::save(&kpath, &set) {
        tracing::warn!("内核落盘失败: {}", e);
    }
    s.set_kernel_meta(KernelMeta {
        floor_rev: bundle.rev,
        version: bundle.version.clone(),
        sha256: bundle.sha256.clone(),
    })
    .await;
    tracing::info!("已采用内核 {} (rev {})", bundle.version, bundle.rev);

    if broadcast {
        let st = s.clone();
        let b = bundle.clone();
        tokio::spawn(async move {
            net::broadcast_kernel(&st, &b).await;
        });
    }
    Ok(bundle.rev)
}

/// `POST /api/kernel/distribute` — upload a signed kernel bundle. It is verified
/// against `release_pubkey`, installed locally, and (unless `?broadcast=false`)
/// pushed across the mesh. Use `?broadcast=false` for a canary rollout.
async fn api_kernel_distribute(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Json(bundle): Json<KernelBundle>,
) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let broadcast = q.get("broadcast").map(|v| v != "false").unwrap_or(true);
    match apply_bundle(&s, bundle, broadcast).await {
        Ok(rev) => {
            Json(serde_json::json!({ "ok": true, "rev": rev, "broadcast": broadcast })).into_response()
        }
        Err((status, msg)) => plain(status, &msg),
    }
}

/// `POST /api/kernel/sync` — mesh-internal signed bundle push (session-sealed).
/// Receivers verify, install, and re-propagate. Idempotent: a bundle at or below
/// the accepted revision is answered without action.
async fn api_kernel_sync(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let (bundle, sess) = match crate::api::open_session::<KernelBundle>(
        &s,
        &headers,
        Kind::KernelAnnounce,
        &body,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &e),
    };
    match apply_bundle(&s, bundle, true).await {
        Ok(rev) => tracing::info!("已同步内核 rev {} 并继续传播", rev),
        Err((_, msg)) => tracing::debug!("内核同步忽略: {}", msg),
    }
    sealed_key(&sess.key, Kind::KernelAnnounce, &Ping)
}

/// `POST /api/kernel/rollback` — restore the previous kernel script. The
/// anti-rollback floor is kept, so the reverted revision cannot come back.
async fn api_kernel_rollback(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let raw = match tokio::fs::read_to_string(s.kernel_prev_path()).await {
        Ok(c) => c,
        Err(_) => return plain(StatusCode::NOT_FOUND, "没有可回滚的上一版内核"),
    };
    let set = match serde_json::from_str::<crate::kernel::ModuleSet>(&raw) {
        Ok(set) => set,
        Err(e) => return plain(StatusCode::BAD_REQUEST, &format!("上一版内核损坏: {}", e)),
    };
    let kpath = s.kernel_path().await;
    match crate::kernel::install(&s, &set, &kpath.display().to_string()).await {
        Ok(k) => {
            let _ = crate::kernel::disk::save(&kpath, &set);
            let mut meta = s.kernel_meta().await;
            meta.version = k.info().version.clone();
            meta.sha256 = set.hash();
            s.set_kernel_meta(meta).await;
            Json(serde_json::json!({ "ok": true, "version": k.info().version })).into_response()
        }
        Err(e) => plain(StatusCode::BAD_REQUEST, &format!("回滚失败: {}", e)),
    }
}

/// Build an axum response from a kernel endpoint result.
fn kernel_resp(r: KernelHttpResp) -> Response {
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::OK);
    let mut builder = Response::builder().status(status);
    let mut has_ct = false;
    for (k, v) in &r.headers {
        if k.eq_ignore_ascii_case("content-type") {
            has_ct = true;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }
    if !has_ct {
        builder = builder.header("content-type", "text/plain; charset=utf-8");
    }
    builder
        .body(Body::from(r.body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// `ANY /x/*` — dispatch a request to the kernel's `endpoints` table. 404 when no
/// kernel is loaded or the path is unknown.
async fn kernel_endpoint(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(path): Path<String>,
    method: Method,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    if !mesh_ok(&headers, &s).await {
        return challenge();
    }
    let Some(k) = s.kernel().await else {
        return plain(StatusCode::NOT_FOUND, "未加载内核");
    };
    let full = format!("/x/{}", path);
    let req = KernelHttpReq {
        method: method.as_str().to_string(),
        path: full.clone(),
        query: query.unwrap_or_default(),
        headers: headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.as_str().to_string(), s.to_string())))
            .collect(),
        body: String::from_utf8_lossy(&body).to_string(),
    };
    match k.endpoint(&full, req).await {
        Some(r) => kernel_resp(r),
        None => plain(StatusCode::NOT_FOUND, "未找到该端点"),
    }
}

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

/// Fallback for anything not matched by an API route: serve the embedded SPA
/// (client-side routing). Requires admin auth, which also triggers the
/// browser's Basic-auth prompt on first load.
async fn spa_handler(State(s): State<Shared>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    if !admin_ok(&headers, &s).await {
        return challenge();
    }
    let path = uri.path();
    // Unknown API/x paths should 404, not return the HTML shell.
    if path.starts_with("/api/") || path.starts_with("/x/") {
        return plain(StatusCode::NOT_FOUND, "not found");
    }
    crate::web::serve(path)
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

    let kernel_meta = state.kernel_meta().await;
    let kernel_files: Vec<String> = state
        .kernel_module_set()
        .await
        .map(|ms| ms.files.keys().cloned().collect())
        .unwrap_or_default();
    let kernel = match state.kernel().await {
        Some(k) => crate::protocol::KernelStatus {
            loaded: true,
            version: k.info().version.clone(),
            rev: kernel_meta.floor_rev,
            workers: k.info().workers,
            source: k.info().source.clone(),
            files: kernel_files,
        },
        None => crate::protocol::KernelStatus {
            rev: kernel_meta.floor_rev,
            files: kernel_files,
            ..Default::default()
        },
    };
    StatusResponse {
        self_node: state.self_info.clone(),
        version: crate::config::VERSION.to_string(),
        listen_port: state.listen_port,
        pinned_port: cfg.port,
        ports: cfg.ports(),
        cluster_epoch: state.cluster_epoch().await,
        uptime_secs: state.started.elapsed().as_secs(),
        nodes,
        routes,
        capabilities,
        egress,
        kernel,
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
