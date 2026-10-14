//! `ctx.node` — a read-only view of the shell's node/mesh state.

use super::{blocking, CtxDeps};
use mlua::{Lua, LuaSerdeExt, Table};

pub(crate) fn attach(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let node = lua.create_table()?;
    self_info(lua, deps, &node)?;
    peers(lua, deps, &node)?;
    cluster_epoch(lua, deps, &node)?;
    config(lua, deps, &node)?;
    ctx.set("node", node)
}

fn self_info(lua: &Lua, deps: &CtxDeps, node: &Table) -> mlua::Result<()> {
    let st = deps.state.clone();
    node.set(
        "self",
        lua.create_function(move |lua, ()| {
            let t = lua.create_table()?;
            t.set("id", st.self_info.id.clone())?;
            t.set("name", st.self_info.name.clone())?;
            t.set("ip", st.self_info.ip.clone())?;
            t.set("port", st.self_info.port)?;
            t.set("version", st.self_info.version.clone())?;
            Ok(t)
        })?,
    )
}

fn peers(lua: &Lua, deps: &CtxDeps, node: &Table) -> mlua::Result<()> {
    let d = deps.clone();
    node.set(
        "peers",
        lua.create_function(move |lua, ()| {
            let st = d.state.clone();
            let peers: Vec<_> = blocking(&d, async move {
                st.peers.read().await.values().cloned().collect()
            });
            let out = lua.create_table()?;
            for (i, p) in peers.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", p.info.id.clone())?;
                t.set("name", p.info.name.clone())?;
                t.set("ip", p.info.ip.clone())?;
                t.set("port", p.info.port)?;
                t.set("reachable", p.directly_reachable)?;
                t.set("internet", p.internet)?;
                t.set("latency_ms", p.latency_ms)?;
                out.set(i + 1, t)?;
            }
            Ok(out)
        })?,
    )
}

fn cluster_epoch(lua: &Lua, deps: &CtxDeps, node: &Table) -> mlua::Result<()> {
    let d = deps.clone();
    node.set(
        "cluster_epoch",
        lua.create_function(move |_, ()| Ok(blocking(&d, d.state.cluster_epoch())))?,
    )
}

fn config(lua: &Lua, deps: &CtxDeps, node: &Table) -> mlua::Result<()> {
    let d = deps.clone();
    node.set(
        "config",
        lua.create_function(move |lua, ()| {
            let st = d.state.clone();
            let cfg = blocking(&d, async move { st.config().await });
            lua.to_value(&cfg)
        })?,
    )
}
