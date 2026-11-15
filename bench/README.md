# Local performance benchmarks

This harness runs the same upload-then-fetch workload across the complete matrix:

| Axis | Variants |
|---|---|
| Linux executable packaging | exported standalone binary, project runtime image |
| Linux target | `arm64`, `amd64` |
| Storage | LocalFS, MinIO S3-compatible |

The harness stores run metadata, HTTP samples, resource samples, and operation summaries in `bench/results/benchmarks.sqlite`. It also creates `bench/results/latest.html` with p95 latency, maximum CPU, maximum memory usage, and a result table. These result files are local artifacts and should not be committed unless you deliberately want to publish benchmark data.

## Run the full matrix

Requirements: Docker Desktop running with the `desktop-linux` Buildx builder, `curl`, Python 3, the repository fixture `TEST_IMAGE_ORIGINAL.png`, and enough free disk for BuildKit caches and downloaded images.

```bash
python3 scripts/bench_local.py
```

The default test ramps through 1, 4, 8, and 16 virtual users, spending 15 seconds at each level, then ramps down. Each iteration sends a create-only `POST` to a unique resource and fetches the optimized result with `GET`; this avoids accidentally turning repeated uploads into replacements or duplicate-slot conflicts. The report keeps `post_create` and `fetch` samples separate. API rate limits are disabled for this isolated benchmark so they do not cap the load generator. The application processing limit remains at its configured default of two simultaneous jobs.

Use shorter runs while developing the harness:

```bash
python3 scripts/bench_local.py --stage-seconds 2
```

For a bounded stress ramp, increase the virtual-user targets and repeat each
matrix combination. The run below ramps toward 64 VUs over four 15-second
intervals, then repeats the full matrix once. SQLite stores latency percentiles
for each ramp interval. These are ramp measurements, not steady-state plateaus.

```bash
python3 scripts/bench_local.py --stage-seconds 15 --vu-levels 1,8,32,64 --repeats 2
```

The harness accepts at most 128 VUs and five repeats. Large image uploads can
queue behind the app's two concurrent processing slots, so watch the per-stage
latencies and error counts instead of treating VU count as completed request
volume.

Use multiple fixtures by running the harness once per image; all runs append to
the same local SQLite database. Each run records the fixture's SHA-256 and byte
length, so results remain distinguishable:

```bash
python3 scripts/bench_local.py --fixture ./fixtures/small.png
python3 scripts/bench_local.py --fixture ./fixtures/large.jpg
```

Tune processing concurrency against the same matrix with:

```bash
python3 scripts/bench_local.py --arches arm64 --modes docker \
  --storages localfs,minio --concurrency 3 --profile-processing
```

Repeat with `--concurrency 2` and `--concurrency 4` for comparison. The optional
processing profile adds per-upload queue wait, decode, and WebP compression
timings to `processing_samples` and the HTML report. Profiling adds a debug log
event for each processed image, so use it for focused runs rather than ordinary
production traffic.

Run only selected combinations by passing comma-separated values:

```bash
python3 scripts/bench_local.py --arches arm64 --modes docker --storages localfs,minio
```

## What is measured

- k6 response-duration samples, status, request method, operation label, failed-request samples, and request throughput.
- Docker CPU percentage, memory usage/limit, network I/O, block I/O, and PIDs sampled during each run.
- Repository SHA, host/target architecture, packaging mode, storage backend, fixture SHA-256, Docker version, VU schedule, and result state.

Useful SQLite queries:

```sql
SELECT target_arch, emulation, mode, storage, operation, requests, errors,
       round(rps, 2) AS rps, round(p95_ms, 1) AS p95_ms,
       round(cpu_max_pct, 1) AS cpu_max_pct,
       round(mem_max_bytes / 1048576.0, 1) AS max_mib
FROM runs JOIN summaries USING (run_id)
ORDER BY target_arch, mode, storage, operation;
```

Compare the stress ramp intervals across repeats:

```sql
SELECT target_arch, emulation, mode, storage, stage_vus AS ramp_target,
       operation, sum(requests) AS requests, sum(errors) AS errors,
       round(avg(rps), 2) AS avg_rps,
       round(avg(p95_ms), 1) AS avg_p95_ms
FROM runs JOIN stage_summaries USING (run_id)
WHERE scenario LIKE 'ramp_1_8_32_64%'
GROUP BY target_arch, emulation, mode, storage, ramp_target, operation
ORDER BY target_arch, storage, mode, ramp_target, operation;
```

Inspect time spent inside the image pipeline:

```sql
SELECT r.scenario, count(*) AS images,
       round(avg(p.queue_wait_us) / 1000.0, 1) AS queue_avg_ms,
       round(avg(p.decode_us) / 1000.0, 1) AS decode_avg_ms,
       round(avg(p.compression_us) / 1000.0, 1) AS compression_avg_ms,
       round(avg(p.processing_us) / 1000.0, 1) AS processing_avg_ms
FROM runs r JOIN processing_samples p USING (run_id)
GROUP BY r.scenario
ORDER BY r.started_at;
```

Each reported target is the end of a 15-second linear ramp interval (for
example, the `64` interval ramps from 32 toward 64 VUs); it is not a steady
64-VU plateau.

## Interpretation and limits

- On an ARM64 Mac, the `arm64` target runs natively in Docker Desktop and `amd64` runs under emulation. The latter is useful for compatibility checks and a rough comparison, not a replacement for benchmarking on a native x86-64 Linux host.
- The binary mode exports the Linux executable from the Docker build and bind-mounts it into the same Distroless runtime base. Linux executables cannot run directly on macOS, so this keeps the binary test in a Linux container. The Docker mode runs the repository's built runtime image. Both modes use the same executable and OS/architecture; the comparison is primarily packaging/startup, not a claim that containers add a large per-request penalty.
- LocalFS is mounted on ephemeral Docker tmpfs, and MinIO stores data in ephemeral tmpfs. These runs avoid persistent test data and measure the service/storage path without measuring real disk durability or physical-drive latency. To study disk I/O, change the harness to use disposable Docker volumes and label those runs separately.
- Docker Desktop's CPU and memory allocation affect all results. Keep those settings fixed between comparisons and repeat each matrix several times; compare medians rather than a single run.
- The fixture is the repository's `TEST_IMAGE_ORIGINAL.png`. For broader results, add representative small, medium, and large image fixtures and include each fixture hash in the database.

The harness removes its application containers, temporary MinIO container, and Docker network in a `finally` cleanup path. BuildKit caches and the local result database/report are retained.

## Prepare a native Linux/amd64 host

On Ubuntu or Debian, run the preparation script against `develop`. It checks
Git, Python 3.10+, Docker, and Buildx; clones the selected branch; pulls the
Rust, Distroless, k6, and MinIO images; and builds the release image once to
warm the Docker/BuildKit cache. It does not install or change the host's Docker
engine.

```bash
FIXTURE_PATH=/path/to/TEST_IMAGE_ORIGINAL.png bash scripts/prepare_benchmark_host.sh
```

`TEST_IMAGE_ORIGINAL.png` is intentionally excluded from Git, so provide it
from the host or copy it into the checkout after cloning. The script defaults
to `develop`. Override `BRANCH`, `REPO_URL`, or `WORK_DIR` when needed.
