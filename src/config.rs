use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};

/// Default scan/bind ports. The user only configures an IP address, so the
/// service probes these five (uncommon, high) ports to find itself a free port
/// and to discover sibling services on peer IPs.
pub const DEFAULT_PORTS: [u16; 5] = [45910, 45911, 45912, 45913, 45914];

/// Common local proxy (Clash / v2ray / Surge) ports scanned on 127.0.0.1 to
/// auto-detect an egress proxy during the heartbeat.
pub const AUTO_PROXY_PORTS: [u16; 12] = [
    7890, 7891, 7892, 7893, 7897, 7899, 1080, 1081, 2080, 8889, 10808, 10809,
];

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    /// Logical name of this node (shown in the dashboard).
    #[serde(default)]
    pub name: Option<String>,
    /// IP address advertised to the rest of the mesh. If omitted it is
    /// auto-detected from the first non-loopback interface.
    #[serde(default)]
    pub ip: Option<String>,
    /// Local interface to bind on. Defaults to 0.0.0.0.
    #[serde(default)]
    pub bind: Option<String>,
    /// Override the port scan list. Defaults to `DEFAULT_PORTS`.
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
    /// Fixed listen port. Written back automatically after the first run so the
    /// port never drifts (keeps firewall/security-group rules stable).
    #[serde(default)]
    pub port: Option<u16>,
    /// IPs (optionally `ip:port`) of sibling nodes to scan/join (no auth).
    #[serde(default)]
    pub seeds: Vec<String>,
    /// Authenticated links to other nodes (address + account + password).
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    /// Credential this node requires from inbound mesh traffic.
    #[serde(default)]
    pub api_username: Option<String>,
    #[serde(default)]
    pub api_password: Option<String>,
    /// Shared, non-public mesh key. Every node must set the SAME value. All
    /// node-to-node traffic (heartbeat/control and proxy/data plane) is encoded
    /// with a private binary protocol and encrypted with a key derived from
    /// this passphrase via HKDF-SHA256. Required: the mesh refuses to start
    /// without it.
    #[serde(default)]
    pub mesh_key: Option<String>,
    /// Credential required to open the web management UI / status API.
    /// Falls back to the api credential when unset.
    #[serde(default)]
    pub admin_username: Option<String>,
    #[serde(default)]
    pub admin_password: Option<String>,
    /// Advertise that this node can reach the public internet (its egress can
    /// be used by any other node in the mesh).
    #[serde(default)]
    pub internet: bool,
    /// Egress proxies installed on this machine (clash, v2ray, etc).
    #[serde(default)]
    pub proxies: Vec<ProxyConfig>,
    /// Hosts / CIDRs / wildcards this node can reach (e.g. a LAN web service).
    #[serde(default)]
    pub reachable: Vec<String>,
    /// Auto-detect a local egress proxy (Clash / v2ray) by scanning common
    /// localhost ports during each heartbeat. Defaults to enabled.
    #[serde(default)]
    pub auto_proxy_scan: Option<bool>,
    /// Seconds between discovery/health/gossip rounds.
    #[serde(default)]
    pub gossip_interval_secs: Option<u64>,
    /// Optional path to a Luau kernel script (relative paths are resolved
    /// against the config directory). Defaults to a sibling file
    /// `.<config-stem>.kernel.lua`; loaded at startup when it exists.
    #[serde(default)]
    pub kernel_file: Option<String>,
    /// Number of independent Lua VMs (worker threads) for the kernel. Defaults
    /// to 1.
    #[serde(default)]
    pub kernel_workers: Option<usize>,
    /// Ed25519 public key (hex) that signs distributed kernel bundles. When set,
    /// a signed bundle whose signer matches is the ONLY thing a node will accept
    /// from the mesh. Keep the matching private key off every node (CI only).
    #[serde(default)]
    pub release_pubkey: Option<String>,
    /// Allow unsigned kernel bundles pushed by mesh peers. Defaults true: mesh
    /// traffic is already authenticated by the `mesh_key` handshake + session
    /// keys (same trust model as cluster-key rotation), so a signature is only
    /// *optional* hardening via `release_pubkey`. Set false to require signing.
    #[serde(default)]
    pub kernel_allow_unsigned: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PeerConfig {
    /// `ip` or `ip:port`. When the port is omitted the scan ports are probed.
    pub address: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyConfig {
    pub name: String,
    /// "http" | "socks5"
    #[serde(default = "default_proxy_kind")]
    pub kind: String,
    /// e.g. http://127.0.0.1:7890 or socks5://127.0.0.1:7891
    pub url: String,
}

fn default_proxy_kind() -> String {
    "http".to_string()
}

#[derive(Debug, Clone)]
pub struct Target {
    pub address: String,
    pub port: Option<u16>,
    pub auth: Option<(String, String)>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).context("serializing config")?;
        crate::atomic::write(path, raw.as_bytes())
            .with_context(|| format!("writing config {}", path.display()))?;
        Ok(())
    }

    pub fn ports(&self) -> Vec<u16> {
        match &self.ports {
            Some(p) if !p.is_empty() => p.clone(),
            _ => DEFAULT_PORTS.to_vec(),
        }
    }

    /// Ports to try when binding ourselves. Once a port is pinned, only that
    /// one is used.
    pub fn bind_candidates(&self) -> Vec<u16> {
        match self.port {
            Some(p) => vec![p],
            None => self.ports(),
        }
    }

    pub fn bind_ip(&self) -> IpAddr {
        self.bind
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    }

    pub fn advertised_ip(&self) -> String {
        if let Some(ip) = &self.ip {
            return ip.clone();
        }
        detect_local_ip().unwrap_or_else(|| "127.0.0.1".to_string())
    }

    pub fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("node-{}", &uuid::Uuid::new_v4().to_string()[..8]))
    }

    pub fn gossip_interval(&self) -> u64 {
        self.gossip_interval_secs.unwrap_or(5).max(1)
    }

    pub fn auto_proxy_scan(&self) -> bool {
        self.auto_proxy_scan.unwrap_or(true)
    }

    /// Number of kernel worker VMs (each is a dedicated Lua thread).
    pub fn kernel_workers(&self) -> usize {
        self.kernel_workers.unwrap_or(1).clamp(1, 32)
    }

    /// Effective path of the kernel script.
    pub fn kernel_path(&self, config_path: &Path) -> PathBuf {
        match &self.kernel_file {
            Some(p) if !p.trim().is_empty() => {
                let pb = PathBuf::from(p.trim());
                if pb.is_absolute() {
                    pb
                } else {
                    config_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(pb)
                }
            }
            _ => kernel_file_for(config_path),
        }
    }

    /// The pinned release public key (hex), if configured and non-empty.
    pub fn release_pubkey(&self) -> Option<&str> {
        self.release_pubkey
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn kernel_allow_unsigned(&self) -> bool {
        self.kernel_allow_unsigned.unwrap_or(true)
    }

    pub fn api_auth(&self) -> Option<(String, String)> {
        match (&self.api_username, &self.api_password) {
            (Some(u), Some(p)) if !u.is_empty() => Some((u.clone(), p.clone())),
            _ => None,
        }
    }

    /// The shared mesh key, if set and non-empty.
    pub fn mesh_key(&self) -> Option<&str> {
        self.mesh_key.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn admin_auth(&self) -> Option<(String, String)> {
        match (&self.admin_username, &self.admin_password) {
            (Some(u), Some(p)) if !u.is_empty() => Some((u.clone(), p.clone())),
            _ => self.api_auth(),
        }
    }

    /// All outbound join targets: plain seeds plus authenticated peers.
    pub fn targets(&self) -> Vec<Target> {
        let mut out = Vec::new();
        for s in &self.seeds {
            let (address, port) = parse_seed(s);
            out.push(Target {
                address,
                port,
                auth: None,
            });
        }
        for p in &self.peers {
            let (address, port) = parse_seed(&p.address);
            let auth = match (&p.username, &p.password) {
                (Some(u), Some(pw)) => Some((u.clone(), pw.clone())),
                _ => None,
            };
            out.push(Target {
                address,
                port,
                auth,
            });
        }
        out
    }
}

/// Best-effort local IPv4 detection without sending any packets.
pub fn detect_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    match addr.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
        _ => None,
    }
}

/// Resolve the persistent node id file next to the config.
pub fn id_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "nodeid")
}

/// Auto-generated X25519 identity key file.
pub fn identity_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "identity")
}

/// Persisted, rotatable cluster key file.
pub fn cluster_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "clusterkey")
}

/// Default kernel path (a directory of `.lua` modules, sibling of the config).
/// A path ending in `.lua` selects single-file mode instead.
pub fn kernel_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "kernel")
}

/// Kernel key/value store file (sibling of the config).
pub fn kernel_store_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "kernel.json")
}

/// Kernel anti-rollback / current-revision file (sibling of the config).
pub fn kernel_meta_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "kernel.meta")
}

/// Previous kernel module set, kept for `POST /api/kernel/rollback`.
pub fn kernel_prev_file_for(config_path: &Path) -> PathBuf {
    sibling_file(config_path, "kernel.prev.json")
}

/// `dir/.<config-stem>.<ext>` — a hidden sibling of the config file.
fn sibling_file(config_path: &Path, ext: &str) -> PathBuf {
    let mut p = config_path.to_path_buf();
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "tiny-station".to_string());
    p.set_file_name(format!(".{}.{}", stem, ext));
    p
}

pub fn load_or_create_id(config_path: &Path) -> Result<String> {
    let path = id_file_for(config_path);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let id = raw.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    crate::atomic::write(&path, id.as_bytes())
        .with_context(|| format!("writing id {}", path.display()))?;
    Ok(id)
}

/// Persist the chosen listen port as a root-level `port = N` key, editing the
/// file in place so hand-written comments are preserved. The key is inserted
/// before the first table so TOML keeps it at the root.
pub fn pin_port_file(path: &Path, port: u16) -> Result<()> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = raw.lines().map(|s| s.to_string()).collect();
    let mut replaced = false;
    for line in lines.iter_mut() {
        let t = line.trim_start();
        if !t.starts_with('#') && t.starts_with("port") && t.contains('=') {
            *line = format!("port = {}", port);
            replaced = true;
            break;
        }
    }
    if !replaced {
        let pos = lines
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        lines.insert(pos, format!("port = {}", port));
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    crate::atomic::write(path, out.as_bytes())
        .with_context(|| format!("pinning port in {}", path.display()))?;
    Ok(())
}

/// Parse a seed/peer string into an address candidate. Tolerant of common
/// mistakes: a scheme prefix (http://), a path, a trailing slash, or an
/// explicit `host:port`.
pub fn parse_seed(seed: &str) -> (String, Option<u16>) {
    let mut s = seed.trim();
    for prefix in ["http://", "https://", "socks5://", "socks5h://", "tcp://"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    let s = s.split(['/', '?', '#']).next().unwrap_or(s).trim();

    // Bracketed IPv6, e.g. [::1]:45910
    if let Some(inner) = s.strip_prefix('[') {
        if let Some(end) = inner.find(']') {
            let host = &inner[..end];
            let port = inner[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok());
            return (host.to_string(), port);
        }
    }

    if let Ok(addr) = s.parse::<SocketAddr>() {
        return (addr.ip().to_string(), Some(addr.port()));
    }

    // A single colon means host:port (multiple colons would be a bare IPv6).
    if s.matches(':').count() == 1 {
        if let Some((host, port)) = s.split_once(':') {
            if let Ok(port) = port.parse::<u16>() {
                return (host.to_string(), Some(port));
            }
            return (host.to_string(), None);
        }
    }

    (s.to_string(), None)
}

/// Parse a proxy URL into the `host:port` we can TCP-probe for liveness.
/// `http://127.0.0.1:7897` -> ("127.0.0.1", 7897). Falls back to the scheme's
/// default port when none is given.
pub fn proxy_addr(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => (String::new(), url),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    if let Some(inner) = authority.strip_prefix('[') {
        let host = inner.split(']').next().unwrap_or("").to_string();
        let port = inner.split("]:").nth(1).and_then(|p| p.parse().ok());
        if host.is_empty() {
            return None;
        }
        let port = port.unwrap_or_else(|| default_proxy_port(&scheme));
        return Some((host, port));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return Some((host.to_string(), port));
        }
    }
    Some((authority.to_string(), default_proxy_port(&scheme)))
}

fn default_proxy_port(scheme: &str) -> u16 {
    match scheme {
        "https" => 443,
        "socks5" | "socks5h" => 1080,
        _ => 80,
    }
}

/// Path of the config that lives next to the executable. This keeps a release
/// a single self-contained binary: on first run it writes its config here.
pub fn default_config_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("tiny-station.toml");
        }
    }
    PathBuf::from("tiny-station.toml")
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "tiny-station".to_string())
}

/// A fresh random mesh key (32 random bytes, base64-encoded) for first-run
/// config generation. The operator copies it to every node.
pub fn random_mesh_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}

/// Write a commented starter config and return it. Credentials use the fixed
/// default password `123456` so the mesh works out of the box; change it for
/// any untrusted network.
pub fn write_default(path: &Path) -> Result<Config> {
    let name = hostname();
    let mesh_key = random_mesh_key();
    let template = format!(
        r#"# tiny-station 配置文件(首次运行自动生成)
# 通常只需要填写 ip(不填则自动探测内网地址)。
# 首次运行会从下面 5 个端口里选一个空闲端口,并自动写入固定端口 port;
# 之后每次启动都用这个端口,不再漂移。扫描其它节点时仍会尝试全部 5 个端口。

name = "{name}"
# ip = "203.0.113.5"        # 对外广播的 IP,不填自动探测
# bind = "0.0.0.0"
# ports = [45910, 45911, 45912, 45913, 45914]
# port = 45910              # 固定监听端口(首次运行后自动写入,一般不用手改)

# 全网统一的【根密钥 / 入网密钥】,所有节点必须填完全相同的值。
# 它只用于握手认证(PSK)和新节点入网,不直接加密消息。节点间会话密钥由
# 自动生成的身份密钥 + 临时 ECDH 动态协商;真正用于消息加密的是一个可轮换的
# cluster key(可通过管理接口 POST /api/rotate 设置并自动广播到全网,后加入的
# 节点会在入网握手中自动同步最新值)。
# 首次运行自动生成了一串随机值 —— 请把它原样复制到其它节点的配置里;
# 不设置将拒绝启动。
mesh_key = "{mesh_key}"

# 其它节点访问本节点时需要提供的组网账号密码。
# 注意:这只是额外的 Basic 认证;真正的机密性/完整性由上面的 mesh_key 保证。
api_username = "mesh"
api_password = "123456"

# 网页管理界面的账号密码(不填则复用上面的组网账号)。
admin_username = "admin"
admin_password = "123456"

# 发现 / 心跳 / 广播 的间隔秒数。
gossip_interval_secs = 5

# 只有本节点能直达的主机 / 域名 / 网段 / 通配符(其它节点请求会自动路由过来)。
# 本节点会周期性主动探测本列表,只有真正可达的才会广播出去:
#   - "host:port"  用 TCP 直连探测
#   - "域名"(如 esi.evetech.net)按 443/80 探测,直连不通时会尝试经本机出口代理探测
# 所以可以给所有节点配同样的列表,由各节点自行探测谁真正能连上(例如只有带 clash 的节点能上 ESI)。
# 支持 "host:port", "域名", "host:port/路径前缀", "*.example.com", "192.168.1.0/24", "*"(兜底)。
# reachable = ["esi.evetech.net", "127.0.0.1:39181", "192.168.1.0/24"]

# 本机安装的出口代理(clash / v2ray 等),其它节点用 ?via=名称 借用。
# 另外,心跳会自动扫描本机常见代理端口(7890/7897/1080 等)并自动登记为出口,
# 无需手动配置;如需关闭自动扫描,设置 auto_proxy_scan = false。
# auto_proxy_scan = true
# [[proxies]]
# name = "clash"
# kind = "http"
# url = "http://127.0.0.1:7890"

# 组网目标服务:地址 + 账号 + 密码,可配置多个。
# address 可写 "ip"(自动扫描端口)或 "ip:端口"。
# [[peers]]
# address = "192.168.1.10"
# name = "home"
# username = "mesh"
# password = "对方的组网密码"
"#
    );
    crate::atomic::write(path, template.as_bytes())
        .with_context(|| format!("writing default config {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&template).context("parsing generated default config")?;
    Ok(cfg)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seed_variants() {
        assert_eq!(parse_seed("http://82.157.210.209"), ("82.157.210.209".to_string(), None));
        assert_eq!(parse_seed("82.157.210.209"), ("82.157.210.209".to_string(), None));
        assert_eq!(
            parse_seed("82.157.210.209:45910"),
            ("82.157.210.209".to_string(), Some(45910))
        );
        assert_eq!(
            parse_seed("http://1.2.3.4:8080/api"),
            ("1.2.3.4".to_string(), Some(8080))
        );
        assert_eq!(parse_seed(" [::1]:45910 "), ("::1".to_string(), Some(45910)));
        assert_eq!(parse_seed("example.com/"), ("example.com".to_string(), None));
    }
}
