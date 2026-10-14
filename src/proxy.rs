use crate::protocol::TunnelRequest;
use crate::state::AppState;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::Method;
use std::time::{Duration, Instant};

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
            | "expect"
    )
}

fn proxy_scheme(port: u16) -> &'static str {
    if port == 443 || port == 8443 {
        "https"
    } else {
        "http"
    }
}

async fn build_client(state: &AppState, egress: Option<&str>) -> Result<reqwest::Client, String> {
    // `.no_proxy()` disables any ambient HTTP_PROXY/ALL_PROXY env vars so that a
    // "direct" request is truly direct; only an explicitly chosen egress proxy
    // is applied.
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if let Some(name) = egress {
        match state.find_proxy(name).await {
            Some(p) => match reqwest::Proxy::all(&p.url) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(e) => return Err(format!("invalid proxy {}: {}", p.url, e)),
            },
            None => return Err(format!("unknown egress proxy '{}'", name)),
        }
    }
    builder.build().map_err(|e| e.to_string())
}

/// Try to fetch `scheme://host:port/` through an egress proxy to check that the
/// proxy is not only listening but can actually reach the target.
pub async fn probe_via(proxy_url: &str, host: &str, port: u16) -> bool {
    let proxy = match reqwest::Proxy::all(proxy_url) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(proxy)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let url = format!("{}://{}:{}/", proxy_scheme(port), host, port);
    matches!(client.get(url).send().await, Ok(_))
}

/// Send one attempt with a fixed (optional) egress. Errors are returned as
/// strings so the caller can try another egress before giving up.
async fn send_once(
    state: &AppState,
    req: &TunnelRequest,
    egress: Option<&str>,
) -> Result<crate::protocol::TunnelResponse, String> {
    let start = Instant::now();
    let client = build_client(state, egress).await?;

    let method = Method::from_bytes(req.method.as_bytes()).unwrap_or(Method::GET);
    let mut rb = client.request(method, &req.url);
    for (k, v) in &req.headers {
        if is_hop_by_hop(k) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            rb = rb.header(name, val);
        }
    }

    if !req.body_b64.is_empty() {
        if let Ok(body) = STANDARD.decode(&req.body_b64) {
            rb = rb.body(body);
        }
    }

    let resp = rb
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {}", e))?;
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .filter(|(k, _)| !is_hop_by_hop(k.as_str()))
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect();
    let bytes = resp.bytes().await.unwrap_or_default();
    Ok(crate::protocol::TunnelResponse {
        status,
        headers,
        body_b64: STANDARD.encode(bytes),
        error: None,
        served_by: state.self_info.id.clone(),
        path: vec![],
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

/// Execute a tunnel request on this node and return a relayable response.
///
/// When no egress is explicitly requested it first tries a direct connection,
/// then automatically falls back to each live local egress proxy in turn (each
/// attempt is itself a reachability probe of the upstream target).
pub async fn execute(state: &AppState, req: &TunnelRequest) -> crate::protocol::TunnelResponse {
    let mut plan: Vec<Option<String>> = Vec::new();
    match &req.egress {
        Some(name) => plan.push(Some(name.clone())),
        None => {
            plan.push(None);
            for name in state.live_proxy_names().await {
                plan.push(Some(name));
            }
        }
    }

    let mut last_err = String::from("no egress attempt made");
    for egress in &plan {
        match send_once(state, req, egress.as_deref()).await {
            Ok(mut r) => {
                if let Some(name) = egress {
                    r.headers
                        .push(("x-tiny-station-egress".into(), name.clone()));
                }
                return r;
            }
            Err(e) => {
                tracing::debug!(
                    "发送失败,出口={:?},稍后尝试其它出口: {}",
                    egress.as_deref().unwrap_or("直连"),
                    e
                );
                last_err = e;
            }
        }
    }
    error_response(state, 502, last_err)
}

fn error_response(
    state: &AppState,
    status: u16,
    msg: impl Into<String>,
) -> crate::protocol::TunnelResponse {
    let mut r = crate::protocol::TunnelResponse::error(status, msg);
    r.served_by = state.self_info.id.clone();
    r
}
