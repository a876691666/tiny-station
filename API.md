# tiny-station HTTP API

tiny-station 把多台机器组成一个逻辑代理网络(mesh)。任意服务只需连接**其中一个节点**,
就能通过该节点把请求路由到全网:节点会自己挑选「真正能到达目标」的那台机器去执行请求,
必要时借用对方的本机出口代理(clash / v2ray 等)。

本文档面向**接入方**,描述如何用 HTTP 与某个节点交互。

- 节点地址:`http://<节点IP>:<端口>`,默认端口从 `45910`–`45914` 中选取(首次运行后固定)。
- 鉴权:HTTP Basic。组网账号为节点配置里的 `api_username` / `api_password`
  (默认 `mesh` / `123456`,务必修改)。管理/状态接口也接受 `admin_username` / `admin_password`。
- 控制面与数据面都支持 Basic 鉴权;未配置账号时该接口无鉴权(仅建议在可信内网使用)。

---

## 数据面 API

### POST /api/request —— 标准请求接口(推荐)

**核心约定:所有参数都放在 JSON 请求体里,不放在 query,因此没有 URL 长度限制。**
目标 URL、请求头、请求体、路由方式全部走 body。

请求头:

| Header | 值 |
| --- | --- |
| `Authorization` | `Basic <base64(组网账号:组网密码)>` |
| `Content-Type` | `application/json` |

请求体字段:

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `url` | string | 必填 | 目标绝对 URL,如 `https://api.example.com/v1/items?page=2` |
| `method` | string | `GET` | HTTP 方法 |
| `headers` | object | `{}` | 转发给目标的请求头。这里的 `authorization` 是发给**目标**的,与连接本节点的 Basic 鉴权互不影响 |
| `body` | any | 无 | 请求体。字符串按 `body_encoding` 处理;对象/数组/数字等其它 JSON 值会被序列化成 JSON,并自动补 `content-type: application/json`(未显式设置时) |
| `body_encoding` | string | `utf8` | `utf8`(`body` 字符串原样发送)或 `base64`(`body` 字符串先 base64 解码) |
| `via` | string | `auto` | 路由方式:`auto` / 节点名 / 节点ID / 出口代理名 / `self` |
| `egress` | string | 无 | 指定执行节点上要使用的出口代理名(如 `clash`) |
| `timeout_ms` | number | `90000` | 上游请求超时(毫秒) |
| `mode` | string | `json` | `json` 返回下方 JSON 信封;`raw` 直接原样返回上游响应 |

响应(`mode=json`,默认)字段:

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `ok` | bool | 请求是否被 mesh 成功执行(上游返回 4xx/5xx 仍为 `true`) |
| `status` | number | 上游 HTTP 状态码 |
| `headers` | object | 上游响应头(键小写,重复项保留最后一个) |
| `cookies` | array | 所有 `Set-Cookie` 值(保留重复) |
| `body` | string | 上游响应体,**base64 编码** |
| `body_encoding` | string | 固定为 `base64` |
| `served_by` | string | 真正执行请求的节点 ID |
| `path` | array | 请求经过的节点 ID 列表(入口 → `served_by`) |
| `egress` | string? | 实际使用的出口代理名(直连时省略) |
| `elapsed_ms` | number | 耗时(毫秒) |
| `error` | string? | 路由/传输错误信息(`ok=false` 时存在) |

#### `via` 路由语义

| `via` 值 | 含义 |
| --- | --- |
| `auto`(默认) | 自动挑选:谁能直达目标就交给谁;公网目标优先用有外网出口的节点 |
| 节点名 / 节点ID | 指定由该节点执行(只影响「谁去发」,不影响目标地址) |
| 出口代理名 | 在本节点(或拥有该代理的节点)上经该代理发出 |
| `self` / `direct` / `local` | 强制本节点直连发出 |

### POST / GET /proxy —— 裸流兼容接口(大文件上传)

和 `/api/request` 功能相同,但请求体**原样透传**(不做 base64/JSON 包装),响应也**原样返回**
(附 `x-tiny-station-served-by` / `x-tiny-station-path` / `x-tiny-station-egress` 等头)。
适合上传大文件或需要流式转发的场景。

目标与路由可放在 query(短 URL 场景):

```
POST /proxy?target=https://example.com/upload&via=auto
```

或放在请求头(避免 query 长度限制):

| Header | 说明 |
| --- | --- |
| `X-Tiny-Station-Target` | 目标绝对 URL |
| `X-Tiny-Station-Via` | 路由方式(同 `via`) |

请求体即要发送给目标的内容;除 `Authorization` / `Host` / `X-Tiny-Station-*` 外的请求头会转发给目标。

---

## 控制面 API

除特别说明外均需 Basic 鉴权。`/api/status`、`/api/config`、`/` 需要管理鉴权。

控制面中节点间使用的接口(`/api/handshake`、`/api/peers`、`/api/gossip`、`/api/reach`、`/api/tunnel`、
`/api/reverse/poll`、`/api/keyrotate`)全部为 `POST`,请求与响应 body 都是**私有二进制编码 + 加密的信封**
(`Content-Type: application/octet-stream`)。除握手外,请求需带 `X-TS-Node: <发送方节点id>` 头以定位配对会话密钥。
握手用全网统一的 `mesh_key`(根/入网密钥)派生密钥加密,其余用配对会话密钥(自动协商)。下表描述语义载荷。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/api/handshake` | 入网握手:交换身份+临时公钥,协商会话密钥并同步 cluster key(根密钥加密) |
| POST | `/api/peers` | 拉取本节点广播的全网视图 |
| POST | `/api/gossip` | 合并对方视图并返回自己的视图(节点间使用) |
| POST | `/api/reach` | 探测本节点能否直达某目标,收发 `{host,port}` / `{reachable,egress}` |
| POST | `/api/tunnel` | 节点间隧道协议(内部使用,收发 `TunnelRequest` / `TunnelResponse`) |
| POST | `/api/reverse/poll` | 反向通道长轮询(NAT 节点 → 中心节点,内部使用) |
| POST | `/api/keyrotate` | 接收并采用对方的 cluster key 轮换(epoch 更新时) |
| GET | `/api/status` | 全量状态:节点、路由、能力、出口、端口、`cluster_epoch` 等 |
| GET | `/api/config` | 读取完整配置 |
| POST | `/api/config` | 覆盖写入配置(立即生效,`ip`/`bind`/`ports` 需重启) |
| POST | `/api/rotate` | **管理接口**:设置新 cluster key `{"mesh_key":"新值"}`(省略则随机)并广播全网 |
| GET | `/api/kernel` | **管理接口**:查看当前内核状态(`{loaded,abi,version,workers,source,rev,metrics,files}`) |
| POST | `/api/kernel` | **管理接口**:上传内核。**内核包(`.zip`)** = 整包替换;JSON `{entry,files}` = 整包替换;纯文本 = `main.lua`(保留其它模块)。编译后热替换并落盘;`?broadcast=true` 推送全网 |
| GET | `/api/kernel/source` | **管理接口**:返回入口模块(`main.lua`)原文(供管理界面编辑器) |
| POST | `/api/kernel/reload` | **管理接口**:从磁盘上的内核文件重新加载 |
| POST | `/api/kernel/rollback` | **管理接口**:回滚到上一版内核(防回滚基线保留) |
| POST | `/api/kernel/distribute` | **管理接口**:上传**已签名** bundle(JSON),验签+防回滚后本地生效并广播全网(`?broadcast=false` 仅本节点灰度) |
| POST | `/api/kernel/sync` | **节点间**:内核 bundle 广播(会话密钥加密,内部使用) |
| DELETE | `/api/kernel` | **管理接口**:卸载内核,回退到内置行为 |
| ANY | `/x/*` | 内核自定义端点(需组网账号鉴权);无内核或路径未注册时 404 |
| GET | `/` | 网页管理界面 |

> **NAT 穿透**:位于 NAT 后、只能出站的节点会自动向配置的 `peers` 发起 `/api/reverse/poll` 长轮询。
> 对端可经该通道向它下发请求,因此**单向配置即可双向访问**,且**绝不回拨**对方广播的 IP。
> 详见 [README](README.md#位于-nat-后的节点反向通道)。

> **密钥轮换**:任一节点管理员 `POST /api/rotate` 后,该节点立即换用新 cluster key 并广播;
> 收到 `/api/keyrotate`(epoch 更新)的节点持久化新密钥并在下一轮 gossip 继续传播。
> **后加入的节点**用根密钥 `mesh_key` 完成 `/api/handshake` 时,会从对端收到当前 cluster key,
> 因此无论期间轮换过多少次都能自动同步最新值。

> **可热替换内核**:`POST /api/kernel` 上传一段 Luau 脚本(非 JSON,纯文本 body),
> 节点先编译校验(`abi` 必须为 1),成功后**热替换当前内核并落盘**,无需重启。
> 加 `?broadcast=true` 会把该版本推送到全网所有节点立即生效。管理界面「内核 · 热升级」
> 面板提供同样的**一键升级**。默认信任已认证的 mesh 会话;配置 `release_pubkey` 后则
> 强制签名校验(见 `POST /api/kernel/distribute`)。脚本可覆盖路由(`route`)、出站请求/
> 响应变换(`on_request` / `on_response`)与自定义端点(`endpoints`)。详见
> [README](README.md#可热替换内核luau) 与 [`examples/kernel.lua`](examples/kernel.lua)。

---

## 示例

`examples/` 下是可直接运行、也可作为库引入的客户端封装(响应体自动解码,`raiseForStatus` 统一抛错):

- [`examples/request.py`](examples/request.py) — Python `TinyStation`(仅标准库)
- [`examples/request.js`](examples/request.js) — Node `TinyStation`(内置 `fetch`)
- [`examples/request.sh`](examples/request.sh) — shell:source 后得到 `ts_get` / `ts_post` / `ts_request`

### curl

```sh
NODE=http://192.168.1.10:45910

# GET,参数全部在 JSON 里
curl -s "$NODE/api/request" \
  -u mesh:123456 \
  -H 'content-type: application/json' \
  -d '{"url":"https://api.example.com/v1/items?page=2","via":"auto"}'

# POST JSON body(直接放对象,自动序列化)
curl -s "$NODE/api/request" \
  -u mesh:123456 \
  -H 'content-type: application/json' \
  -d '{
        "url": "http://192.168.1.66:8080/api/orders",
        "method": "POST",
        "headers": {"authorization": "Bearer target-token"},
        "body": {"sku": "A-1", "qty": 2}
      }'

# 指定出口代理
curl -s "$NODE/api/request" -u mesh:123456 \
  -H 'content-type: application/json' \
  -d '{"url":"https://esi.evetech.net/latest/status/","egress":"clash"}'
```

### Python(仅标准库)

```python
from request import TinyStation

ts = TinyStation("http://192.168.1.10:45910", "mesh", "123456")

# GET(默认),body 已解码,json() 直接解析
r = ts.get("https://api.example.com/v1/items?page=2")
print(r.status, r.served_by, " -> ".join(r.path))
data = r.raise_for_status().json()

# POST JSON:json= 传字典即可;headers 里的 authorization 发给目标
r = ts.post(
    "http://192.168.1.66:8080/api/orders",
    json={"sku": "A-1", "qty": 2},
    headers={"authorization": "Bearer target-token"},
)
r.raise_for_status()

# 指定出口代理 / 超时
r = ts.get("https://esi.evetech.net/latest/status/", egress="clash", timeout_ms=15000)
```

### Node.js(内置 fetch)

```js
const { TinyStation } = require("./request");

const ts = new TinyStation("http://192.168.1.10:45910", {
  username: "mesh",
  password: "123456",
});

const r = await ts.get("https://api.example.com/v1/items?page=2");
console.log(r.status, r.servedBy, r.path.join(" -> "));
const data = r.raiseForStatus().json();

const r2 = await ts.post("http://192.168.1.66:8080/api/orders", {
  json: { sku: "A-1", qty: 2 },
  headers: { authorization: "Bearer target-token" },
});
```

### shell(curl)

```sh
. ./request.sh
ts_get  "https://api.example.com/v1/items?page=2"
ts_post "http://192.168.1.66:8080/api/orders" '{"sku":"A-1","qty":2}'
ts_get  "https://api.example.com/v1/items" | jq -r '.body' | base64 -d
```

### 错误处理

- HTTP 状态码是**连接本节点**的结果:`401` 鉴权失败,`400` 请求信封非法(base64/编码错误等)。
- `ok=false` 且带 `error` 表示 mesh 内部路由/传输失败(如无路由、下一跳不可达、上游超时)。
- 上游自身的 `4xx/5xx` 会体现在 `status`,此时 `ok=true`。
