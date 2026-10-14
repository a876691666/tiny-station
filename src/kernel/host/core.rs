//! `ctx.log`, `ctx.time` and the top-level meta keys.

use super::{bump_budget, CtxDeps};
use crate::kernel::monotonic_ms;
use crate::state::now_ms;
use mlua::{Lua, Table};
use std::time::Duration;

pub(crate) fn attach(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    log(lua, ctx)?;
    time(lua, deps, ctx)?;
    meta(lua, ctx)?;
    Ok(())
}

fn log(lua: &Lua, ctx: &Table) -> mlua::Result<()> {
    let log = lua.create_table()?;
    for (name, level) in [("debug", 0u8), ("info", 1), ("warn", 2), ("error", 3)] {
        log.set(
            name,
            lua.create_function(move |_, m: String| {
                match level {
                    0 => tracing::debug!(target: "kernel", "{}", m),
                    1 => tracing::info!(target: "kernel", "{}", m),
                    2 => tracing::warn!(target: "kernel", "{}", m),
                    _ => tracing::error!(target: "kernel", "{}", m),
                }
                Ok(())
            })?,
        )?;
    }
    ctx.set("log", log)
}

fn time(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let time = lua.create_table()?;
    time.set("now", lua.create_function(|_, ()| Ok(now_ms()))?)?;
    time.set("monotonic", lua.create_function(|_, ()| Ok(monotonic_ms()))?)?;
    {
        let b = deps.budget.clone();
        time.set(
            "sleep",
            lua.create_function(move |_, ms: u64| {
                std::thread::sleep(Duration::from_millis(ms.min(60_000)));
                bump_budget(&b);
                Ok(())
            })?,
        )?;
    }
    ctx.set("time", time)
}

fn meta(lua: &Lua, ctx: &Table) -> mlua::Result<()> {
    ctx.set(
        "supports",
        lua.create_function(|_, name: String| {
            Ok(matches!(
                name.as_str(),
                "log" | "time" | "store" | "crypto" | "node" | "metrics" | "timer" | "queue"
                    | "net" | "mesh"
            ))
        })?,
    )?;
    // Overwritten by the runtime with the real script version once loaded.
    ctx.set("kernel_version", "")
}
