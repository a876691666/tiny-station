//! The worker runtime: one Lua VM per OS thread, a message channel to the
//! shell, hook dispatch, and the `KernelHandle` the shell holds.
//!
//! No Lua value ever crosses a thread boundary — jobs and replies are plain
//! Rust data, which is what makes the whole module `Send`/`Sync` safe.

use crate::kernel::host::{self, CtxDeps};
use crate::kernel::timers::TimerRegistry;
use crate::kernel::types::{
    KernelHttpReq, KernelHttpResp, KernelInfo, ModuleSet, RouteDecision, RouteQuery,
};
use crate::kernel::{monotonic_ms, HOOK_BUDGET_MS, MEMORY_LIMIT, SUPPORTED_ABI};
use crate::protocol::{TunnelRequest, TunnelResponse};
use crate::state::AppState;
use mlua::{Lua, LuaSerdeExt, Table, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

/// Sandboxed module loader installed as the script's global `require`. It only
/// resolves names against the injected `__TS_MODULES` table — no filesystem, no
/// arbitrary paths — and caches results (standard Lua module semantics).
const REQUIRE_SHIM: &str = r#"
local loaders = __TS_MODULES
local cache = {}
local function find(name)
  local n = name:gsub("%.", "/")
  return loaders[name] or loaders[n] or loaders[n .. ".lua"] or loaders[n .. "/init.lua"]
end
function require(name)
  if type(name) ~= "string" then error("require: 模块名必须是字符串", 2) end
  local hit = cache[name]
  if hit ~= nil then return hit end
  local loader = find(name)
  if not loader then error("模块未找到: " .. name, 2) end
  local m = loader()
  if m == nil then m = true end
  cache[name] = m
  return m
end
"#;

/// A unit of work sent to a worker VM.
pub(crate) enum Job {
    Route(RouteQuery, oneshot::Sender<Option<RouteDecision>>),
    OnRequest(TunnelRequest, oneshot::Sender<TunnelRequest>),
    OnResponse(TunnelResponse, oneshot::Sender<TunnelResponse>),
    Endpoint(String, KernelHttpReq, oneshot::Sender<Option<KernelHttpResp>>),
}

/// Initialisation result reported by a freshly spawned worker.
type Init = oneshot::Receiver<Result<KernelInfo, String>>;

/// A loaded kernel worker.
struct Worker {
    /// `std::sync::mpsc` so the worker can `recv_timeout` and fire timers; the
    /// `Mutex` only guards the (non-blocking, unbounded) send and makes the
    /// handle `Sync`.
    tx: Mutex<Sender<Job>>,
    _join: JoinHandle<()>,
}

/// Shell-side handle to a loaded kernel. Hold it behind an `Arc`; dropping the
/// last reference signals the worker threads to stop once their in-flight jobs
/// drain.
pub struct KernelHandle {
    workers: Vec<Worker>,
    rr: AtomicUsize,
    info: KernelInfo,
}

impl KernelHandle {
    /// Load `set` into `workers` independent Lua VMs. Fails if any module fails
    /// to compile, the ABI mismatches, or `init` fails.
    pub async fn load(
        set: &ModuleSet,
        source: &str,
        state: Arc<AppState>,
        workers: usize,
    ) -> anyhow::Result<KernelHandle> {
        set.validate().map_err(|e| anyhow::anyhow!(e))?;
        let rt = Handle::current();
        let n = workers.clamp(1, 32);
        let mut spawned = Vec::with_capacity(n);
        let mut inits = Vec::with_capacity(n);
        for _ in 0..n {
            let (tx, join, init) = spawn_worker(set, source, state.clone(), rt.clone());
            spawned.push(Worker {
                tx: Mutex::new(tx),
                _join: join,
            });
            inits.push(init);
        }
        let mut info = KernelInfo {
            abi: SUPPORTED_ABI,
            version: "?".to_string(),
            workers: n,
            source: source.to_string(),
        };
        for init in inits {
            match init.await {
                Ok(Ok(i)) => info = i,
                Ok(Err(e)) => return Err(anyhow::anyhow!(e)),
                Err(_) => return Err(anyhow::anyhow!("内核 worker 初始化中断")),
            }
        }
        info.workers = n;
        Ok(KernelHandle {
            workers: spawned,
            rr: AtomicUsize::new(0),
            info,
        })
    }

    pub fn info(&self) -> &KernelInfo {
        &self.info
    }

    fn pick(&self) -> &Worker {
        let i = self.rr.fetch_add(1, Ordering::Relaxed);
        &self.workers[i % self.workers.len()]
    }

    fn send(&self, job: Job) -> bool {
        self.pick()
            .tx
            .lock()
            .map(|tx| tx.send(job).is_ok())
            .unwrap_or(false)
    }

    /// Ask the kernel where to route a request. `None` = use the built-in router.
    pub async fn route(&self, q: RouteQuery) -> Option<RouteDecision> {
        let (tx, rx) = oneshot::channel();
        if !self.send(Job::Route(q, tx)) {
            return None;
        }
        rx.await.ok().flatten().filter(|d| {
            // An all-empty decision carries no information; treat as no opinion.
            !(d.via.is_none() && d.egress.is_none())
        })
    }

    /// Transform an outbound request on the executing node before it is sent.
    pub async fn on_request(&self, tr: TunnelRequest) -> TunnelRequest {
        let (tx, rx) = oneshot::channel();
        if !self.send(Job::OnRequest(tr.clone(), tx)) {
            return tr;
        }
        rx.await.unwrap_or(tr)
    }

    /// Transform the upstream response before it is relayed back.
    pub async fn on_response(&self, resp: TunnelResponse) -> TunnelResponse {
        let (tx, rx) = oneshot::channel();
        if !self.send(Job::OnResponse(resp.clone(), tx)) {
            return resp;
        }
        rx.await.unwrap_or(resp)
    }

    /// Dispatch a custom `/x/*` request to the kernel's `endpoints` table.
    pub async fn endpoint(&self, path: &str, req: KernelHttpReq) -> Option<KernelHttpResp> {
        let (tx, rx) = oneshot::channel();
        if !self.send(Job::Endpoint(path.to_string(), req, tx)) {
            return None;
        }
        rx.await.ok().flatten()
    }
}

/// Compile a module set and, if valid, atomically install it as the active
/// kernel. Does not touch revision bookkeeping — callers that distribute
/// bundles must update [`AppState::set_kernel_meta`] themselves.
pub async fn install(
    state: &Arc<AppState>,
    set: &ModuleSet,
    source: &str,
) -> anyhow::Result<Arc<KernelHandle>> {
    let workers = state.config().await.kernel_workers();
    let k = Arc::new(KernelHandle::load(set, source, state.clone(), workers).await?);
    state.set_kernel(Some(k.clone())).await;
    Ok(k)
}

// ------------------------- worker thread -------------------------

/// Spawn one kernel worker thread. The returned receiver resolves once the
/// modules have loaded and `init` has run (or with the reason it failed).
fn spawn_worker(
    set: &ModuleSet,
    source: &str,
    state: Arc<AppState>,
    rt: Handle,
) -> (Sender<Job>, JoinHandle<()>, Init) {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let (init_tx, init_rx) = oneshot::channel();
    let set = set.clone();
    let source = source.to_string();
    let join = std::thread::Builder::new()
        .name("kernel".to_string())
        .spawn(move || match Vm::build(&set, &source, state, rt) {
            Ok(vm) => {
                if init_tx.send(Ok(vm.info.clone())).is_err() {
                    return;
                }
                vm.run(rx);
            }
            Err(e) => {
                let _ = init_tx.send(Err(e));
            }
        })
        .expect("spawn kernel worker");
    (tx, join, init_rx)
}

/// A fully initialised Lua VM and its per-VM state.
struct Vm {
    lua: Lua,
    module: Table,
    ctx: Table,
    timers: Rc<RefCell<TimerRegistry>>,
    budget: Rc<Cell<u64>>,
    info: KernelInfo,
}

impl Vm {
    /// Build a fresh VM: compile every module, install `require`, run the entry
    /// module, validate the ABI and run `init`.
    fn build(
        set: &ModuleSet,
        source: &str,
        state: Arc<AppState>,
        rt: Handle,
    ) -> Result<Vm, String> {
        let lua = Lua::new();
        let _ = lua.set_memory_limit(MEMORY_LIMIT);
        sandbox(&lua);

        // A per-callback deadline (ms); 0 means "no limit" (init and timers run
        // unbounded — they are not on the request path).
        let budget: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        {
            let b = budget.clone();
            lua.set_interrupt(move |_| {
                let deadline = b.get();
                if deadline != 0 && monotonic_ms() > deadline {
                    return Err(mlua::Error::RuntimeError(
                        "内核钩子超出时间预算(疑似死循环)".to_string(),
                    ));
                }
                Ok(mlua::VmState::Continue)
            });
        }

        let timers = Rc::new(RefCell::new(TimerRegistry::default()));
        let deps = CtxDeps {
            state: state.clone(),
            rt: rt.clone(),
            timers: timers.clone(),
            budget: budget.clone(),
        };
        let ctx = host::build_ctx(&lua, &deps).map_err(|e| e.to_string())?;

        // Compile every module into a loader, then install `require`.
        let loaders = lua.create_table().map_err(|e| e.to_string())?;
        for (path, src) in &set.files {
            let loader = lua
                .load(src.as_str())
                .set_name(path.as_str())
                .into_function()
                .map_err(|e| format!("模块 {} 编译失败: {}", path, e))?;
            loaders
                .set(path.as_str(), loader)
                .map_err(|e| e.to_string())?;
        }
        lua.globals()
            .set("__TS_MODULES", loaders)
            .map_err(|e| e.to_string())?;
        lua.load(REQUIRE_SHIM)
            .set_name("=require")
            .exec()
            .map_err(|e| format!("安装 require 失败: {}", e))?;

        let entry_src = set
            .files
            .get(&set.entry)
            .ok_or_else(|| format!("入口模块 {} 不存在", set.entry))?;
        let module: Table = lua
            .load(entry_src.as_str())
            .set_name(set.entry.as_str())
            .eval()
            .map_err(|e| format!("加载入口模块 {} 失败: {}", set.entry, e))?;

        let abi = match module.raw_get::<Value>("abi").map_err(|e| e.to_string())? {
            Value::Integer(i) => i,
            Value::Number(n) => n as i64,
            _ => return Err("内核入口缺少 abi 字段".to_string()),
        };
        if abi != SUPPORTED_ABI as i64 {
            return Err(format!(
                "内核 abi={} 与宿主 abi={} 不匹配",
                abi, SUPPORTED_ABI
            ));
        }
        let version = match module.raw_get::<Value>("version") {
            Ok(Value::String(s)) => s.to_str().map(|t| t.to_string()).unwrap_or_default(),
            _ => "0".to_string(),
        };
        ctx.set("kernel_version", version.clone())
            .map_err(|e| e.to_string())?;

        if let Ok(Value::Function(init)) = module.raw_get::<Value>("init") {
            let cfg = rt.block_on(state.config());
            let cfg = lua.to_value(&cfg).map_err(|e| e.to_string())?;
            init.call::<()>((cfg, ctx.clone()))
                .map_err(|e| format!("内核 init 失败: {}", e))?;
        }

        let info = KernelInfo {
            abi: abi as u32,
            version,
            workers: 1,
            source: source.to_string(),
        };
        Ok(Vm {
            lua,
            module,
            ctx,
            timers,
            budget,
            info,
        })
    }

    /// Process jobs and fire timers until the shell drops the handle.
    fn run(self, rx: Receiver<Job>) {
        loop {
            let timeout = match self.timers.borrow().next_deadline() {
                Some(d) => Duration::from_millis(d.saturating_sub(monotonic_ms())),
                None => Duration::from_secs(60),
            };
            match rx.recv_timeout(timeout) {
                Ok(job) => self.run_job(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let due = self.timers.borrow_mut().take_due(monotonic_ms());
            for f in due {
                if let Err(e) = f.call::<()>(self.ctx.clone()) {
                    tracing::warn!("内核 timer 回调出错: {}", e);
                }
            }
        }
    }

    fn run_job(&self, job: Job) {
        self.budget.set(monotonic_ms() + HOOK_BUDGET_MS);
        match job {
            Job::Route(q, reply) => match self.call_route(&q) {
                Ok(v) => {
                    let _ = reply.send(v);
                }
                Err(e) => {
                    tracing::warn!("内核 route 钩子出错,回退内置路由: {}", e);
                    let _ = reply.send(None);
                }
            },
            Job::OnRequest(tr, reply) => match self.call_transform("on_request", &tr) {
                Ok(Some(v)) => {
                    let _ = reply.send(v);
                }
                Ok(None) => {
                    let _ = reply.send(tr);
                }
                Err(e) => {
                    tracing::warn!("内核 on_request 钩子出错,跳过: {}", e);
                    let _ = reply.send(tr);
                }
            },
            Job::OnResponse(resp, reply) => match self.call_transform("on_response", &resp) {
                Ok(Some(v)) => {
                    let _ = reply.send(v);
                }
                Ok(None) => {
                    let _ = reply.send(resp);
                }
                Err(e) => {
                    tracing::warn!("内核 on_response 钩子出错,跳过: {}", e);
                    let _ = reply.send(resp);
                }
            },
            Job::Endpoint(path, req, reply) => match self.call_endpoint(&path, &req) {
                Ok(v) => {
                    let _ = reply.send(v);
                }
                Err(e) => {
                    tracing::warn!("内核端点 {} 出错: {}", path, e);
                    let _ = reply.send(None);
                }
            },
        }
        self.budget.set(0);
    }

    fn call_route(&self, q: &RouteQuery) -> mlua::Result<Option<RouteDecision>> {
        let Value::Function(f) = self.module.raw_get::<Value>("route")? else {
            return Ok(None);
        };
        let qv = self.lua.to_value(q)?;
        let rv: Value = f.call((qv, self.ctx.clone()))?;
        if rv.is_nil() {
            return Ok(None);
        }
        Ok(Some(self.lua.from_value(rv)?))
    }

    fn call_transform<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        name: &str,
        input: &T,
    ) -> mlua::Result<Option<T>> {
        let Value::Function(f) = self.module.raw_get::<Value>(name)? else {
            return Ok(None);
        };
        let iv = self.lua.to_value(input)?;
        let rv: Value = f.call((iv, self.ctx.clone()))?;
        if rv.is_nil() {
            return Ok(None);
        }
        Ok(Some(self.lua.from_value(rv)?))
    }

    fn call_endpoint(&self, path: &str, req: &KernelHttpReq) -> mlua::Result<Option<KernelHttpResp>> {
        let Value::Table(endpoints) = self.module.raw_get::<Value>("endpoints")? else {
            return Ok(None);
        };
        let Value::Function(f) = endpoints.raw_get::<Value>(path)? else {
            return Ok(None);
        };
        let rv: Value = f.call((self.lua.to_value(req)?, self.ctx.clone()))?;
        if rv.is_nil() {
            return Ok(None);
        }
        Ok(Some(self.lua.from_value(rv)?))
    }
}

/// Remove capabilities we do not want scripts to have.
fn sandbox(lua: &Lua) {
    let g = lua.globals();
    for name in [
        "os",
        "io",
        "load",
        "loadstring",
        "dofile",
        "loadfile",
        "require",
        "package",
        "collectgarbage",
        "newproxy",
    ] {
        let _ = g.set(name, Value::Nil);
    }
}
