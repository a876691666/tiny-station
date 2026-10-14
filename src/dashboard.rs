use crate::config::Config;
use crate::protocol::StatusResponse;
use std::collections::HashSet;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn opt(s: &Option<String>) -> String {
    esc(s.as_deref().unwrap_or(""))
}

fn peer_row(p: &crate::config::PeerConfig) -> String {
    format!(
        "<tr><td><input class=\"pc-address\" value=\"{addr}\" placeholder=\"192.168.1.10\"></td><td><input class=\"pc-name\" value=\"{name}\" placeholder=\"home\"></td><td><input class=\"pc-user\" value=\"{user}\"></td><td><input class=\"pc-pass\" type=\"password\" value=\"{pass}\"></td><td><button class=\"del\">×</button></td></tr>",
        addr = esc(&p.address),
        name = opt(&p.name),
        user = opt(&p.username),
        pass = opt(&p.password),
    )
}

fn proxy_row(p: &crate::config::ProxyConfig) -> String {
    format!(
        "<tr><td><input class=\"px-name\" value=\"{name}\" placeholder=\"clash\"></td><td><input class=\"px-kind\" value=\"{kind}\" placeholder=\"http\"></td><td><input class=\"px-url\" value=\"{url}\" placeholder=\"http://127.0.0.1:7890\"></td><td><button class=\"del\">×</button></td></tr>",
        name = esc(&p.name),
        kind = esc(&p.kind),
        url = esc(&p.url),
    )
}

fn status_label(state: &str) -> &'static str {
    match state {
        "self" => "本机",
        "ok" => "直连正常",
        "indirect" => "经其它节点可达",
        _ => "不可达",
    }
}

pub fn render(s: &StatusResponse, cfg: &Config) -> String {
    let routed: HashSet<&str> = s.routes.iter().map(|r| r.dest_id.as_str()).collect();

    let mut nodes = String::new();
    for n in &s.nodes {
        let state = if n.is_self {
            "self"
        } else if n.directly_reachable {
            "ok"
        } else if routed.contains(n.info.id.as_str()) {
            "indirect"
        } else {
            "bad"
        };
        let proxies = if n.proxies.is_empty() {
            "-".to_string()
        } else {
            n.proxies
                .iter()
                .map(|p| format!("{} ({}) {}", esc(&p.name), esc(&p.kind), esc(&p.url)))
                .collect::<Vec<_>>()
                .join("<br>")
        };
        let reachable = if n.reachable.is_empty() {
            "-".to_string()
        } else {
            n.reachable.iter().map(|r| esc(r)).collect::<Vec<_>>().join("<br>")
        };
        let detail = if state == "bad" {
            n.last_error
                .as_deref()
                .map(|e| format!("<span class=\"reason\">{}</span>", esc(e)))
                .unwrap_or_else(|| "<span class=\"reason\">探测失败</span>".to_string())
        } else if n.is_self {
            "运行中".to_string()
        } else {
            match n.latency_ms {
                Some(ms) => format!("{} ms", ms),
                None => "-".to_string(),
            }
        };
        let addr = if n.info.port > 0 {
            format!("{}:{}", esc(&n.info.ip), n.info.port)
        } else {
            esc(&n.info.ip)
        };
        nodes.push_str(&format!(
            "<tr class=\"row-{state}\"><td><span class=\"dot {state}\"></span><span class=\"name {state}\">{name}</span>{self_tag}</td><td><code>{id}</code></td><td>{addr}</td><td><span class=\"pill {state}\">{label}</span></td><td>{detail}</td><td>{links}</td><td>{internet}</td><td>{proxies}</td><td>{reachable}</td></tr>",
            state = state,
            name = esc(&n.info.name),
            self_tag = if n.is_self { " (本机)" } else { "" },
            id = esc(&n.info.id),
            addr = addr,
            label = status_label(state),
            detail = detail,
            links = n.links.len(),
            internet = if n.internet { "有" } else { "-" },
            proxies = proxies,
            reachable = reachable,
        ));
    }

    let mut routes = String::new();
    for r in &s.routes {
        let path = r
            .path
            .iter()
            .map(|h| {
                s.nodes
                    .iter()
                    .find(|n| &n.info.id == h)
                    .map(|n| esc(&n.info.name))
                    .unwrap_or_else(|| esc(h))
            })
            .collect::<Vec<_>>()
            .join(" &rarr; ");
        routes.push_str(&format!(
            "<tr><td>{dest}</td><td>{hops}</td><td>{path}</td></tr>",
            dest = esc(&r.dest_name),
            hops = r.hops,
            path = path,
        ));
    }
    if routes.is_empty() {
        routes.push_str("<tr><td colspan=\"3\">暂无其它节点</td></tr>");
    }

    let mut caps = String::new();
    for c in &s.capabilities {
        caps.push_str(&format!(
            "<tr><td>{node}</td><td>{name}</td><td>{kind}</td><td>{url}</td><td>{net}</td></tr>",
            node = esc(&c.node_name),
            name = esc(&c.name),
            kind = esc(&c.kind),
            url = esc(&c.url),
            net = if c.internet { "可上外网" } else { "-" },
        ));
    }
    if caps.is_empty() {
        caps.push_str("<tr><td colspan=\"5\">暂无出口代理</td></tr>");
    }

    let mut egress_rows = String::new();
    for e in &s.egress {
        let (cls, label) = if e.alive {
            ("ok", "探测可达")
        } else {
            ("bad", "探测不可达")
        };
        egress_rows.push_str(&format!(
            "<tr><td>{name}</td><td>{kind}</td><td>{url}</td><td><span class=\"pill {cls}\">{label}</span></td></tr>",
            name = esc(&e.name),
            kind = esc(&e.kind),
            url = esc(&e.url),
            cls = cls,
            label = label,
        ));
    }
    if s.egress.is_empty() {
        egress_rows.push_str("<tr><td colspan=\"4\">未配置出口代理</td></tr>");
    }

    let peer_rows: String = cfg.peers.iter().map(peer_row).collect();
    let proxy_rows: String = cfg.proxies.iter().map(proxy_row).collect();
    let reachable_text = esc(&cfg.reachable.join("\n"));
    let portinfo = match s.pinned_port {
        Some(p) => format!("端口已固定为 {}", p),
        None => "端口将在首次运行后自动固定".to_string(),
    };
    let cfg_json = serde_json::to_string(cfg)
        .unwrap_or_else(|_| "{}".into())
        .replace('<', "\\u003c");
    let kernel_status = if s.kernel.loaded {
        let mods = if s.kernel.files.is_empty() {
            String::new()
        } else {
            format!(" · {} 模块: {}", s.kernel.files.len(), esc(&s.kernel.files.join(", ")))
        };
        format!(
            "已加载 <b>{}</b> · rev {} · {} worker{}",
            esc(&s.kernel.version),
            s.kernel.rev,
            s.kernel.workers,
            mods
        )
    } else {
        format!("未加载(当前 rev {})", s.kernel.rev)
    };

    format!(
        r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>tiny-station · {name}</title>
<style>
:root{{color-scheme:dark}}
body{{font-family:ui-monospace,Menlo,Consolas,"Microsoft YaHei",monospace;background:#0f1115;color:#d7dce2;margin:0;padding:24px}}
h1{{font-size:18px;margin:0 0 4px}} h2{{font-size:13px;letter-spacing:.05em;color:#8b95a3;margin:28px 0 8px}}
.sub{{color:#8b95a3;font-size:12px;margin-bottom:16px}}
table{{border-collapse:collapse;width:100%;font-size:12px}} th,td{{text-align:left;padding:6px 10px;border-bottom:1px solid #1e2430;vertical-align:top}}
th{{color:#8b95a3;font-weight:600}}
.dot{{display:inline-block;width:9px;height:9px;border-radius:50%;margin-right:6px}}
.dot.ok,.pill.ok{{background:#1a3d24}} .dot.self,.pill.self{{background:#12335c}} .dot.indirect,.pill.indirect{{background:#4a3a12}} .dot.bad,.pill.bad{{background:#4a1618}}
.dot.ok{{background:#39d353}} .dot.self{{background:#58a6ff}} .dot.indirect{{background:#e3b341}} .dot.bad{{background:#f85149}}
.pill{{display:inline-block;padding:1px 8px;border-radius:10px;font-size:11px;color:#fff}}
.pill.ok{{background:#2ea043}} .pill.self{{background:#1f6feb}} .pill.indirect{{background:#9e6a03}} .pill.bad{{background:#da3633}}
.name.ok{{color:#7ee787}} .name.self{{color:#79c0ff}} .name.indirect{{color:#e3b341}} .name.bad{{color:#ff7b72;font-weight:700}}
.row-bad td{{background:#2a1215}} .row-indirect td{{background:#211b0e}}
.reason{{color:#ff7b72}}
.legend{{font-size:12px;color:#8b95a3;margin:6px 0 12px}} .legend b{{color:#d7dce2}}
input,textarea{{background:#161b22;border:1px solid #2d333b;color:#d7dce2;padding:6px 8px;border-radius:6px;width:100%;box-sizing:border-box}}
button{{background:#238636;border:0;color:#fff;padding:7px 14px;border-radius:6px;cursor:pointer}} button.del{{background:#3d1a1a;padding:2px 8px}} button.ghost{{background:#21262d}}
pre{{background:#161b22;border:1px solid #2d333b;border-radius:6px;padding:12px;overflow:auto;max-height:320px}}
code{{color:#79c0ff}} .row{{display:flex;gap:10px;flex-wrap:wrap;align-items:center;margin:6px 0}} .grid{{display:grid;grid-template-columns:170px 1fr;gap:8px;max-width:760px}} label{{color:#8b95a3;font-size:12px;align-self:center}}
#toast{{margin-left:12px;color:#39d353}}
body{{padding-bottom:72px}}
.savebar{{position:fixed;left:0;right:0;bottom:0;background:#161b22;border-top:1px solid #2d333b;padding:10px 24px;display:flex;gap:12px;align-items:center;z-index:10}}
</style></head><body>
<h1>tiny-station <span class="sub">v{version}</span></h1>
<div class="sub">节点 <b>{name}</b> · id <code>{id}</code> · 监听 <code>{ip}:{port}</code> · {portinfo} · 扫描端口 {ports} · 已运行 {uptime} 秒</div>

<h2>代理测试</h2>
<div class="sub">请求会经全网路由。via 填 auto 自动选择,或填节点名 / 节点ID / 出口代理名。</div>
<div class="row"><input id="url" placeholder="https://example.com/api" value="http://127.0.0.1:8000/" style="width:min(560px,60%)">
<input id="via" value="auto" style="width:260px"><button onclick="run()">发送</button></div>
<pre id="out">就绪。</pre>

<h2>内核 · 热升级(免重启)</h2>
<div class="sub">粘贴入口脚本(main.lua)后点「仅本机升级」即热替换当前节点;「全网升级」会把新版推送到所有节点并自动生效。多模块内核请用 JSON <code>{{entry,files}}</code> 上传。当前:{kernel_status}</div>
<textarea id="k-code" rows="10" placeholder="粘贴 Luau 入口脚本(参考 examples/kernel/)…"></textarea>
<div class="row">
<button onclick="kernelApply(false)">仅本机升级</button>
<button onclick="kernelApply(true)">全网升级</button>
<button class="ghost" onclick="kernelLoad()">载入当前脚本</button>
<button class="ghost" onclick="kernelDelete()">卸载内核</button>
<span id="k-toast"></span></div>

<h2>配置管理</h2>
<div class="sub">保存后立即写入文件并在运行时生效;修改 ip / bind / ports 需重启。</div>
<div class="grid">
<label>节点名称</label><input id="f-name" value="{f_name}">
<label>对外 IP</label><input id="f-ip" value="{f_ip}">
<label>组网账号</label><input id="f-apiuser" value="{f_apiuser}">
<label>组网密码</label><input id="f-apipass" type="password" value="{f_apipass}">
<label>管理账号</label><input id="f-adminuser" value="{f_adminuser}">
<label>管理密码</label><input id="f-adminpass" type="password" value="{f_adminpass}">
<label>可访问外网</label><input id="f-internet" type="checkbox" {f_internet}>
<label>心跳间隔(秒)</label><input id="f-gossip" value="{f_gossip}">
<label>可直达主机/域名(自动探测)</label><textarea id="f-reachable" rows="3" placeholder="esi.evetech.net&#10;127.0.0.1:39181&#10;192.168.1.0/24">{f_reachable}</textarea>
</div>

<h2>组网目标服务</h2>
<div class="sub">其它节点的地址 + 账号 + 密码,可添加多个。</div>
<table id="peers"><tr><th>地址</th><th>名称</th><th>账号</th><th>密码</th><th></th></tr>{peer_rows}</table>
<div class="row"><button class="ghost" onclick="addPeer()">+ 添加目标服务</button><button onclick="save()">保存配置</button></div>

<h2>本机出口代理</h2>
<div class="sub">clash / v2ray 等,其它节点可用 ?via=名称 借用。心跳会自动扫描本机常见端口(7890/7897/1080 等)并自动登记为出口。</div>
<table id="proxies"><tr><th>名称</th><th>类型</th><th>地址</th><th></th></tr>{proxy_rows}</table>
<div class="row"><button class="ghost" onclick="addProxy()">+ 添加代理</button><button onclick="save()">保存配置</button></div>

<h2>本机出口代理探测</h2>
<table><tr><th>名称</th><th>类型</th><th>地址</th><th>探测</th></tr>{egress_rows}</table>

<div class="savebar"><button onclick="save()">保存配置</button><span id="toast"></span></div>

<h2>节点</h2>
<div class="legend">图例:<b style="color:#58a6ff">蓝=本机</b> · <b style="color:#39d353">绿=直连正常</b> · <b style="color:#e3b341">黄=经其它节点可达</b> · <b style="color:#f85149">红=不可达(含原因)</b></div>
<table><tr><th>节点</th><th>ID</th><th>地址</th><th>状态</th><th>延迟 / 原因</th><th>直连数</th><th>外网</th><th>出口代理</th><th>可直达</th></tr>{nodes}</table>

<h2>本节点路由</h2>
<table><tr><th>目标</th><th>跳数</th><th>路径</th></tr>{routes}</table>

<h2>全网可用出口代理</h2>
<table><tr><th>节点</th><th>名称</th><th>类型</th><th>地址</th><th>范围</th></tr>{caps}</table>

<script>
const CFG = {cfg_json};
const q=id=>document.getElementById(id);
const val=id=>{{const v=q(id).value.trim();return v===''?null:v;}};
function addPeer(){{const t=q('peers');const r=t.insertRow(-1);r.innerHTML='<td><input class="pc-address"></td><td><input class="pc-name"></td><td><input class="pc-user"></td><td><input class="pc-pass" type="password"></td><td><button class="del">×</button></td>';}}
function addProxy(){{const t=q('proxies');const r=t.insertRow(-1);r.innerHTML='<td><input class="px-name"></td><td><input class="px-kind"></td><td><input class="px-url"></td><td><button class="del">×</button></td>';}}
document.addEventListener('click',e=>{{if(e.target.classList.contains('del'))e.target.closest('tr').remove();}});
function collectPeers(){{const out=[];document.querySelectorAll('#peers tr').forEach((tr,i)=>{{if(i===0)return;const a=tr.querySelector('.pc-address');if(!a||!a.value.trim())return;out.push({{address:a.value.trim(),name:tr.querySelector('.pc-name').value.trim()||null,username:tr.querySelector('.pc-user').value.trim()||null,password:tr.querySelector('.pc-pass').value||null}});}});return out;}}
function collectProxies(){{const out=[];document.querySelectorAll('#proxies tr').forEach((tr,i)=>{{if(i===0)return;const n=tr.querySelector('.px-name');if(!n||!n.value.trim())return;out.push({{name:n.value.trim(),kind:tr.querySelector('.px-kind').value.trim()||'http',url:tr.querySelector('.px-url').value.trim()}});}});return out;}}
function save(){{
  if(!confirm('确定要保存配置吗?保存后立即写入文件并生效。')) return;
  const cfg=JSON.parse(JSON.stringify(CFG));
  cfg.name=val('f-name'); cfg.ip=val('f-ip');
  cfg.api_username=val('f-apiuser'); cfg.api_password=val('f-apipass');
  cfg.admin_username=val('f-adminuser'); cfg.admin_password=val('f-adminpass');
  cfg.internet=q('f-internet').checked;
  const gi=val('f-gossip'); cfg.gossip_interval_secs=gi?parseInt(gi):null;
  cfg.reachable=q('f-reachable').value.split('\n').map(s=>s.trim()).filter(Boolean);
  cfg.peers=collectPeers(); cfg.proxies=collectProxies();
  q('toast').textContent='保存中…';
  fetch('/api/config',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify(cfg)}})
    .then(r=>r.json().then(j=>({{ok:r.ok,j}}))).then(({{ok,j}})=>{{q('toast').textContent=ok?'已保存 ✓':('错误: '+JSON.stringify(j));}})
    .catch(e=>{{q('toast').textContent='保存失败: '+e;}});
}}
async function run(){{
  const out=q('out'); out.textContent='请求中…';
  const url=q('url').value; const via=q('via').value||'auto';
  try{{
    const r=await fetch('/proxy?target='+encodeURIComponent(url)+'&via='+encodeURIComponent(via));
    const served=r.headers.get('x-tiny-station-served-by')||'', path=r.headers.get('x-tiny-station-path')||'', err=r.headers.get('x-tiny-station-error')||'';
    const body=await r.text();
    out.textContent='HTTP '+r.status+'  由节点处理='+served+'  路径='+path+(err?'\n错误='+err:'')+'\n\n'+body.slice(0,6000);
  }}catch(e){{out.textContent='请求失败: '+e;}}
}}
setInterval(()=>{{fetch('/api/status').then(r=>r.json()).catch(()=>{{}})}},5000);
function kToast(m){{document.getElementById('k-toast').textContent=m;}}
async function kernelApply(bc){{
  const code=document.getElementById('k-code').value;
  if(!code.trim()){{kToast('脚本为空');return;}}
  if(bc&&!confirm('将把该脚本推送到全网所有节点并立即生效,确定?'))return;
  kToast('升级中…');
  try{{
    const r=await fetch('/api/kernel?broadcast='+bc,{{method:'POST',headers:{{'content-type':'text/plain'}},body:code}});
    const t=await r.text();
    kToast(r.ok?('已升级 ✓ '+(bc?'已推送全网':'仅本机')):('失败: '+t));
    if(r.ok)setTimeout(()=>location.reload(),800);
  }}catch(e){{kToast('失败: '+e);}}
}}
async function kernelLoad(){{
  try{{const r=await fetch('/api/kernel/source');document.getElementById('k-code').value=r.ok?await r.text():'';if(!r.ok)kToast('尚无脚本');}}
  catch(e){{kToast(''+e);}}
}}
async function kernelDelete(){{
  if(!confirm('卸载内核,回退内置行为?'))return;
  await fetch('/api/kernel',{{method:'DELETE'}});
  kToast('已卸载'); setTimeout(()=>location.reload(),600);
}}
</script>
</body></html>"#,
        name = esc(&s.self_node.name),
        version = esc(&s.version),
        id = esc(&s.self_node.id),
        ip = esc(&s.self_node.ip),
        port = s.listen_port,
        portinfo = portinfo,
        ports = s.ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(","),
        uptime = s.uptime_secs,
        f_name = opt(&cfg.name),
        f_ip = opt(&cfg.ip),
        f_apiuser = opt(&cfg.api_username),
        f_apipass = opt(&cfg.api_password),
        f_adminuser = opt(&cfg.admin_username),
        f_adminpass = opt(&cfg.admin_password),
        f_internet = if cfg.internet { "checked" } else { "" },
        f_gossip = cfg
            .gossip_interval_secs
            .map(|g| g.to_string())
            .unwrap_or_default(),
        f_reachable = reachable_text,
        peer_rows = peer_rows,
        proxy_rows = proxy_rows,
        nodes = nodes,
        routes = routes,
        caps = caps,
        egress_rows = egress_rows,
        cfg_json = cfg_json,
        kernel_status = kernel_status,
    )
}
