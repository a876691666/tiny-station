//! The kernel host API (`ctx`).
//!
//! `ctx` is the *only* way a script touches the world — a small, stable
//! "syscall table" assembled from one module per capability group. Adding a
//! capability means adding a module here and a line in [`build_ctx`], never
//! editing one giant function.
//!
//! | module | namespaces |
//! | --- | --- |
//! | [`core`] | `log`, `time`, meta (`supports`, `kernel_version`) |
//! | [`data`] | `store`, `metrics`, `crypto` |
//! | [`node`] | `node` |
//! | [`schedule`] | `timer`, `queue` |
//! | [`io`] | `net`, `mesh` |

mod core;
mod data;
mod io;
mod node;
mod schedule;

use crate::kernel::timers::TimerRegistry;
use crate::kernel::{monotonic_ms, HOOK_BUDGET_MS, SUPPORTED_ABI};
use crate::state::AppState;
use mlua::{Lua, Table};
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::Handle;

/// Everything a host-API builder needs. Cheap to clone (Arc/Handle/Rc), so
/// `'static` host closures can each own a copy.
#[derive(Clone)]
pub(crate) struct CtxDeps {
    pub(crate) state: Arc<AppState>,
    pub(crate) rt: Handle,
    pub(crate) timers: Rc<RefCell<TimerRegistry>>,
    pub(crate) budget: Rc<Cell<u64>>,
}

/// Run a blocking host call on the async runtime, then give the running hook a
/// fresh CPU-budget window (legitimate I/O time must not count against it).
pub(crate) fn blocking<R>(deps: &CtxDeps, fut: impl Future<Output = R>) -> R {
    let out = deps.rt.block_on(fut);
    bump_budget(&deps.budget);
    out
}

/// Extend the current hook's deadline (no-op when no budget is active).
pub(crate) fn bump_budget(budget: &Rc<Cell<u64>>) {
    if budget.get() != 0 {
        budget.set(monotonic_ms() + HOOK_BUDGET_MS);
    }
}

/// Assemble the full `ctx` table. Each module attaches its own namespaces.
pub(crate) fn build_ctx(lua: &Lua, deps: &CtxDeps) -> mlua::Result<Table> {
    let ctx = lua.create_table()?;
    ctx.set("abi", SUPPORTED_ABI)?;
    core::attach(lua, deps, &ctx)?;
    data::attach(lua, deps, &ctx)?;
    node::attach(lua, deps, &ctx)?;
    schedule::attach(lua, deps, &ctx)?;
    io::attach(lua, deps, &ctx)?;
    Ok(ctx)
}
