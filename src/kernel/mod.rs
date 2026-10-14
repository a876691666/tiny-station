//! A hot-swappable **Lua (Luau) kernel**.
//!
//! # Architecture
//!
//! The node's Rust code is the stable *shell* (listeners, sessions, mesh state).
//! The kernel is a Luau script that may override a few behaviours without
//! restarting the process. The shell only ever sees [`KernelHandle`]; everything
//! below it is an implementation detail:
//!
//! ```text
//! shell (route.rs / proxy.rs / api.rs)
//!   └── KernelHandle ─────────── runtime.rs  (worker pool, hook dispatch)
//!         └── worker thread ── host/         (the `ctx` host API)
//!               └── Vm ─────── types.rs      (the shell↔kernel contracts)
//!                              store.rs / metrics.rs / bundle.rs  (side data)
//! ```
//!
//! | file | responsibility |
//! | --- | --- |
//! | [`runtime`] | worker threads, jobs, hook dispatch, CPU budget |
//! | [`types`] | the data contracts crossing the boundary |
//! | [`disk`] | single-file / directory on-disk layout |
//! | [`timers`] | per-VM timer registry |
//! | [`host`] | the `ctx` host API, one module per capability group |
//! | [`store`] / [`metrics`] / [`bundle`] | persistent KV / counters / signed bundles |
//!
//! A kernel is a **module set** ([`ModuleSet`]): an entry module plus any number
//! of `require`-able modules, so real business logic is split across files. On
//! disk it is a directory of `.lua` files (or a single file); over the mesh it
//! travels as one signed [`bundle::KernelBundle`].
//!
//! # Guarantees
//! * Every hook is optional; returning `nil` means "no opinion — use the
//!   built-in behaviour".
//! * Each worker owns its own Lua VM on a dedicated OS thread, so a slow or
//!   blocking hook only stalls that one worker, never the async runtime.
//! * All shared state lives in the shell (`AppState` + [`store::Store`] +
//!   [`metrics::Metrics`]); Lua globals are lost on reload.
//! * A hook error is isolated and logged; the shell falls back to its built-in
//!   behaviour and never crashes.

pub mod bundle;
pub mod disk;
mod host;
pub mod metrics;
mod runtime;
pub mod store;
mod timers;
mod types;

pub use runtime::{install, KernelHandle};
pub use types::{default_entry, KernelHttpReq, KernelHttpResp, ModuleSet, RouteQuery};

use std::time::Instant;

/// The host API / kernel ABI version. A script's `M.abi` must match this.
pub const SUPPORTED_ABI: u32 = 1;

/// Per-VM memory ceiling. A script cannot exhaust node memory.
pub(crate) const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Wall-clock budget for one hook callback's *pure Lua* execution (host calls
/// extend it). Guards against a runaway loop wedging a worker.
pub(crate) const HOOK_BUDGET_MS: u64 = 2000;

/// Milliseconds since process start (monotonic within a run).
pub(crate) fn monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::NodeInfo;
    use crate::state::AppState;
    use std::sync::Arc;

    fn temp_state() -> Arc<AppState> {
        let mut dir = std::env::temp_dir();
        dir.push(format!("ts-kernel-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("tiny-station.toml");
        let info = NodeInfo {
            id: "self".into(),
            name: "n".into(),
            ip: "127.0.0.1".into(),
            port: 1,
            version: "0".into(),
        };
        Arc::new(AppState::new(crate::config::Config::default(), cfg_path, info, 1).unwrap())
    }

    #[tokio::test]
    async fn loads_and_routes() {
        let st = temp_state();
        let code = r#"
            local M = {}
            M.abi = 1
            M.version = "test-1"
            function M.route(q, ctx)
              if q.host == "internal.example.com" then return { via = "self" } end
              return nil
            end
            return M
        "#;
        let k = KernelHandle::load(&ModuleSet::single(code), "test", st, 1).await.unwrap();
        assert_eq!(k.info().version, "test-1");
        let hit = k
            .route(RouteQuery {
                host: "internal.example.com".into(),
                ..Default::default()
            })
            .await;
        assert_eq!(hit.unwrap().via.as_deref(), Some("self"));
        let miss = k
            .route(RouteQuery {
                host: "elsewhere".into(),
                ..Default::default()
            })
            .await;
        assert!(miss.is_none());
    }

    #[tokio::test]
    async fn rejects_bad_scripts() {
        let st = temp_state();
        assert!(KernelHandle::load(&ModuleSet::single("return {}"), "t", st.clone(), 1).await.is_err());
        assert!(KernelHandle::load(&ModuleSet::single("return { abi = 2 }"), "t", st.clone(), 1)
            .await
            .is_err());
        assert!(KernelHandle::load(&ModuleSet::single("this is not lua (("), "t", st.clone(), 1)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn empty_decision_is_ignored() {
        let st = temp_state();
        let code = r#"
            local M = {}
            M.abi = 1
            function M.route(q, ctx) return {} end
            return M
        "#;
        let k = KernelHandle::load(&ModuleSet::single(code), "test", st, 1).await.unwrap();
        assert!(k.route(RouteQuery::default()).await.is_none());
    }

    #[tokio::test]
    async fn endpoint_dispatch() {
        let st = temp_state();
        let code = r#"
            local M = {}
            M.abi = 1
            M.endpoints = {
              ["/x/ping"] = function(req, ctx)
                return { status = 200, body = "pong:" .. req.method }
              end,
            }
            return M
        "#;
        let k = KernelHandle::load(&ModuleSet::single(code), "test", st, 1).await.unwrap();
        let r = k
            .endpoint(
                "/x/ping",
                KernelHttpReq {
                    method: "GET".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "pong:GET");
        assert!(k.endpoint("/x/nope", KernelHttpReq::default()).await.is_none());
    }

    #[tokio::test]
    async fn store_and_metrics_shared() {
        let st = temp_state();
        let code = r#"
            local M = {}
            M.abi = 1
            function M.init(cfg, ctx)
              ctx.store.set("k", "v")
              ctx.metrics.counter("boot")
            end
            return M
        "#;
        let _k = KernelHandle::load(&ModuleSet::single(code), "test", st.clone(), 1).await.unwrap();
        assert_eq!(st.kernel_store.get("k"), Some("v".to_string()));
        assert_eq!(st.kernel_metrics.snapshot().get("boot"), Some(&1.0));
    }

    #[tokio::test]
    async fn crypto_sha256_hashes() {
        let st = temp_state();
        let code = r#"
            local M = {}
            M.abi = 1
            M.endpoints = {
              ["/x/sha"] = function(req, ctx) return { status = 200, body = ctx.crypto.sha256("a") } end,
            }
            return M
        "#;
        let k = KernelHandle::load(&ModuleSet::single(code), "test", st, 1).await.unwrap();
        let r = k
            .endpoint("/x/sha", KernelHttpReq::default())
            .await
            .unwrap();
        assert_eq!(
            r.body,
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
    }

    #[tokio::test]
    async fn requires_modules() {
        let st = temp_state();
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            "main.lua".to_string(),
            r#"
                local util = require("lib.util")
                local M = {}
                M.abi = 1
                M.version = "mod"
                M.endpoints = {
                  ["/x/u"] = function() return { status = 200, body = util.greet("x") } end,
                }
                return M
            "#
            .to_string(),
        );
        files.insert(
            "lib/util.lua".to_string(),
            "local M = {}\nfunction M.greet(n) return \"hi \" .. n end\nreturn M\n".to_string(),
        );
        let set = ModuleSet {
            entry: "main.lua".to_string(),
            files,
        };
        let k = KernelHandle::load(&set, "test", st, 1).await.unwrap();
        let r = k.endpoint("/x/u", KernelHttpReq::default()).await.unwrap();
        assert_eq!(r.body, "hi x");
    }

    #[tokio::test]
    async fn runaway_hook_is_bounded() {
        let st = temp_state();
        let code = r#"
            local M = {}
            M.abi = 1
            function M.route(q, ctx) while true do end end
            return M
        "#;
        let k = KernelHandle::load(&ModuleSet::single(code), "test", st, 1).await.unwrap();
        // The CPU budget aborts the loop; the hook reports "no opinion".
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            k.route(RouteQuery::default()),
        )
        .await
        .expect("route should return once the budget expires");
        assert!(r.is_none());
    }
}
