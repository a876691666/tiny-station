// tiny-station 数据面客户端(Node 18+,内置 fetch,无第三方依赖)。
//
// 所有参数都放在 JSON body 里,不占用 query,因此没有 URL 长度限制。
//
//   NODE=http://192.168.1.10:45910 TS_USER=mesh TS_PASS=123456 node request.js
//
// 作为库使用:
//
//   const { TinyStation } = require("./request");
//   const ts = new TinyStation("http://192.168.1.10:45910", { username: "mesh", password: "123456" });
//
//   const r = await ts.get("https://api.example.com/v1/items?page=2");
//   const data = r.raiseForStatus().json();
//
//   const r2 = await ts.post("http://192.168.1.66:8080/api/orders", {
//     json: { sku: "A-1", qty: 2 },
//     headers: { authorization: "Bearer target-token" },
//   });
//   console.log(r2.status, r2.servedBy, r2.path.join(" -> "));

class TinyStationError extends Error {}

class TinyStation {
  /**
   * @param {string} node        节点地址,如 http://192.168.1.10:45910
   * @param {{username?: string, password?: string, timeout?: number}} [opts]
   */
  constructor(node, { username = "mesh", password = "123456", timeout = 120000 } = {}) {
    this.node = node.replace(/\/+$/, "");
    this.timeout = timeout;
    this.auth = "Basic " + Buffer.from(`${username}:${password}`).toString("base64");
  }

  /**
   * 发送一个请求,返回带 body/headers 及便捷方法的响应对象。
   * @returns {Promise<{ok:boolean,status:number,headers:object,cookies:string[],
   *   bytes:Buffer,text:string,json:()=>any,servedBy:string,path:string[],
   *   elapsedMs:number,egress?:string,error?:string,raiseForStatus:()=>any}>}
   */
  async request(url, { method = "GET", headers, body, json, via = "auto", egress, timeoutMs, mode } = {}) {
    if (json !== undefined) body = json;
    const payload = { url, method, via };
    if (headers) payload.headers = { ...headers };
    if (body !== undefined) payload.body = body;
    if (egress) payload.egress = egress;
    if (timeoutMs) payload.timeout_ms = timeoutMs;
    if (mode) payload.mode = mode;

    const res = await fetch(`${this.node}/api/request`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: this.auth },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(this.timeout),
    });

    const env = await res.json().catch(async () => {
      throw new TinyStationError(`node HTTP ${res.status}: ${await res.text()}`);
    });

    const bytes = Buffer.from(env.body ?? "", "base64");
    const text = () => bytes.toString("utf8");
    return {
      ok: env.ok,
      status: env.status,
      headers: env.headers ?? {},
      cookies: env.cookies ?? [],
      bytes,
      get text() {
        return text();
      },
      json: () => JSON.parse(text()),
      servedBy: env.served_by,
      path: env.path ?? [],
      elapsedMs: env.elapsed_ms ?? 0,
      egress: env.egress,
      error: env.error,
      raiseForStatus() {
        if (!env.ok) throw new TinyStationError(env.error ?? `mesh error (status ${env.status})`);
        if (env.status >= 400) throw new TinyStationError(`upstream HTTP ${env.status}: ${text().slice(0, 200)}`);
        return this;
      },
    };
  }

  get(url, opts) {
    return this.request(url, { ...opts, method: "GET" });
  }

  post(url, opts) {
    return this.request(url, { ...opts, method: "POST" });
  }

  put(url, opts) {
    return this.request(url, { ...opts, method: "PUT" });
  }

  delete(url, opts) {
    return this.request(url, { ...opts, method: "DELETE" });
  }
}

module.exports = { TinyStation, TinyStationError };

if (require.main === module) {
  (async () => {
    const ts = new TinyStation(process.env.NODE ?? "http://127.0.0.1:45910", {
      username: process.env.TS_USER ?? "mesh",
      password: process.env.TS_PASS ?? "123456",
    });
    const r = await ts.get(process.env.URL ?? "http://127.0.0.1:8000/");
    console.log(`HTTP ${r.status} · servedBy=${r.servedBy} · ${r.elapsedMs}ms`);
    if (r.path.length) console.log("path:", r.path.join(" -> "));
    if (!r.ok) console.log("error:", r.error);
    console.log(r.text.slice(0, 400));
  })().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
