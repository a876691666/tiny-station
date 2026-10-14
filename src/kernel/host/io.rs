//! `ctx.net` and `ctx.mesh` — outbound HTTP and cross-node requests.
//!
//! These are the only blocking host calls (they run on the async runtime and
//! extend the hook's CPU budget when they return). `ctx.net.fetch` does not leak
//! the shell's proxies unless the script asks for one.

use super::{blocking, CtxDeps};
use base64::{engine::general_purpose::STANDARD, Engine};
use mlua::{Lua, LuaSerdeExt, Table, Value};
use std::collections::BTreeMap;

/// Request body shared by `ctx.net.fetch` and `ctx.mesh.call`.
#[derive(serde::Deserialize)]
struct FetchReq {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    egress: Option<String>,
}

impl FetchReq {
    fn method(&self) -> String {
        self.method
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("GET")
            .to_ascii_uppercase()
    }

    fn headers(&self) -> Vec<(String, String)> {
        self.headers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    fn body(&self) -> Vec<u8> {
        self.body.clone().unwrap_or_default().into_bytes()
    }
}

pub(crate) fn attach(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    net(lua, deps, ctx)?;
    mesh(lua, deps, ctx)
}

fn net(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let d = deps.clone();
    let t = lua.create_table()?;
    t.set(
        "fetch",
        lua.create_function(move |lua, req: Value| {
            let r: FetchReq = lua.from_value(req)?;
            let (url, method, headers, egress, timeout) =
                (r.url.clone(), r.method(), r.headers(), r.egress.clone(), r.timeout_ms);
            let body = r.body();
            let st = d.state.clone();
            let result = blocking(&d, async move {
                crate::proxy::kernel_fetch(
                    &st,
                    &url,
                    &method,
                    &headers,
                    body,
                    timeout,
                    egress.as_deref(),
                )
                .await
            });
            match result {
                Ok((status, hdrs, bytes)) => {
                    let t = lua.create_table()?;
                    t.set("status", status)?;
                    let ht = lua.create_table()?;
                    for (k, v) in hdrs {
                        ht.set(k, v)?;
                    }
                    t.set("headers", ht)?;
                    t.set("body", lua.create_string(&bytes)?)?;
                    Ok((Value::Table(t), Value::Nil))
                }
                Err(e) => Ok((Value::Nil, Value::String(lua.create_string(e)?))),
            }
        })?,
    )?;
    ctx.set("net", t)
}

fn mesh(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    call(lua, deps, &t)?;
    who_can_reach(lua, deps, &t)?;
    ctx.set("mesh", t)
}

fn call(lua: &Lua, deps: &CtxDeps, mesh: &Table) -> mlua::Result<()> {
    let d = deps.clone();
    mesh.set(
        "call",
        lua.create_function(move |lua, (dest, req): (String, Value)| {
            let r: FetchReq = lua.from_value(req)?;
            let body = r.body();
            let tr = crate::protocol::TunnelRequest {
                dest,
                method: r.method(),
                url: r.url.clone(),
                headers: r.headers(),
                body_b64: STANDARD.encode(&body),
                egress: r.egress.clone(),
                hops: vec![],
                timeout_ms: r.timeout_ms,
            };
            let st = d.state.clone();
            let resp = blocking(&d, async move { crate::route::route_request(&st, tr).await });
            let t = lua.create_table()?;
            t.set("status", resp.status)?;
            let ht = lua.create_table()?;
            for (k, v) in resp.headers {
                ht.set(k, v)?;
            }
            t.set("headers", ht)?;
            t.set("body_b64", resp.body_b64)?;
            t.set("served_by", resp.served_by)?;
            let p = lua.create_table()?;
            for (i, id) in resp.path.iter().enumerate() {
                p.set(i + 1, id.clone())?;
            }
            t.set("path", p)?;
            t.set("elapsed_ms", resp.elapsed_ms)?;
            t.set("error", resp.error)?;
            Ok(t)
        })?,
    )
}

fn who_can_reach(lua: &Lua, deps: &CtxDeps, mesh: &Table) -> mlua::Result<()> {
    let d = deps.clone();
    mesh.set(
        "who_can_reach",
        lua.create_function(move |lua, (host, port): (String, u16)| {
            let st = d.state.clone();
            let r = blocking(&d, async move {
                crate::net::discover_owner(&st, &host, port).await
            });
            match r {
                Some((id, egress)) => Ok((Value::String(lua.create_string(id)?), egress)),
                None => Ok((Value::Nil, None)),
            }
        })?,
    )
}
