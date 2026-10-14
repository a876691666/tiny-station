//! `ctx.timer` and `ctx.queue` — callbacks scheduled on this worker's VM.
//!
//! Backed by the worker-local [`TimerRegistry`](crate::kernel::timers); the
//! worker's event loop fires them. `queue.spawn(fn)` is simply a one-shot timer
//! with zero delay.

use super::CtxDeps;
use mlua::{Function, Lua, Table};

pub(crate) fn attach(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    timer(lua, deps, ctx)?;
    queue(lua, deps, ctx)
}

fn timer(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    {
        let reg = deps.timers.clone();
        t.set(
            "every",
            lua.create_function(move |_, (ms, f): (u64, Function)| {
                Ok(reg.borrow_mut().add(ms, Some(ms.max(1)), f))
            })?,
        )?;
    }
    {
        let reg = deps.timers.clone();
        t.set(
            "after",
            lua.create_function(move |_, (ms, f): (u64, Function)| {
                Ok(reg.borrow_mut().add(ms, None, f))
            })?,
        )?;
    }
    {
        let reg = deps.timers.clone();
        t.set(
            "cancel",
            lua.create_function(move |_, id: u64| Ok(reg.borrow_mut().cancel(id)))?,
        )?;
    }
    ctx.set("timer", t)
}

fn queue(lua: &Lua, deps: &CtxDeps, ctx: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    {
        let reg = deps.timers.clone();
        t.set(
            "spawn",
            lua.create_function(move |_, f: Function| {
                reg.borrow_mut().add(0, None, f);
                Ok(())
            })?,
        )?;
    }
    ctx.set("queue", t)
}
