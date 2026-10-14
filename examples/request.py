#!/usr/bin/env python3
"""tiny-station 数据面客户端(仅标准库,无第三方依赖)。

所有参数都放在 JSON body 里,不占用 query,因此没有 URL 长度限制。

    NODE=http://192.168.1.10:45910 TS_USER=mesh TS_PASS=123456 python3 request.py

作为库使用::

    from request import TinyStation

    ts = TinyStation("http://192.168.1.10:45910", "mesh", "123456")

    # GET(默认)
    r = ts.get("https://api.example.com/v1/items?page=2")
    data = r.raise_for_status().json()

    # POST JSON:body 传字典即可,自动序列化;headers 里的 authorization 发给目标
    r = ts.post(
        "http://192.168.1.66:8080/api/orders",
        json={"sku": "A-1", "qty": 2},
        headers={"authorization": "Bearer target-token"},
        via="auto",
    )
    print(r.status, r.served_by, " -> ".join(r.path))
"""
from __future__ import annotations

import base64
import json as _json
import os
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Mapping, Optional


class TinyStationError(RuntimeError):
    """连接节点失败,或 mesh 无法完成请求。"""


@dataclass
class Response:
    ok: bool
    status: int
    headers: dict[str, str] = field(default_factory=dict)
    cookies: list[str] = field(default_factory=list)
    body: bytes = b""
    served_by: str = ""
    path: list[str] = field(default_factory=list)
    elapsed_ms: int = 0
    egress: Optional[str] = None
    error: Optional[str] = None

    @property
    def text(self) -> str:
        return self.body.decode("utf-8", "replace")

    def json(self) -> Any:
        return _json.loads(self.body)

    def raise_for_status(self) -> "Response":
        """mesh 出错或上游返回 >=400 时抛 TinyStationError。"""
        if not self.ok:
            raise TinyStationError(self.error or f"mesh error (status {self.status})")
        if self.status >= 400:
            raise TinyStationError(f"upstream HTTP {self.status}: {self.text[:200]}")
        return self

    def __repr__(self) -> str:
        return f"<Response {self.status} via {self.served_by or '?'}>"


class TinyStation:
    """通过一个 tiny-station 节点把请求路由到全网。"""

    def __init__(
        self,
        node: str,
        username: str = "mesh",
        password: str = "123456",
        timeout: float = 120.0,
    ) -> None:
        self.node = node.rstrip("/")
        self.timeout = timeout
        token = base64.b64encode(f"{username}:{password}".encode()).decode()
        self._auth = f"Basic {token}"

    def request(
        self,
        url: str,
        method: str = "GET",
        *,
        headers: Optional[Mapping[str, str]] = None,
        body: Any = None,
        json: Any = None,
        via: str = "auto",
        egress: Optional[str] = None,
        timeout_ms: Optional[int] = None,
        mode: Optional[str] = None,
    ) -> Response:
        """发送一个请求,返回 :class:`Response`(body 已解码)。"""
        if json is not None:
            body = json
        payload: dict[str, Any] = {"url": url, "method": method, "via": via}
        if headers:
            payload["headers"] = dict(headers)
        if body is not None:
            payload["body"] = body
        if egress:
            payload["egress"] = egress
        if timeout_ms:
            payload["timeout_ms"] = timeout_ms
        if mode:
            payload["mode"] = mode

        req = urllib.request.Request(
            f"{self.node}/api/request",
            data=_json.dumps(payload).encode(),
            headers={"content-type": "application/json", "authorization": self._auth},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                env = _json.load(resp)
        except urllib.error.HTTPError as e:  # 节点本身返回的错误(401/400/...)
            detail = e.read().decode("utf-8", "replace")
            raise TinyStationError(f"node HTTP {e.code}: {detail}") from e
        return Response(
            ok=env["ok"],
            status=env["status"],
            headers=env.get("headers", {}),
            cookies=env.get("cookies", []),
            body=base64.b64decode(env["body"]),
            served_by=env.get("served_by", ""),
            path=env.get("path", []),
            elapsed_ms=env.get("elapsed_ms", 0),
            egress=env.get("egress"),
            error=env.get("error"),
        )

    def get(self, url: str, **kw: Any) -> Response:
        return self.request(url, "GET", **kw)

    def post(self, url: str, **kw: Any) -> Response:
        return self.request(url, "POST", **kw)

    def put(self, url: str, **kw: Any) -> Response:
        return self.request(url, "PUT", **kw)

    def delete(self, url: str, **kw: Any) -> Response:
        return self.request(url, "DELETE", **kw)


def _main() -> None:
    ts = TinyStation(
        os.environ.get("NODE", "http://127.0.0.1:45910"),
        os.environ.get("TS_USER", "mesh"),
        os.environ.get("TS_PASS", "123456"),
    )
    r = ts.get(os.environ.get("URL", "http://127.0.0.1:8000/"))
    print(f"HTTP {r.status} · served_by={r.served_by} · {r.elapsed_ms}ms")
    if r.path:
        print("path:", " -> ".join(r.path))
    if not r.ok:
        print("error:", r.error)
    print(r.text[:400])


if __name__ == "__main__":
    _main()
