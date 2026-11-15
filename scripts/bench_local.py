#!/usr/bin/env python3
"""Run a local ARM64/AMD64, binary/image, LocalFS/MinIO benchmark matrix."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import html
import json
import math
import platform
import re
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "bench" / "results"
K6_IMAGE = "grafana/k6:latest"
MINIO_IMAGE = "pgsty/minio:latest"
RUNTIME_IMAGE = "gcr.io/distroless/cc-debian12:nonroot"
API_KEY = "local-benchmark-only-key"
ACCESS_KEY = "bench-minio-user"
SECRET_KEY = "bench-minio-secret-123"
BUCKET = "image-service-bench"
ARCHES = ("arm64", "amd64")
MODES = ("binary", "docker")
STORAGES = ("localfs", "minio")


def run(cmd: list[str], *, check: bool = True, capture: bool = True, timeout: int | None = None):
    return subprocess.run(
        cmd,
        cwd=ROOT,
        check=check,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
        timeout=timeout,
    )


def docker(*args: str, check: bool = True, capture: bool = True, timeout: int | None = None):
    return run(["docker", *args], check=check, capture=capture, timeout=timeout)


def parse_size(value: str) -> int | None:
    match = re.fullmatch(r"\s*([0-9.]+)\s*([KMGT]?i?B)?\s*", value)
    if not match:
        return None
    number = float(match.group(1))
    unit = (match.group(2) or "B").upper()
    scale = {"B": 1, "KB": 1000, "KIB": 1024, "MB": 1000**2, "MIB": 1024**2,
             "GB": 1000**3, "GIB": 1024**3, "TB": 1000**4, "TIB": 1024**4}.get(unit, 1)
    return int(number * scale)


def pair_sizes(value: str) -> tuple[int | None, int | None]:
    parts = value.split(" / ", 1)
    if len(parts) != 2:
        return None, None
    return parse_size(parts[0]), parse_size(parts[1])


def parse_iso_datetime(value: str) -> dt.datetime:
    # k6's fractional precision varies; normalize it to Python microseconds.
    match = re.search(r"\.(\d+)(Z|[+-]\d\d:\d\d)$", value)
    if match:
        fraction = match.group(1)[:6].ljust(6, "0")
        value = value[:match.start()] + f".{fraction}{match.group(2)}"
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def parse_processing_profile(log_text: str) -> list[tuple[int, int, int, int]]:
    samples = []
    for line in log_text.splitlines():
        try:
            event = json.loads(line)
            fields = event.get("fields", event)
            if fields.get("message") != "image processing profile":
                continue
            samples.append((int(fields["queue_wait_us"]), int(fields["decode_us"]),
                            int(fields["compression_us"]), int(fields["processing_us"])))
        except (json.JSONDecodeError, KeyError, TypeError, ValueError):
            continue
    return samples


def stats_collector(names: list[str], output: list[dict], stop: threading.Event):
    proc = subprocess.Popen(
        ["docker", "stats", "--format", "{{json .}}", *names],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )
    try:
        assert proc.stdout is not None
        while not stop.is_set():
            line = proc.stdout.readline()
            if not line:
                break
            try:
                # Docker Desktop emits terminal cursor-control escapes even
                # when stdout is piped; remove them before decoding each JSON row.
                clean_line = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", line).strip()
                if not clean_line.startswith("{"):
                    continue
                row = json.loads(clean_line)
                mem_used, mem_limit = pair_sizes(row.get("MemUsage", ""))
                net_rx, net_tx = pair_sizes(row.get("NetIO", ""))
                block_read, block_write = pair_sizes(row.get("BlockIO", ""))
                output.append({
                    "ts": time.time(),
                    "container": row.get("Name") or row.get("Container"),
                    "cpu_pct": float(row.get("CPUPerc", "0%").rstrip("%")),
                    "mem_used": mem_used,
                    "mem_limit": mem_limit,
                    "net_rx": net_rx,
                    "net_tx": net_tx,
                    "block_read": block_read,
                    "block_write": block_write,
                    "pids": int(row["PIDs"]) if str(row.get("PIDs", "")).isdigit() else None,
                })
            except (ValueError, TypeError, json.JSONDecodeError):
                continue
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()


def wait_http(url: str, *, attempts: int = 60) -> None:
    import urllib.error
    import urllib.request

    for _ in range(attempts):
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status in (200, 204):
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(0.5)
    raise RuntimeError(f"Service did not become healthy: {url}")


def make_k6_script(path: Path, fixture_name: str, stage_seconds: int, vu_levels: tuple[int, ...]) -> None:
    stages = [{"duration": f"{stage_seconds}s", "target": vu} for vu in vu_levels]
    stages.append({"duration": "5s", "target": 0})
    script = f'''import http from "k6/http";
import {{ check }} from "k6";
const image = open("/work/{fixture_name}", "b");
const base = __ENV.BASE_URL;
const token = __ENV.API_KEY;
const runLabel = __ENV.RUN_LABEL;
export const options = {{
  scenarios: {{ stress: {{ executor: "ramping-vus", startVUs: {vu_levels[0]}, stages: {json.dumps(stages)},
    gracefulRampDown: "5s" }} }}
}};
export default function () {{
  // POST is create-only, so each iteration needs a fresh resource/slot. Reusing
  // a slot would turn later iterations into conflicts instead of measuring the
  // successful create path.
  const resource = `bench-${{runLabel}}-vu-${{__VU}}-iter-${{__ITER}}`;
  const url = `${{base}}/api/images/bench/${{resource}}/1`;
  const headers = {{ Authorization: `Bearer ${{token}}` }};
  const form = {{ image: http.file(image, "fixture.png", "image/png") }};
  const post = http.post(url, form, {{ headers, tags: {{ operation: "post_create" }} }});
  check(post, {{ "create-only POST accepted": r => r.status === 201 }});
  // A rejected create has no resource to fetch. Avoid generating a secondary
  // 404 that would obscure the actual admission/transport failure.
  if (post.status !== 201) return;
  const get = http.get(url, {{ headers, tags: {{ operation: "fetch" }} }});
  check(get, {{ "fetch returned image": r => r.status === 200 && r.body.length > 0 }});
}}
'''
    path.write_text(script)


def init_db(db: sqlite3.Connection) -> None:
    db.executescript("""
    PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS runs (
      run_id TEXT PRIMARY KEY, started_at TEXT, ended_at TEXT, git_sha TEXT,
      host_arch TEXT, target_arch TEXT, emulation TEXT, mode TEXT, storage TEXT,
      scenario TEXT, fixture_sha256 TEXT, fixture_bytes INTEGER, docker_version TEXT,
      app_image TEXT, minio_image TEXT, vus_stages TEXT, stage_seconds INTEGER,
      result TEXT, error TEXT
    );
    CREATE TABLE IF NOT EXISTS http_samples (
      id INTEGER PRIMARY KEY, run_id TEXT, ts TEXT, metric TEXT, value REAL,
      operation TEXT, status TEXT, method TEXT
    );
    CREATE INDEX IF NOT EXISTS http_samples_run ON http_samples(run_id, metric, operation);
    CREATE TABLE IF NOT EXISTS resource_samples (
      id INTEGER PRIMARY KEY, run_id TEXT, ts TEXT, container TEXT, cpu_pct REAL,
      mem_used_bytes INTEGER, mem_limit_bytes INTEGER, net_rx_bytes INTEGER,
      net_tx_bytes INTEGER, block_read_bytes INTEGER, block_write_bytes INTEGER, pids INTEGER
    );
    CREATE INDEX IF NOT EXISTS resource_samples_run ON resource_samples(run_id, container, ts);
    CREATE TABLE IF NOT EXISTS summaries (
      run_id TEXT, operation TEXT, requests INTEGER, errors INTEGER, rps REAL,
      mean_ms REAL, p50_ms REAL, p90_ms REAL, p95_ms REAL, p99_ms REAL, max_ms REAL,
      cpu_avg_pct REAL, cpu_max_pct REAL, mem_avg_bytes INTEGER, mem_max_bytes INTEGER,
      PRIMARY KEY(run_id, operation)
    );
    CREATE TABLE IF NOT EXISTS stage_summaries (
      run_id TEXT, stage_vus INTEGER, operation TEXT, requests INTEGER, errors INTEGER,
      rps REAL, p50_ms REAL, p95_ms REAL, p99_ms REAL,
      PRIMARY KEY(run_id, stage_vus, operation)
    );
    CREATE TABLE IF NOT EXISTS processing_samples (
      id INTEGER PRIMARY KEY, run_id TEXT, queue_wait_us INTEGER, decode_us INTEGER,
      compression_us INTEGER, processing_us INTEGER
    );
    CREATE INDEX IF NOT EXISTS processing_samples_run ON processing_samples(run_id);
    """)


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(pct * len(ordered)) - 1)]


def parse_k6(path: Path) -> list[dict]:
    rows = []
    for line in path.read_text().splitlines():
        try:
            item = json.loads(line)
            if item.get("type") != "Point":
                continue
            metric = item.get("metric")
            if metric not in {"http_req_duration", "http_req_failed", "http_reqs", "iteration_duration"}:
                continue
            data = item.get("data", {})
            tags = data.get("tags") or {}
            rows.append({
                "ts": data.get("time"), "metric": metric,
                "value": float(data.get("value", 0)),
                "operation": tags.get("operation", "all"),
                "status": tags.get("status"), "method": tags.get("method"),
            })
        except (json.JSONDecodeError, ValueError, TypeError):
            continue
    return rows


def build_percentile(values: list[float]) -> tuple[float | None, ...]:
    return (sum(values) / len(values) if values else None,
            percentile(values, .50), percentile(values, .90), percentile(values, .95),
            percentile(values, .99), max(values) if values else None)


def create_report(db_path: Path, report_path: Path) -> None:
    db = sqlite3.connect(db_path)
    runs = db.execute("""
      SELECT r.run_id, r.target_arch, r.emulation, r.mode, r.storage, r.result,
        s.operation, s.requests, s.errors, s.rps, s.p50_ms, s.p95_ms, s.p99_ms,
        s.cpu_avg_pct, s.cpu_max_pct, s.mem_avg_bytes, s.mem_max_bytes
      FROM runs r JOIN summaries s USING(run_id)
      ORDER BY r.target_arch, r.mode, r.storage, s.operation
    """).fetchall()
    def mib(value):
        return "—" if value is None else f"{value / 1024 / 1024:.1f} MiB"
    def num(value, suffix=""):
        return "—" if value is None else f"{value:.1f}{suffix}"
    table_rows = []
    for r in runs:
        table_rows.append("<tr>" + "".join(f"<td>{html.escape(str(v))}</td>" for v in (
            r[1], r[2], r[3], r[4], r[6], r[7], r[8], num(r[9]), num(r[10], " ms"),
            num(r[11], " ms"), num(r[12], " ms"), num(r[13], "%"), num(r[14], "%"),
            mib(r[15]), mib(r[16]))) + "</tr>")
    chart_rows = db.execute("""
      SELECT r.target_arch, r.mode, r.storage, s.operation, s.p95_ms, s.cpu_max_pct, s.mem_max_bytes
      FROM runs r JOIN summaries s USING(run_id) ORDER BY r.target_arch, r.mode, r.storage, s.operation
    """).fetchall()
    stage_rows = db.execute("""
      SELECT r.target_arch, r.emulation, r.mode, r.storage, r.scenario, r.vus_stages,
        s.stage_vus, s.operation, s.requests, s.errors, s.rps, s.p50_ms, s.p95_ms, s.p99_ms
      FROM runs r JOIN stage_summaries s USING(run_id)
      ORDER BY r.started_at, s.stage_vus, s.operation
    """).fetchall()
    stage_table_rows = []
    for r in stage_rows:
        levels = [int(x) for x in r[5].split(",")]
        index = levels.index(r[6])
        from_vus = levels[0] if index == 0 else levels[index - 1]
        stage_label = f"{from_vus}→{r[6]}"
        stage_table_rows.append("<tr>" + "".join(f"<td>{html.escape(str(v))}</td>" for v in (
            r[0], r[1], r[2], r[3], stage_label, r[7], r[8], r[9], num(r[10]),
            num(r[11], " ms"), num(r[12], " ms"), num(r[13], " ms"))) + "</tr>")
    processing_rows = db.execute("""
      SELECT r.target_arch, r.mode, r.storage, r.scenario,
        p.queue_wait_us, p.decode_us, p.compression_us, p.processing_us
      FROM runs r JOIN processing_samples p USING(run_id)
      ORDER BY r.started_at
    """).fetchall()
    processing_groups: dict[tuple, list[tuple[int, ...]]] = {}
    for row in processing_rows:
        processing_groups.setdefault(row[:4], []).append(row[4:])
    processing_table_rows = []
    for (arch, mode, storage, scenario), samples in processing_groups.items():
        columns = list(zip(*samples))
        averages = [sum(values) / len(values) for values in columns]
        p95s = [percentile(list(values), .95) for values in columns]
        processing_table_rows.append("<tr>" + "".join(f"<td>{html.escape(str(v))}</td>" for v in (
            arch, mode, storage, scenario, len(samples), num(averages[0] / 1000, " ms"),
            num(p95s[1] / 1000, " ms"), num(p95s[2] / 1000, " ms"),
            num(p95s[3] / 1000, " ms"))) + "</tr>")
    resource_rows = db.execute("""
      SELECT r.run_id, r.target_arch, r.emulation, r.mode, r.storage, r.started_at,
        r.stage_seconds, s.ts, s.cpu_pct, s.mem_used_bytes, s.container
      FROM runs r JOIN resource_samples s USING(run_id)
      ORDER BY r.started_at, s.ts
    """).fetchall()
    def timeseries_svg(value_index: int, unit: str, divisor: float = 1, role: str = "app") -> str:
        groups: dict[str, dict] = {}
        for row in resource_rows:
            run_id, arch, emulation, mode, storage, started, stage, stamp, cpu, memory, container = row
            is_minio = str(container).startswith("imgbench-minio-")
            if is_minio != (role == "minio"):
                continue
            group = groups.setdefault(run_id, {
                "label": f"{arch}/{mode}/{storage}/{role}", "started": dt.datetime.fromisoformat(started),
                "stage": stage, "points": []
            })
            value = (cpu if value_index == 8 else memory)
            if value is None:
                continue
            seconds = (dt.datetime.fromisoformat(stamp) - group["started"]).total_seconds()
            group["points"].append((seconds, value / divisor))
        width, height = 900, 350
        left, right, top, bottom = 55, 870, 20, 285
        duration = max((g["stage"] * 4 + 5 for g in groups.values()), default=65)
        max_value = max((v for g in groups.values() for _, v in g["points"]), default=1) * 1.1
        colors = ["#1769aa", "#e07a1f", "#378c55", "#a23e48", "#7953a9", "#1c9b9b", "#ca4f9c", "#7a6a43"]
        svg = [f'<svg viewBox="0 0 {width} {height}" width="100%" height="{height}">']
        for tick in range(5):
            yval = max_value * tick / 4
            y = bottom - (bottom - top) * tick / 4
            svg.append(f'<line x1="{left}" y1="{y:.1f}" x2="{right}" y2="{y:.1f}" stroke="#d8dee8"/>')
            svg.append(f'<text x="4" y="{y+4:.1f}" font-size="11">{yval:.0f}{unit}</text>')
        for tick in range(5):
            xval = duration * tick / 4
            x = left + (right - left) * tick / 4
            svg.append(f'<line x1="{x:.1f}" y1="{top}" x2="{x:.1f}" y2="{bottom}" stroke="#edf0f4"/>')
            svg.append(f'<text x="{x-10:.1f}" y="{bottom+18}" font-size="11">{xval:.0f}s</text>')
        for idx, group in enumerate(groups.values()):
            coords = []
            for seconds, value in group["points"]:
                x = left + (right - left) * min(1, max(0, seconds / duration))
                y = bottom - (bottom - top) * min(1, max(0, value / max_value))
                coords.append(f"{x:.1f},{y:.1f}")
            color = colors[idx % len(colors)]
            svg.append(f'<polyline fill="none" stroke="{color}" stroke-width="2" points="{" ".join(coords)}"/>')
            lx = left + (idx % 4) * 205
            ly = bottom + 43 + (idx // 4) * 16
            svg.append(f'<line x1="{lx}" y1="{ly-4}" x2="{lx+18}" y2="{ly-4}" stroke="{color}" stroke-width="3"/>')
            svg.append(f'<text x="{lx+23}" y="{ly}" font-size="11">{html.escape(group["label"])}</text>')
        svg.append("</svg>")
        return "".join(svg)
    def chart_svg(value_index: int, color: str, unit: str, divisor: float = 1) -> str:
        vals = [x[value_index] / divisor for x in chart_rows if x[value_index] is not None]
        scale = max(vals or [1])
        bars = []
        for i, row in enumerate(chart_rows):
            label = f"{row[0]}/{row[1]}/{row[2]}/{row[3]}"
            value = row[value_index] / divisor if row[value_index] is not None else None
            width = 500 * (value or 0) / scale
            y = i * 28 + 18
            bars.append(f'<text x="0" y="{y+12}" font-size="11">{html.escape(label)}</text>'
                        f'<rect x="190" y="{y}" width="{width:.1f}" height="17" fill="{color}"/>'
                        f'<text x="{195+width:.1f}" y="{y+12}" font-size="10">{num(value, unit)}</text>')
        height = max(80, len(bars) * 28 + 25)
        return f'<svg viewBox="0 0 760 {height}" width="100%" height="{height}">{"".join(bars)}</svg>'
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(f'''<!doctype html><meta charset="utf-8"><title>Image Service local benchmarks</title>
<style>body{{font:14px system-ui;margin:2rem;color:#20242b}}h1,h2{{color:#172b4d}}table{{border-collapse:collapse;width:100%;font-size:12px}}th,td{{border:1px solid #ccd2dc;padding:6px;text-align:right}}th{{background:#eef2f7}}td:nth-child(-n+5){{text-align:left}}.note{{max-width:1000px;background:#fff7d6;padding:12px}}</style>
<h1>Image Service — benchmarks locales</h1>
<p>Resultados consultados directamente desde <code>{html.escape(str(db_path))}</code>.</p>
<div class="note">AMD64 se ejecuta emulado en un host ARM64. El modo binary ejecuta el artefacto Linux exportado dentro de la imagen Distroless montándolo como archivo; el modo docker usa la imagen runtime del proyecto. LocalFS y MinIO usan almacenamiento efímero. Estas cifras comparan esta máquina/configuración, no equivalen a servidores AMD64 nativos.</div>
<h2>p95 de latencia por variante y operación</h2>{chart_svg(4, "#4776c5", " ms")}
<h2>CPU máxima del contenedor de Image Service</h2>{chart_svg(5, "#e08b36", "%")}
<h2>Uso máximo de memoria del contenedor de Image Service</h2>{chart_svg(6, "#39a987", " MiB", 1024 * 1024)}
<h2>CPU de Image Service a lo largo de la carga</h2>{timeseries_svg(8, "%")}
<h2>Memoria de Image Service a lo largo de la carga</h2>{timeseries_svg(9, " MiB", 1024 * 1024)}
<h2>CPU de MinIO a lo largo de la carga</h2>{timeseries_svg(8, "%", role="minio")}
<h2>Memoria de MinIO a lo largo de la carga</h2>{timeseries_svg(9, " MiB", 1024 * 1024, role="minio")}
<h2>Detalle por ejecución</h2><div style="overflow:auto"><table><thead><tr><th>Arch.</th><th>Ejecución</th><th>Modo</th><th>Storage</th><th>Operación</th><th>Req.</th><th>Errores</th><th>RPS</th><th>p50</th><th>p95</th><th>p99</th><th>CPU avg</th><th>CPU max</th><th>RAM avg</th><th>RAM max</th></tr></thead><tbody>{''.join(table_rows)}</tbody></table></div>
<h2>Resumen por tramo de carga</h2><p>Cada intervalo ramping-vus aumenta linealmente hacia el objetivo indicado; no es una meseta estable de usuarios.</p><div style="overflow:auto"><table><thead><tr><th>Arch.</th><th>Ejecución</th><th>Modo</th><th>Storage</th><th>Rampa VUs</th><th>Operación</th><th>Req.</th><th>Errores</th><th>RPS</th><th>p50</th><th>p95</th><th>p99</th></tr></thead><tbody>{''.join(stage_table_rows)}</tbody></table></div>
<h2>Perfil interno de procesamiento (solo ejecuciones instrumentadas)</h2><div style="overflow:auto"><table><thead><tr><th>Arch.</th><th>Modo</th><th>Storage</th><th>Ejecución</th><th>Imágenes</th><th>Espera media</th><th>Decode p95</th><th>Compresión p95</th><th>CPU total p95</th></tr></thead><tbody>{''.join(processing_table_rows)}</tbody></table></div>
<p>SQLite contiene muestras HTTP y de recursos por timestamp. Repite cada variante 3 veces para comparar medianas, y conserva la misma configuración de Docker Desktop.</p>''')
    db.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, default=ROOT / "TEST_IMAGE_ORIGINAL.png")
    parser.add_argument("--db", type=Path, default=RESULTS / "benchmarks.sqlite")
    parser.add_argument("--report", type=Path, default=RESULTS / "latest.html")
    parser.add_argument("--stage-seconds", type=int, default=15,
                        help="duration in seconds for each ramp interval")
    parser.add_argument("--vu-levels", default="1,4,8,16",
                        help="comma-separated ramp targets, for example 1,8,32,64")
    parser.add_argument("--repeats", type=int, default=1,
                        help="repeat each matrix combination this many times")
    parser.add_argument("--concurrency", type=int, default=2,
                        help="IMAGE_MAX_CONCURRENCY for each app container (1..64)")
    parser.add_argument("--profile-processing", action="store_true",
                        help="capture internal queue, decode, and compression timings")
    parser.add_argument("--arches", default="arm64,amd64")
    parser.add_argument("--modes", default="binary,docker")
    parser.add_argument("--storages", default="localfs,minio")
    args = parser.parse_args()
    fixture = args.fixture.resolve()
    if not fixture.is_file():
        raise SystemExit(f"Fixture not found: {fixture}")
    if args.stage_seconds < 1:
        raise SystemExit("--stage-seconds must be >= 1")
    vu_levels = tuple(int(x.strip()) for x in args.vu_levels.split(",") if x.strip())
    if not vu_levels or any(x < 1 or x > 128 for x in vu_levels):
        raise SystemExit("--vu-levels must contain values from 1 to 128")
    if args.repeats < 1 or args.repeats > 5:
        raise SystemExit("--repeats must be between 1 and 5")
    if args.concurrency < 1 or args.concurrency > 64:
        raise SystemExit("--concurrency must be between 1 and 64")
    arches = tuple(x.strip() for x in args.arches.split(",") if x.strip())
    modes = tuple(x.strip() for x in args.modes.split(",") if x.strip())
    storages = tuple(x.strip() for x in args.storages.split(",") if x.strip())
    if set(arches) - set(ARCHES) or set(modes) - set(MODES) or set(storages) - set(STORAGES):
        raise SystemExit("Invalid matrix value; supported arches=arm64,amd64; modes=binary,docker; storages=localfs,minio")

    args.db = args.db.resolve()
    args.report = args.report.resolve()
    args.db.parent.mkdir(parents=True, exist_ok=True)
    fixture_hash = hashlib.sha256(fixture.read_bytes()).hexdigest()
    sha = run(["git", "rev-parse", "HEAD"]).stdout.strip()
    host_arch = platform.machine().lower()
    if host_arch in ("aarch64", "arm64"):
        host_arch = "arm64"
    elif host_arch in ("x86_64", "amd64"):
        host_arch = "amd64"
    docker_version = docker("info", "--format", "{{.ServerVersion}} / {{.OperatingSystem}}").stdout.strip()
    db = sqlite3.connect(args.db)
    init_db(db)
    temp = tempfile.TemporaryDirectory(prefix="image-service-bench-")
    work = Path(temp.name)
    tag = uuid.uuid4().hex[:8]
    network = f"imgbench-net-{tag}"
    minio_name = f"imgbench-minio-{tag}"
    minio_port = None
    minio_started = False
    app_names: list[str] = []
    try:
        docker("network", "create", network)
        if "minio" in storages:
            docker("run", "-d", "--rm", "--name", minio_name, "--network", network,
                   "--tmpfs", "/data:rw,nosuid,size=512m,mode=1777",
                   "--publish", "127.0.0.1::9000",
                   "--env", f"MINIO_ROOT_USER={ACCESS_KEY}",
                   "--env", f"MINIO_ROOT_PASSWORD={SECRET_KEY}",
                   MINIO_IMAGE, "server", "/data", "--console-address", ":9001")
            minio_started = True
            minio_port = docker("port", minio_name, "9000/tcp").stdout.strip().rsplit(":", 1)[-1]
            wait_http(f"http://127.0.0.1:{minio_port}/minio/health/live")
            run(["curl", "--fail", "--silent", "--show-error", "--aws-sigv4", "aws:amz:us-east-1:s3",
                 "--user", f"{ACCESS_KEY}:{SECRET_KEY}", "--request", "PUT",
                 f"http://127.0.0.1:{minio_port}/{BUCKET}"])

        binary_paths: dict[str, Path] = {}
        image_tags: dict[str, str] = {}
        for arch in arches:
            platform_tag = f"linux/{arch}"
            if "binary" in modes:
                dest = work / f"binary-{arch}"
                dest.mkdir()
                docker("buildx", "build", "--platform", platform_tag, "--target", "binary",
                       "--output", f"type=local,dest={dest}", ".", capture=False, timeout=1800)
                binary_paths[arch] = dest / "image_service"
            if "docker" in modes:
                image_tag = f"image-service:bench-{arch}-{tag}"
                docker("buildx", "build", "--platform", platform_tag, "--target", "runtime",
                       "--tag", image_tag, "--load", ".", capture=False, timeout=1800)
                image_tags[arch] = image_tag

        for repeat in range(1, args.repeats + 1):
          for arch in arches:
            for mode in modes:
                for storage in storages:
                    run_id = f"{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}-{arch}-{mode}-{storage}-{uuid.uuid4().hex[:6]}"
                    app_name = f"imgbench-app-{tag}"
                    app_names.append(app_name)
                    platform_tag = f"linux/{arch}"
                    app_image = image_tags.get(arch, RUNTIME_IMAGE) if mode == "docker" else RUNTIME_IMAGE
                    port_args = ["--publish", "127.0.0.1::3000"]
                    common = ["run", "-d", "--rm", "--name", app_name, "--platform", platform_tag,
                              "--network", network, *port_args, "--read-only",
                              "--tmpfs", "/tmp:rw,nosuid,size=32m,mode=1777",
                              "--tmpfs", "/data:rw,nosuid,size=1g,uid=65532,gid=65532,mode=0755",
                              "--env", "IMAGE_BIND=0.0.0.0:3000",
                              "--env", f"IMAGE_API_KEY={API_KEY}",
                              "--env", f"IMAGE_MAX_CONCURRENCY={args.concurrency}",
                              "--env", "IMAGE_RATE_LIMIT_READ_REQUESTS=0",
                              "--env", "IMAGE_RATE_LIMIT_WRITE_REQUESTS=0"]
                    if args.profile_processing:
                        common.extend(["--env", "RUST_LOG=image_service=info,image_processing_profile=debug"])
                    if mode == "binary":
                        binary = binary_paths[arch]
                        common.extend(["--mount", f"type=bind,source={binary},target=/image_service,readonly",
                                       "--entrypoint", "/image_service"])
                    if storage == "localfs":
                        common.extend(["--env", "IMAGE_STORAGE_SCHEME=fs",
                                       "--env", "IMAGE_STORAGE_ROOT=/data",
                                       "--env", "IMAGE_STORAGE_ATOMIC_WRITE_DIR=/data/.tmp"])
                    else:
                        common.extend(["--env", "IMAGE_STORAGE_SCHEME=s3",
                                       "--env", f"IMAGE_STORAGE_BUCKET={BUCKET}",
                                       "--env", "IMAGE_STORAGE_REGION=us-east-1",
                                       "--env", f"IMAGE_STORAGE_ENDPOINT=http://{minio_name}:9000",
                                       "--env", "IMAGE_STORAGE_ENABLE_VIRTUAL_HOST_STYLE=false",
                                       "--env", f"IMAGE_STORAGE_ACCESS_KEY_ID={ACCESS_KEY}",
                                       "--env", f"IMAGE_STORAGE_SECRET_ACCESS_KEY={SECRET_KEY}"])
                    common.append(app_image)
                    docker(*common, capture=False)
                    host_port = docker("port", app_name, "3000/tcp").stdout.strip().rsplit(":", 1)[-1]
                    wait_http(f"http://127.0.0.1:{host_port}/health")
                    k6_script = work / "bench.js"
                    output_file = work / f"{run_id}.jsonl"
                    fixture_copy = work / fixture.name
                    if not fixture_copy.exists():
                        shutil.copyfile(fixture, fixture_copy)
                    make_k6_script(k6_script, fixture.name, args.stage_seconds, vu_levels)

                    resources: list[dict] = []
                    stop = threading.Event()
                    monitored = [app_name] + ([minio_name] if storage == "minio" else [])
                    monitor = threading.Thread(target=stats_collector,
                                               args=(monitored, resources, stop), daemon=True)
                    monitor.start()
                    start_wall = time.time()
                    started_at = dt.datetime.now(dt.timezone.utc).isoformat()
                    result = "ok"
                    error = None
                    try:
                        docker("run", "--rm", "--user", "0:0", "--network", network,
                               "--volume", f"{work}:/work", K6_IMAGE, "run", "--quiet",
                               "--out", f"json=/work/{output_file.name}",
                               "--env", f"BASE_URL=http://{app_name}:3000",
                               "--env", f"API_KEY={API_KEY}",
                               "--env", f"RUN_LABEL={run_id.replace('-', '')[:22]}",
                               f"/work/{k6_script.name}", timeout=1200)
                    except subprocess.CalledProcessError as exc:
                        result = "failed"
                        error = (exc.stdout or "")[-3000:]
                        print(f"K6 returned non-zero for {run_id}; importing available samples")
                    finally:
                        stop.set()
                        monitor.join(timeout=5)
                    profile_samples = []
                    if args.profile_processing:
                        logs = docker("logs", app_name, check=False).stdout
                        profile_samples = parse_processing_profile(logs)
                    elapsed = time.time() - start_wall
                    ended_at = dt.datetime.now(dt.timezone.utc).isoformat()
                    points = parse_k6(output_file) if output_file.exists() else []
                    if not points:
                        result = "failed"
                        error = error or "No k6 samples were collected"
                    else:
                        failed_requests = [p for p in points
                                           if p["metric"] == "http_req_failed" and p["value"] > 0]
                        if failed_requests and result == "ok":
                            result = "completed_with_http_errors"
                            error = f"{len(failed_requests)} HTTP requests failed"
                    emulation = "native" if arch == host_arch else "emulated"
                    db.execute("INSERT INTO runs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", (
                        run_id, started_at, ended_at, sha, host_arch, arch, emulation, mode, storage,
                        f"ramp_{'_'.join(map(str, vu_levels))}_c{args.concurrency}_post_then_fetch_repeat_{repeat}", fixture_hash, fixture.stat().st_size,
                        docker_version, app_image, MINIO_IMAGE if minio_started else None,
                        ",".join(map(str, vu_levels)), args.stage_seconds, result, error))
                    db.executemany("""INSERT INTO http_samples
                      (run_id,ts,metric,value,operation,status,method) VALUES (?,?,?,?,?,?,?)""",
                      [(run_id, p["ts"], p["metric"], p["value"], p["operation"], p["status"], p["method"])
                       for p in points])
                    db.executemany("""INSERT INTO resource_samples
                      (run_id,ts,container,cpu_pct,mem_used_bytes,mem_limit_bytes,net_rx_bytes,net_tx_bytes,
                       block_read_bytes,block_write_bytes,pids) VALUES (?,?,?,?,?,?,?,?,?,?,?)""",
                      [(run_id, dt.datetime.fromtimestamp(x["ts"], dt.timezone.utc).isoformat(), x["container"],
                        x["cpu_pct"], x["mem_used"], x["mem_limit"], x["net_rx"], x["net_tx"],
                        x["block_read"], x["block_write"], x["pids"]) for x in resources])
                    db.executemany("""INSERT INTO processing_samples
                      (run_id,queue_wait_us,decode_us,compression_us,processing_us) VALUES (?,?,?,?,?)""",
                      [(run_id, *sample) for sample in profile_samples])
                    operations = ("post_create", "fetch")
                    for operation in operations:
                        op_points = [p for p in points if p["metric"] == "http_req_duration" and p["operation"] == operation]
                        durations = [p["value"] for p in op_points]
                        failed = [p for p in points if p["metric"] == "http_req_failed" and p["operation"] == operation and p["value"] > 0]
                        op_resources = [x for x in resources if x["container"] == app_name]
                        mean, p50, p90, p95, p99, max_ms = build_percentile(durations)
                        cpu_values = [x["cpu_pct"] for x in op_resources]
                        mem_values = [x["mem_used"] for x in op_resources if x["mem_used"] is not None]
                        db.execute("INSERT INTO summaries VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", (
                            run_id, operation, len(durations), len(failed), len(durations) / elapsed if elapsed else 0,
                            mean, p50, p90, p95, p99, max_ms,
                            sum(cpu_values) / len(cpu_values) if cpu_values else None,
                            max(cpu_values) if cpu_values else None,
                            int(sum(mem_values) / len(mem_values)) if mem_values else None,
                            max(mem_values) if mem_values else None))
                    db.commit()
                    # Split HTTP samples into the configured constant-target stages.
                    run_start = dt.datetime.fromisoformat(started_at)
                    stage_edges = [args.stage_seconds * (i + 1) for i in range(len(vu_levels))]
                    for level_index, target_vus in enumerate(vu_levels):
                        lower = args.stage_seconds * level_index
                        upper = stage_edges[level_index]
                        stage_points = [p for p in points if p["metric"] == "http_req_duration"
                                        and p["operation"] in operations
                                        and p["ts"] is not None
                                        and lower <= (parse_iso_datetime(p["ts"]) - run_start).total_seconds() < upper]
                        for operation in operations:
                            durations = [p["value"] for p in stage_points if p["operation"] == operation]
                            failed = [p for p in points if p["metric"] == "http_req_failed"
                                      and p["operation"] == operation and p["value"] > 0
                                      and p["ts"] is not None
                                      and lower <= (parse_iso_datetime(p["ts"]) - run_start).total_seconds() < upper]
                            db.execute("INSERT OR REPLACE INTO stage_summaries VALUES (?,?,?,?,?,?,?,?,?)", (
                                run_id, target_vus, operation, len(durations), len(failed),
                                len(durations) / args.stage_seconds, percentile(durations, .50),
                                percentile(durations, .95), percentile(durations, .99)))
                    db.commit()
                    docker("rm", "-f", app_name, check=False)
                    app_names.remove(app_name)
                    print(f"{run_id}: {result}, {len(points)} HTTP points, {len(resources)} resource samples")
                    if output_file.exists():
                        output_file.unlink()

        create_report(args.db, args.report)
        print(f"SQLite: {args.db}\nReport: {args.report}")
        return 0
    finally:
        for name in app_names:
            docker("rm", "-f", name, check=False)
        if minio_started:
            docker("rm", "-f", minio_name, check=False)
        docker("network", "rm", network, check=False)
        temp.cleanup()
        db.close()


if __name__ == "__main__":
    raise SystemExit(main())
