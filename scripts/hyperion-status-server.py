#!/usr/bin/env python3
"""Small public-safe status page for a private PulseVM/Hyperion test stack."""

from __future__ import annotations

import html
import json
import os
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


HTTP_BIND = os.environ.get("HYPERION_STATUS_BIND", "127.0.0.1")
HTTP_PORT = int(os.environ.get("HYPERION_STATUS_PORT", "8080"))
PULSEVM_RPC_URL = os.environ.get("PULSEVM_RPC_URL", "")
HYPERION_API_URL = os.environ.get("HYPERION_API_URL", "http://127.0.0.1:7000").rstrip("/")
RUNTIME_PATH = Path(
    os.environ.get("HYPERION_RUNTIME_PATH", "/data/pulsevm-hyperion-test/runtime.json")
)
REPLAY_REPORT_PATH = Path(
    os.environ.get("PULSEVM_REPLAY_REPORT_PATH", "/data/pulsevm-showcase/replay.json")
)
REQUEST_TIMEOUT = float(os.environ.get("HYPERION_STATUS_REQUEST_TIMEOUT", "3"))
MAX_HEAD_LAG = int(os.environ.get("HYPERION_MAX_HEAD_LAG", "20"))
STATUS_CACHE_SECONDS = float(os.environ.get("HYPERION_STATUS_CACHE_SECONDS", "2"))
STATUS_CACHE_LOCK = threading.Lock()
STATUS_CACHE: tuple[float, dict[str, Any]] | None = None


def read_json(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
        return value if isinstance(value, dict) else None
    except (OSError, json.JSONDecodeError):
        return None


def request_json(url: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
    data = None
    headers: dict[str, str] = {}
    if body is not None:
        data = json.dumps(body, separators=(",", ":")).encode()
        headers["content-type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
        value = json.load(response)
    if not isinstance(value, dict):
        raise ValueError("endpoint returned a non-object JSON value")
    return value


def service(health: dict[str, Any], name: str) -> dict[str, Any] | None:
    services = health.get("health")
    if not isinstance(services, list):
        return None
    return next(
        (
            item
            for item in services
            if isinstance(item, dict) and item.get("service") == name
        ),
        None,
    )


def refresh_status() -> dict[str, Any]:
    result: dict[str, Any] = {
        "status": "degraded",
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "replay": read_json(REPLAY_REPORT_PATH),
        "verification": None,
        "pulsevm": {"status": "unavailable"},
        "hyperion": {"status": "unavailable"},
    }
    runtime = read_json(RUNTIME_PATH)
    if runtime is not None:
        result["verification"] = runtime.get("verification")

    pulse_info: dict[str, Any] | None = None
    if PULSEVM_RPC_URL:
        try:
            envelope = request_json(
                PULSEVM_RPC_URL,
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "pulsevm.getInfo",
                    "params": [],
                },
            )
            value = envelope.get("result")
            if isinstance(value, dict):
                pulse_info = value
                result["pulsevm"] = {
                    "status": "ok",
                    "chain_id": value.get("chain_id"),
                    "head_block_num": value.get("head_block_num"),
                    "last_irreversible_block_num": value.get(
                        "last_irreversible_block_num"
                    ),
                }
        except (OSError, ValueError, urllib.error.URLError) as error:
            result["pulsevm"] = {"status": "unavailable", "error": str(error)}

    try:
        health = request_json(f"{HYPERION_API_URL}/v2/health")
        elastic = service(health, "Elasticsearch")
        rpc = service(health, "PulseVM-RPC")
        indexer = service(health, "Indexer")
        index_data = indexer.get("service_data", {}) if indexer else {}
        indexed = index_data.get("last_indexed_block")
        head = pulse_info.get("head_block_num") if pulse_info else None
        lag = head - indexed if isinstance(head, int) and isinstance(indexed, int) else None
        healthy = (
            elastic is not None
            and elastic.get("status") == "OK"
            and rpc is not None
            and rpc.get("status") == "OK"
            and isinstance(indexed, int)
            and indexed > 0
            and isinstance(lag, int)
            and 0 <= lag <= MAX_HEAD_LAG
        )
        result["hyperion"] = {
            "status": "ok" if healthy else "catching_up",
            "chain": health.get("chain"),
            "last_indexed_block": indexed,
            "head_block_num": head,
            "head_lag": lag,
            "elasticsearch": elastic.get("status") if elastic else "Error",
        }
        if healthy and result["pulsevm"].get("status") == "ok":
            result["status"] = "ok"
    except (OSError, ValueError, urllib.error.URLError) as error:
        result["hyperion"] = {"status": "unavailable", "error": str(error)}
    return result


def current_status() -> dict[str, Any]:
    global STATUS_CACHE
    now = time.monotonic()
    cached = STATUS_CACHE
    if cached is not None and now - cached[0] < STATUS_CACHE_SECONDS:
        return cached[1]
    with STATUS_CACHE_LOCK:
        cached = STATUS_CACHE
        now = time.monotonic()
        if cached is not None and now - cached[0] < STATUS_CACHE_SECONDS:
            return cached[1]
        value = refresh_status()
        STATUS_CACHE = (now, value)
        return value


def page() -> bytes:
    title = html.escape(os.environ.get("HYPERION_STATUS_TITLE", "PulseVM Hyperion Test"))
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title><style>
:root{{--bg:#07111f;--card:#102139;--text:#e8f2ff;--muted:#93a9c3;--ok:#36d399;--warn:#fbbd23}}
*{{box-sizing:border-box}}body{{margin:0;background:linear-gradient(145deg,#07111f,#0c1b31);color:var(--text);font:16px system-ui,sans-serif;min-height:100vh}}
main{{max-width:900px;margin:auto;padding:48px 20px}}h1{{font-size:clamp(30px,6vw,54px);margin:0 0 8px}}.sub{{color:var(--muted);margin-bottom:32px}}
.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(230px,1fr));gap:16px}}.card{{background:rgba(16,33,57,.9);border:1px solid #284363;border-radius:16px;padding:20px}}
.label{{color:var(--muted);font-size:13px;text-transform:uppercase;letter-spacing:.12em}}.value{{font-size:28px;font-weight:700;margin-top:8px;word-break:break-word}}
.ok{{color:var(--ok)}}.warn{{color:var(--warn)}}pre{{white-space:pre-wrap;color:var(--muted);font-size:12px;margin-top:28px}}a{{color:#8bc4ff}}</style></head>
<body><main><h1>{title}</h1><div class="sub">Live PulseVM → SHiP → Hyperion → Elasticsearch verification</div>
<div class="grid"><div class="card"><div class="label">Pipeline</div><div class="value" id="status">Loading…</div></div>
<div class="card"><div class="label">PulseVM head</div><div class="value" id="head">—</div></div>
<div class="card"><div class="label">Hyperion indexed</div><div class="value" id="indexed">—</div></div>
<div class="card"><div class="label">Head lag</div><div class="value" id="lag">—</div></div>
<div class="card"><div class="label">Historical replay</div><div class="value" id="replay">—</div></div>
<div class="card"><div class="label">Last verification</div><div class="value" id="verified">—</div></div></div>
<pre id="updated"></pre><script>
const n=x=>Number.isInteger(x)?x.toLocaleString():"—";
async function refresh(){{try{{const r=await fetch('/api/status',{{cache:'no-store'}}),d=await r.json();
let s=document.getElementById('status');s.textContent=d.status==='ok'?'Healthy':'Catching up';s.className='value '+(d.status==='ok'?'ok':'warn');
document.getElementById('head').textContent=n(d.pulsevm?.head_block_num);document.getElementById('indexed').textContent=n(d.hyperion?.last_indexed_block);
document.getElementById('lag').textContent=n(d.hyperion?.head_lag);document.getElementById('replay').textContent=d.replay?.verified_through?n(d.replay.verified_through):'—';
document.getElementById('verified').textContent=d.verification?.status==='passed'?'Passed':'Pending';document.getElementById('updated').textContent='Updated '+d.updated_at;}}catch(e){{document.getElementById('status').textContent='Unavailable'}}}}
refresh();setInterval(refresh,3000);</script></main></body></html>""".encode()


class Handler(BaseHTTPRequestHandler):
    def send_body(self, status: int, content_type: str, body: bytes) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", "default-src 'self'; style-src 'unsafe-inline' 'self'; script-src 'unsafe-inline' 'self'")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        if self.path == "/":
            self.send_body(200, "text/html; charset=utf-8", page())
            return
        if self.path in ("/api/status", "/health"):
            payload = current_status()
            body = (json.dumps(payload, separators=(",", ":")) + "\n").encode()
            self.send_body(
                200 if payload["status"] == "ok" else 503,
                "application/json",
                body,
            )
            return
        self.send_error(404)

    def log_message(self, format: str, *args: object) -> None:
        return


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


if __name__ == "__main__":
    Server((HTTP_BIND, HTTP_PORT), Handler).serve_forever()
