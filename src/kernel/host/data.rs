//! `ctx.store`, `ctx.metrics` and `ctx.crypto` — the shell-owned primitives.

use super::CtxDeps;
use crate::crypto;
use crate::kernel::metrics::Metrics;
use crate::kernel::store::Store;
use base64::{engine::general_purpose::STANDARD, Engine};
use mlua::{Lua, Table};
use std::sync::Arc;

pub(crate) fn attach(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    store(lua, deps, ctx)?;
    metrics(lua, deps, ctx)?;
    crypto(lua, ctx)
}

fn store(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let st: Arc<Store> = deps.state.kernel_store.clone();
    let t = lua.create_table()?;
    {
        let st = st.clone();
        t.set("get", lua.create_function(move |_, k: String| Ok(st.get(&k)))?)?;
    }
    {
        let st = st.clone();
        t.set(
            "set",
            lua.create_function(move |_, (k, v, ttl): (String, String, Option<u64>)| {
                st.set(&k, &v, ttl);
                Ok(())
            })?,
        )?;
    }
    {
        let st = st.clone();
        t.set("del", lua.create_function(move |_, k: String| Ok(st.del(&k)))?)?;
    }
    {
        let st = st.clone();
        t.set(
            "incr",
            lua.create_function(move |_, (k, d): (String, Option<i64>)| {
                Ok(st.incr(&k, d.unwrap_or(1)))
            })?,
        )?;
    }
    {
        let st = st.clone();
        t.set(
            "keys",
            lua.create_function(move |_, prefix: Option<String>| {
                Ok(st.keys(prefix.as_deref().unwrap_or("")))
            })?,
        )?;
    }
    ctx.set("store", t)
}

fn metrics(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let m: Arc<Metrics> = deps.state.kernel_metrics.clone();
    let t = lua.create_table()?;
    {
        let m = m.clone();
        t.set(
            "counter",
            lua.create_function(move |_, (name, d): (String, Option<f64>)| {
                m.counter(&name, d.unwrap_or(1.0));
                Ok(())
            })?,
        )?;
    }
    {
        let m = m.clone();
        t.set(
            "gauge",
            lua.create_function(move |_, (name, v): (String, f64)| {
                m.gauge(&name, v);
                Ok(())
            })?,
        )?;
    }
    {
        let m = m.clone();
        t.set(
            "histogram",
            lua.create_function(move |_, (name, v): (String, f64)| {
                m.histogram(&name, v);
                Ok(())
            })?,
        )?;
    }
    ctx.set("metrics", t)
}

fn crypto(lua: &Lua, ctx: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "sha256",
        lua.create_function(|_, data: mlua::LuaString| {
            Ok(crypto::sha256_hex(&data.as_bytes()))
        })?,
    )?;
    t.set(
        "hmac_sha256",
        lua.create_function(|_, (key, data): (mlua::LuaString, mlua::LuaString)| {
            Ok(crypto::to_hex(&crypto::hmac_sha256(
                &key.as_bytes(),
                &data.as_bytes(),
            )))
        })?,
    )?;
    t.set(
        "hkdf",
        lua.create_function(
            |lua, (ikm, info, len): (mlua::LuaString, mlua::LuaString, usize)| {
                match crypto::hkdf_expand(&ikm.as_bytes(), b"", &info.as_bytes(), len) {
                    Ok(out) => lua.create_string(&out),
                    Err(e) => Err(mlua::Error::RuntimeError(e)),
                }
            },
        )?,
    )?;
    t.set(
        "rand",
        lua.create_function(|lua, n: usize| lua.create_string(crypto::random_vec(n.min(1 << 20))))?,
    )?;
    t.set(
        "b64_enc",
        lua.create_function(|_, data: mlua::LuaString| Ok(STANDARD.encode(data.as_bytes())))?,
    )?;
    t.set(
        "b64_dec",
        lua.create_function(|lua, s: String| match STANDARD.decode(s.trim()) {
            Ok(b) => lua.create_string(&b),
            Err(e) => Err(mlua::Error::RuntimeError(format!("base64 解码失败: {}", e))),
        })?,
    )?;
    t.set(
        "hex_enc",
        lua.create_function(|_, data: mlua::LuaString| Ok(crypto::to_hex(&data.as_bytes())))?,
    )?;
    t.set(
        "hex_dec",
        lua.create_function(|lua, s: String| match crypto::from_hex(&s) {
            Ok(b) => lua.create_string(&b),
            Err(e) => Err(mlua::Error::RuntimeError(e)),
        })?,
    )?;
    ctx.set("crypto", t)
}
