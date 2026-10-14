# tiny-station

把多台机器组成一个**逻辑代理网络(mesh)**。任意服务只需连接其中一台节点,就能把请求
路由到全网:节点会自动挑选「真正能到达目标」的机器去执行请求,必要时借用对方的本机出口
代理(clash / v2ray 等),并支持多跳转发。

典型用途:让只能访问内网的机器、或没有外网出口的机器,通过一个组网节点访问原本到不了的服务。

---

## 特性

- **自动组网**:配置几个目标节点即可,心跳、视图广播、路由计算全自动(支持多跳)。
- **按目标路由**:根据请求的目标 URL 自动决定由哪台节点执行。
- **出口代理**:节点可借用其它节点或本机的 HTTP / SOCKS5 出口;会自动扫描本机常见代理端口。
- **可达声明**:用 `reachable` 声明「只有我能直达」的主机 / 域名 / 网段 / 通配符,周期性探测通过后才广播。
- **标准化 API**:`POST /api/request` 所有参数走 JSON body,不用 query,不受 URL 长度限制。
- **鉴权**:组网接口与管理界面均使用 HTTP Basic;附带网页管理界面。

---

## 快速开始

### 方式一:下载预编译版本

在 Releases 页面下载对应平台的压缩包,解压得到:

```
tiny-station            # 可执行文件(Windows 为 tiny-station.exe)
config.example.toml     # 带注释的示例配置
```

支持 `linux` / `macos` / `windows`,`x86_64` / `aarch64`。

### 方式二:从源码构建

```sh
cargo build --release
# 产物:target/release/tiny-station
```

### 运行

```sh
./tiny-station
# 指定配置文件(默认为可执行文件同目录下的 tiny-station.toml):
./tiny-station --config /path/to/tiny-station.toml
```

首次运行会在可执行文件旁自动生成 `tiny-station.toml`:从 `45910`–`45914` 里挑一个空闲端口,
并把它**固定**写入配置,之后每次启动都用该端口。启动日志会打印管理账号与组网账号
(默认均为 `mesh / 123456`,**请务必修改**)。

打开网页管理界面:

```
http://<节点IP>:<端口>/
```

界面里可以查看节点/路由状态、测试代理请求、在线编辑配置。

---

## 组网

在每台机器上编辑 `tiny-station.toml`,填写**其它节点**的地址和对方的组网账号:

```toml
name = "cloud"              # 本节点名称(显示用)
api_username = "mesh"       # 其它节点访问本节点时需要的账号
api_password = "123456"     # ↑ 对应的密码,请修改

[[peers]]
address = "192.168.1.10"    # 可写 "ip" 或 "ip:端口"
name = "home"
username = "mesh"           # 对方节点的 api 账号
password = "对方的组网密码"

[[peers]]
address = "192.168.1.66"    # 可经 home 多跳到达
name = "lan-web"
username = "mesh"
password = "对方lan节点的组网密码"
```

两台机器互相配置对方地址即可组成网络;配置多个 `peers` 会形成更复杂的拓扑,请求会自动多跳路由。
没有账号的普通 IP 也可以放在 `seeds = ["1.2.3.4"]` 里(不鉴权,适合可信内网)。

---

## 让本节点的资源被全网访问

### 声明可达目标 `reachable`

只有本节点能直达的主机 / 域名 / 网段 / 通配符;本节点会周期性主动探测,**只有真正可达才广播**给全网:

```toml
reachable = [
  "127.0.0.1:39181",          # host:port,直接 TCP 探测
  "esi.evetech.net",          # 域名,按 443/80 探测(可经本机出口代理)
  "192.168.1.0/24",           # 网段
  "*.internal.example.com",   # 通配符子域
]
```

### 提供出口代理 `proxies`

本机安装的 clash / v2ray 等,其它节点可用 `via=名称` 借用:

```toml
[[proxies]]
name = "clash"
kind = "http"                # http | socks5
url = "http://127.0.0.1:7890"
```

心跳还会自动扫描本机常见代理端口(7890 / 7897 / 1080 等)并自动登记,无需手写;
如需关闭自动扫描:`auto_proxy_scan = false`。

### 直连外网

若本节点自身可直连公网,设置 `internet = true`,公网请求会优先交给有外网出口的节点。

---

## 从其它服务发起请求

推荐使用标准接口 `POST /api/request`,**参数全部放在 JSON body**,不受 query 长度限制:

```sh
curl -u mesh:123456 \
  -H 'content-type: application/json' \
  -d '{
        "url": "http://192.168.1.66:8080/api/orders",
        "method": "POST",
        "headers": {"authorization": "Bearer target-token"},
        "body": {"sku": "A-1", "qty": 2}
      }' \
  http://<节点IP>:45910/api/request
```

响应为 JSON 信封(含 `status` / `headers` / `body`(base64)/ `served_by` / `path` 等)。

- 完整字段说明、路由语义与错误处理见 **[API.md](API.md)**。
- 可直接复用 / 参考的客户端封装见 **[examples/](examples/)**(Python、Node.js、shell)。

对于大文件上传或需要原样透传的场景,可用裸流接口 `/proxy`:

```sh
curl -u mesh:123456 -X POST \
  -H 'X-Tiny-Station-Target: https://example.com/upload' \
  --data-binary @file.bin \
  http://<节点IP>:45910/proxy
```

---

## 配置项一览

| 键 | 说明 |
| --- | --- |
| `name` | 本节点名称(显示用) |
| `ip` | 对外广播的 IP;不填自动探测内网地址 |
| `bind` | 本地绑定网卡,默认 `0.0.0.0` |
| `port` | 固定监听端口(首次运行后自动写入) |
| `ports` | 扫描/候选端口列表,默认 `[45910..45914]` |
| `seeds` | 无需鉴权的普通 IP 列表 |
| `api_username` / `api_password` | 其它节点访问本节点所需的组网账号 |
| `admin_username` / `admin_password` | 管理界面账号(不填则复用组网账号) |
| `internet` | 本节点是否能直连外网 |
| `reachable` | 只有本节点能直达的主机 / 域名 / 网段 / 通配符 |
| `[[proxies]]` | 本机出口代理(`name` / `kind` / `url`) |
| `auto_proxy_scan` | 是否自动扫描本机代理端口,默认开启 |
| `gossip_interval_secs` | 发现 / 心跳 / 广播间隔秒数,默认 5 |
| `[[peers]]` | 组网目标(`address` / `name` / `username` / `password`) |

更详细的注释见 [config.example.toml](config.example.toml)。

---

## 构建与发布

- 本地构建:`cargo build --release`(产物 `target/release/tiny-station`)。
- 打标签即触发 GitHub Actions 自动构建并发布多平台 Release(见
  [.github/workflows/release.yml](.github/workflows/release.yml)):

  ```sh
  git tag v0.1.0
  git push origin v0.1.0
  ```

  产物命名如 `tiny-station-linux-x86_64.tar.gz`、`tiny-station-windows-x86_64.zip`。

---

## 端口与安全

- 默认端口 `45910`–`45914`,首次运行后固定一个;节点间互通需在防火墙 / 安全组放行该端口(或整个范围)。
- 默认账号密码为 `mesh / 123456` 与 `admin / 123456`,对外使用前请务必修改。
- 组网与管理接口都使用 Basic 鉴权;若未配置账号则该接口无鉴权,仅建议在可信内网使用。
