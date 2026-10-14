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
- **NAT 穿透**:位于 NAT 后、只能出站访问的节点会自动与中心节点建立**反向长轮询通道**,由中心节点中转,
  从而让两台都在 NAT 后的节点也能互访(`A → 中心 → B`),无需端口映射。
- **标准化 API**:`POST /api/request` 所有参数走 JSON body,不用 query,不受 URL 长度限制。
- **加密组网**:节点间通讯强制加密 —— 心跳/控制面使用私有二进制编码,代理请求/数据面用
  配对会话密钥加密(XChaCha20-Poly1305)。会话密钥由**自动生成的身份密钥 + 临时 ECDH**
  动态协商,密钥泄露可轮换并自动广播全网,无需为每个节点单独配置密钥。
- **密钥轮换**:管理接口 `POST /api/rotate` 可设置新的 mesh 密钥并自动广播到全网;
  后加入的节点在入网握手中自动同步最新密钥。
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
mesh_key = "同一串根密钥"    # 全网统一,所有节点必须一致(用于握手认证与入网,可轮换)
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

### 位于 NAT 后的节点(反向通道)

若某台节点只有出站访问能力(位于 NAT 后),对端无法主动连它。只要**任意一方**在配置里把对端
配成 `[[peers]]`,双方即可**双向访问**——不需要对方反向配置,也**绝不回拨**对方广播的 IP。

原理:被配置的一方启动后,会主动向对端建立一条**反向长轮询通道**。对端借这条**由对方主动发起**的
连接把请求下发、回收;因此:

- 单条(单向)配置即可实现双向可达:例如 A 只配置了 B,则 `A → B` 走 A 主动连接,`B → A` 走反向通道;
- 两个都在 NAT 后的节点也能互访:请求走 `A → 中心 → B`,中心节点经各自的反向通道下发、收回;
- 任何节点都不会为了「反向」去连接对方广播的私有 / NAT IP;反向方向只走对方主动建立的通道;
- 无需在任一侧做端口映射 / 公网 IP。

> 注意:能直连的方向(A 主动连 B)仍走直连;反向方向(B 连 A)只走反向通道,永不回拨。
> 反向通道上的请求会多一次长轮询往返(通常毫秒级),收到任务即时唤醒,不是按心跳周期排队。

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
| `mesh_key` | **必填**。全网统一的**根 / 入网密钥**,所有节点必须一致;用于握手认证与新节点入网 |
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

## 管理界面(前端)

管理界面是一个独立的 Vue 单页应用,源码在 `web/`,构建产物在编译期被**嵌入二进制**
(`rust-embed`),因此发布仍是单文件。

- 技术栈:**Vue 3 + Vite + TypeScript + Vue Router + Naive UI**。
- **按业务拆分页面**(路由级懒加载,每页单独打包):总览 / 节点 / 组网配置 / 代理测试 / 内核。
- **Naive UI 按需引入**(逐组件 `import`),由 Rollup 自动**树摇**;无需全局样式表。
- 鉴权沿用 HTTP Basic:首次加载页面时浏览器弹窗,后续同源请求自动复用。

```sh
# 开发(热更新,自动把 /api、/proxy、/x 代理到本机节点)
cd web && pnpm install && pnpm dev

# 构建产物,再编译进二进制
cd web && pnpm build && cd .. && cargo build --release
```

> 未构建前端时,`cargo build` 会嵌入一个占位页提示先执行 `pnpm build`(`build.rs` 保证
> `web/dist` 存在)。CI/发布流程会自动先构建前端再编译 Rust。

---

## 端口与安全

- 默认端口 `45910`–`45914`,首次运行后固定一个;节点间互通需在防火墙 / 安全组放行该端口(或整个范围)。
- 默认账号密码为 `mesh / 123456` 与 `admin / 123456`,对外使用前请务必修改。
- 组网与管理接口都使用 Basic 鉴权;若未配置账号则该接口无鉴权,仅建议在可信内网使用。
- **节点间通讯强制加密**:心跳 / 控制面(gossip、探活、可达发现)使用私有二进制编码 +
  认证加密;代理请求 / 数据面(tunnel、反向通道,含目标 URL、请求头与 body)同样加密。
- **密钥体系分两层**:
  - **根 / 入网密钥**(配置里的 `mesh_key`,全网统一、长期不变):只用于**握手认证**(PSK)
    和新节点入网,不直接加密消息。
  - **会话密钥**(每对节点一个):由每个节点**首次运行自动生成**的 X25519 身份密钥 +
    当前临时 ECDH 派生(HKDF-SHA256,AEAD 为 XChaCha20-Poly1305),附时间戳窗口防重放。
    你无需为每个节点配置任何密钥文件。
- **密钥轮换**:任一节点管理员可 `POST /api/rotate {"mesh_key":"新值"}`(省略则随机生成),
  该节点立即换用新 cluster key 并自动广播;**后加入的节点**在入网握手中自动同步最新 cluster key。
  所以轮换后既能传播到现有节点,也不影响新节点加入。
- `mesh_key` 是真正的信任根,请妥善保管、勿泄露;Basic 账号密码仅作为附加认证。首次运行
  会在配置里自动生成随机 `mesh_key`,复制到其它节点即可。`.identity` / `.clusterkey` 为
  自动生成本地文件,勿外泄、勿跨节点复用。
- **重放防护**:每条消息带随机 id 与时间戳,接收端在时间窗内拒绝重复报文,并拒绝超出窗口
  (默认 ±5 分钟)的报文。因此**各节点时钟需保持同步(建议开启 NTP)**。
- **健壮性**:密钥/配置文件采用「临时文件 + 原子重命名」写入,崩溃不会写坏;`.clusterkey`
  损坏会告警并回退到 epoch 0;epoch 设了合理上限,单个异常节点无法把全网 epoch 顶到不可用。
- 轮换后各节点在新会话握手时自动换用新 cluster key;旧会话在 TTL 内仍有效。`/api/status`
  的 `cluster_epoch` 可用于观察全网是否已同步。

## 可热替换内核(Luau)

节点可以在**不重启进程**的前提下,通过一段 Luau 逻辑覆盖少量行为(路由决策、出站
请求/响应变换、自定义端点)。设计上逻辑只是「可选覆盖」:**任何钩子返回 `nil` 就自动
落回节点内置行为**,没有内核时行为与普通节点完全一致。

### 内核是「模块集合」,不是单个文件

真实业务逻辑会被拆成多个文件:入口 `main.lua` + 任意数量的 `require` 模块。

```
.tiny-station.kernel/
  main.lua            # 入口:返回内核表(abi / version + 可选钩子)
  lib/util.lua        # require("lib.util") 引用
  routes/admin.lua    # 策略/接口按域拆分
```

- **`require`**:沙箱内的模块加载器,只解析内核自带模块(无文件系统),支持 `"lib.util"`
  与 `"lib/util"` 两种写法,并缓存(标准 Lua 语义)。
- 入口模块返回一个 table:`abi`(必填)、`version`(必填),以及可选的 `init` /
  `route` / `on_request` / `on_response` / `endpoints`。
- 加载方式:默认读取配置同目录的 `.<配置名>.kernel/` 目录(存在即加载);`kernel_file`
  指向 `.lua` 文件时按单文件内核处理。
- 见 `examples/kernel/`(多模块示例)。

### 一键热升级(推荐)

打开网页管理界面 → 「内核 · 热升级」→ 粘贴入口脚本 → 点「仅本机升级」或「全网升级」。
**上传即刻生效,无需重启、无需密钥。**

```bash
# 单文件:粘贴/上传 main.lua 内容(保留其它模块)
curl -u admin:123456 -X POST --data-binary @examples/kernel/main.lua \
     "http://127.0.0.1:45910/api/kernel?broadcast=false"

# 多文件:POST JSON 模块表(?broadcast=true 推送到全网)
curl -u admin:123456 -H 'content-type: application/json' --data-binary '{
  "entry": "main.lua",
  "files": {
    "main.lua": "local u=require(\"lib.util\")\n...",
    "lib/util.lua": "return { greet=function(n) return \"hi \"..n end }"
  }
}' "http://127.0.0.1:45910/api/kernel?broadcast=true"

# 查看状态(含模块列表)/ 载入入口模块 / 从磁盘重载 / 回滚 / 卸载
curl -u admin:123456 http://127.0.0.1:45910/api/kernel
curl -u admin:123456 http://127.0.0.1:45910/api/kernel/source
curl -u admin:123456 -X POST http://127.0.0.1:45910/api/kernel/reload
curl -u admin:123456 -X POST http://127.0.0.1:45910/api/kernel/rollback
curl -u admin:123456 -X DELETE http://127.0.0.1:45910/api/kernel
```

- Host API(`ctx`):

  | 命名空间 | 能力 |
  | --- | --- |
  | `ctx.log` | `debug/info/warn/error` 写入节点日志 |
  | `ctx.time` | `now` / `monotonic` / `sleep` |
  | `ctx.store` | 持久 KV(`get/set/del/incr/keys`,支持 TTL),跨热替换 |
  | `ctx.crypto` | `sha256` / `hmac_sha256` / `hkdf` / `rand` / `b64_*` / `hex_*` |
  | `ctx.node` | 只读节点视图(`self` / `peers` / `config` / `cluster_epoch`) |
  | `ctx.metrics` | `counter` / `gauge` / `histogram`(见 `GET /api/kernel`) |
  | `ctx.timer` | `every` / `after` / `cancel`(热替换后由新脚本重新注册) |
  | `ctx.queue` | `spawn(fn)` 投递异步回调 |
  | `ctx.net` | `fetch(req)` 出站 HTTP(可指定出口代理) |
  | `ctx.mesh` | `call(dest, req)` 跨节点请求 / `who_can_reach(host, port)` |

- 自定义端点:入口模块返回 `endpoints` 表,`/x/<name>` 即可访问(组网账号鉴权)。
- 隔离与安全:每个内核在独立线程的独立 Luau 虚拟机中运行(默认 1 个,可配
  `kernel_workers`),内存上限 64MB;单次钩子有 CPU 时间预算(死循环会被中断并回退内置
  行为);`os`/`io`/`load`/`dofile`/`package` 被禁用,`require` 仅能解析内核自带模块;
  模块路径做了防穿越校验。钩子出错只记日志并落回内置行为,绝不会带崩节点。ABI 不匹配
  的内核会被拒绝加载。

### 全网分发与可选签名加固

内核是 KB 级文本,直接随**已加密的 mesh 会话**扩散,无需分块。默认信任模型与
cluster-key 轮换一致:推送经过 `mesh_key` 握手的会话密钥加密与认证,因此**开箱即用、
无需配置密钥**(`kernel_allow_unsigned=true`,默认)。

- **扩散**:收到并安装的节点会继续向自己的 peers 转发,全网收敛,全程**无需重启**。
- **防回滚**:`rev` 单调递增,重复/过期 bundle 直接拒绝(HTTP 409);`POST /api/kernel/rollback`
  秒级切回上一版,且坏版本无法回灌。
- **灰度**:`?broadcast=false` 只在当前节点生效(先验证再全网)。

如需**额外**的不可抵赖性(防止被入侵的 mesh 成员推送代码),可启用 Ed25519 签名:

```bash
tiny-station keygen                       # 生成密钥对(私钥仅留 CI/本机)
# 各节点配置:release_pubkey = "<公钥>"
tiny-station sign examples/kernel --key <私钥> --rev 2 --version 0.3 > bundle.json
curl -u admin:123456 -H 'content-type: application/json' \
     --data @bundle.json http://127.0.0.1:45910/api/kernel/distribute
```

配置了 `release_pubkey` 后,节点只接受用它签名的内核;`kernel_allow_unsigned=false` 则
完全拒绝未签名推送(仅允许本机上传)。
