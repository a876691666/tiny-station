mod api;
mod config;
mod dashboard;
mod net;
mod protocol;
mod proxy;
mod route;
mod state;

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "tiny-station", version, about = "Logical mesh proxy network")]
struct Args {
    /// Path to the TOML config file. Defaults to `tiny-station.toml` next to
    /// the executable; a starter config is generated on first run.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tiny_station=info".into()),
        )
        .init();

    let args = Args::parse();
    let config_path = args.config.clone().unwrap_or_else(config::default_config_path);
    let (mut cfg, created) = if config_path.exists() {
        (config::Config::load(&config_path)?, false)
    } else {
        (config::write_default(&config_path)?, true)
    };
    let id = config::load_or_create_id(&config_path)?;
    let name = cfg.name();
    let ip = cfg.advertised_ip();
    let bind = cfg.bind_ip();
    let ports = cfg.ports();

    if created {
        tracing::info!("已生成默认配置文件:{}", config_path.display());
        tracing::info!(
            "网页管理账号: {} / {}",
            cfg.admin_username.as_deref().unwrap_or("admin"),
            cfg.admin_password.as_deref().unwrap_or("")
        );
        tracing::info!(
            "组网账号(供其它节点连接本节点): {} / {}",
            cfg.api_username.as_deref().unwrap_or("mesh"),
            cfg.api_password.as_deref().unwrap_or("")
        );
    }

    let mut chosen: Option<(tokio::net::TcpListener, u16)> = None;
    let candidates = cfg.bind_candidates();
    for port in &candidates {
        match tokio::net::TcpListener::bind((bind, *port)).await {
            Ok(l) => {
                chosen = Some((l, *port));
                break;
            }
            Err(e) => tracing::debug!("端口 {} 不可用: {}", port, e),
        }
    }
    let (listener, listen_port) = chosen
        .with_context(|| format!("在 {} 上,端口 {:?} 均不可用", bind, candidates))?;

    // Pin the chosen port into the config on first run so it never drifts.
    if cfg.port.is_none() {
        cfg.port = Some(listen_port);
        match config::pin_port_file(&config_path, listen_port) {
            Ok(_) => tracing::info!("已将监听端口 {} 固定写入配置文件", listen_port),
            Err(e) => tracing::warn!("写入固定端口失败: {}", e),
        }
    }

    let self_info = protocol::NodeInfo {
        id,
        name: name.clone(),
        ip: ip.clone(),
        port: listen_port,
        version: config::VERSION.to_string(),
    };

    let state = Arc::new(state::AppState::new(
        cfg,
        config_path.clone(),
        self_info.clone(),
        listen_port,
    ));
    if state.api_auth().await.is_none() {
        tracing::warn!("未设置组网账号密码,组网接口无鉴权");
    }
    if state.admin_auth().await.is_none() {
        tracing::warn!("未设置管理账号密码,管理界面无鉴权");
    }

    tracing::info!(
        "{} 启动中 · id={} · {}:{} · 扫描端口 {:?}",
        name,
        self_info.id,
        ip,
        listen_port,
        ports
    );
    tracing::info!("管理界面: http://{}:{}/", ip, listen_port);
    tracing::info!(
        "代理接口: http://{}:{}/proxy?target=<url>&via=auto",
        ip,
        listen_port
    );
    let targets = state.config().await.targets();
    if targets.is_empty() {
        tracing::warn!("未配置任何组网目标,本节点将独立运行");
    } else {
        tracing::info!(
            "组网目标: {:?}",
            targets.iter().map(|t| &t.address).collect::<Vec<_>>()
        );
    }

    net::spawn(state.clone());

    let app = api::router(state);
    axum::serve(listener, app)
        .await
        .context("HTTP 服务错误")?;

    bail!("服务意外退出")
}
