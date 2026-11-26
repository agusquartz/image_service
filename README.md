# Image Service

[![CI](https://github.com/agusquartz/image_service/actions/workflows/ci.yml/badge.svg)](https://github.com/agusquartz/image_service/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/agusquartz/image_service)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)

**A self-hosted, single-tenant image processing and storage service built with Rust and Axum.**

Image Service validates and decodes uploaded images with defensive limits, then stores WebP within the configured output-size limit. A valid static WebP that already has the target dimensions, needs no orientation transform, and fits the byte limit is preserved as-is. Other inputs are cropped, resized, or encoded only as needed; output uses the configured target dimensions unless further downscaling is needed to meet the byte limit.

Storage is provided through OpenDAL. The currently compiled backends are local filesystem and S3/S3-compatible object storage. HTTP handling, image processing, application orchestration, validation, and storage are intentionally separated so additional processing policies and storage backends can be added without coupling them to the HTTP layer.

## Why Image Service?

Applications often need image uploads without wanting image decoding, optimization, storage layout, and delivery logic embedded in the main application.

Image Service isolates that responsibility behind a small HTTP API:

```mermaid
---
config:
  theme: base
  look: classic
  layout: dagre
  htmlLabels: true

  themeVariables:
    primaryColor: "#332717"
    primaryTextColor: "#e0c39d"
    primaryBorderColor: "#ffffff1a"
    lineColor: "#a0a0a0"
    edgeLabelBackground: "#322a2d"
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', Arial, sans-serif"
    fontSize: "10.5px"

  flowchart:
    rankSpacing: 60
    nodeSpacing: 45
    minNodeWidth: 210
    curve: basis

  themeCSS: |
    .node rect {
      fill: #332717;
      stroke: rgba(255,255,255,0.10);
      stroke-width: 0.75px;
      rx: 12px;
      ry: 12px;
    }

    .node .label,
    .node .label p,
    .node text {
      color: #e0c39d;
      fill: #e0c39d;
      font-size: 10.5px;
      font-weight: 600;
    }

    .node .label p {
      padding: 4.5px 9px;
      width: 210px !important;
      min-width: 210px !important;
      box-sizing: border-box;
      margin: 0;
      text-align: center;
    }

    .edgeLabel {
      background-color: transparent;
      text-align: center;
    }

    .edgeLabel p {
      background-color: #322a2d;
      color: #e0c39d;
      border: 0.75px solid #e0c39d;
      border-radius: 9.75px;
      padding: 7.5px 18px;
      min-width: 112.5px;
      box-sizing: border-box;
      margin: 0;
      font-size: 9.75px;
      font-weight: 600;
      line-height: 1.5;
      text-align: center;
    }

    .edgeLabel .label rect {
      fill: #322a2d;
      stroke: #e0c39d;
      rx: 9.75px;
      ry: 9.75px;
    }

    .flowchart-link {
      stroke: rgba(249,249,247,0.50);
      stroke-width: 0.75px;
    }

    .marker {
      fill: rgba(249,249,247,0.50);
      stroke: rgba(249,249,247,0.50);
    }
---
flowchart TB
    CLIENT[HTTP clients<br/>ERP, web apps, SDKs, services]
    CORE[FILE CORE<br/>Axum, AuthN/AuthZ, OpenDAL<br/>configuration, request admission]
    INPROC[InProcess path<br/>direct call through the Processor contract]
    REMOTE[Remote / IPC path<br/>protocol DTOs exist, transport not implemented]
    LOCAL_PROCESSORS[InProcess processors<br/>Image: implemented<br/>Video: not implemented yet]
    WORKER_IMAGE[worker-image<br/>not implemented yet]
    WORKER_VIDEO[worker-video<br/>not implemented yet]
    OTHER_WORKERS[Other workers<br/>document/blob and other capabilities<br/>not implemented yet]

    CLIENT -->|HTTP| CORE
    CORE --> INPROC
    CORE -. future worker dispatch .-> REMOTE
    INPROC --> LOCAL_PROCESSORS
    REMOTE -. future transport .-> WORKER_IMAGE
    REMOTE -. future transport .-> WORKER_VIDEO
    REMOTE -. future transport .-> OTHER_WORKERS
```

The solid path shows the current implementation: `file-core` handles HTTP requests and invokes image processing in-process. The normal image routes are synchronous; the explicit `/api/image-jobs/...` routes stage the source and enqueue durable image processing, with a narrow status endpoint. When storage usage is enabled, SQLite/WAL jobs maintain the inventory, recover mutation-journal entries, verify stored images against current policy, and reprocess repairable violations with conditional writes. Dashed paths are future work: remote worker dispatch, worker processes, and video processing. `worker-protocol` currently provides versioned DTOs only.

## Deployment Model and Production Use

Image Service is designed to run as a dedicated instance for one customer. Each customer can have separate API credentials, service configuration, and storage access. Keeping instances separate helps contain application-level failures and protects customer data from access through another customer's service instance. Shared hosts, clusters, networks, or storage infrastructure can still be common failure points. `namespace` organizes resources inside one customer's instance; it is not an access-control boundary.

The version currently on `main` is used in production by two companies running an ERP developed by the project maintainer. As of November 2026, no production errors or operational problems have been reported. This production experience refers to the version on `main`.

## Features

- Multipart image uploads.
- Image retrieval over HTTP; private reads are the secure default and public reads are configurable.
- Image information with storage and WebP dimensions metadata.
- Presigned read URLs when the storage backend supports them.
- Storage-provided ETag and Last-Modified validators with conditional GET/HEAD requests when available.
- Resource image listing, HEAD metadata checks, and idempotent deletion.
- POST creates only in an unused slot; PUT creates or replaces the complete image.
- Optional durable asynchronous image processing through `/api/image-jobs/...`; existing `/api/images/...` routes remain synchronous.
- Optional Tus resumable uploads and storage-backed HTTP `Range` downloads.
- Independent bearer-token policies for reads and writes; both are protected by default.
- Per-source-IP read/write rate limits with `429` responses and `Retry-After`; clients behind one NAT/proxy share the budget.
- Structured JSON request logs, server-generated request IDs, and an optional authenticated Prometheus endpoint.
- Source MIME validation against the detected file format.
- JPEG, PNG, WebP, GIF, BMP, and TIFF input.
- WebP output.
- Configurable input byte limit (15 MiB by default).
- Configurable WebP output byte limit (100 KiB by default).
- Configurable target output dimensions (1024×1024 by default, center-cropped without distortion; further downscaled only if needed to meet the byte limit).
- Optional named WebP processing profiles assigned by namespace; profiles override only explicitly configured input/output limits or dimensions and inherit the existing global settings for the rest.
- Lossy WebP quality search from the same resampled pixels to meet the maximum output byte limit.
- Valid WebP inputs that already meet the configured dimensions and byte limit pass through without lossy re-encoding.
- Configurable decoder width, height, and allocation defaults with weighted processing-memory admission.
- Limited concurrent image-processing jobs.
- Optional authenticated storage usage reporting with a durable SQLite catalog; service mutations are reflected promptly and full inventory runs periodically.
- Automatic background verification and policy-based image maintenance, with conditional writes that protect newer uploads.
- Resource-based image organization.
- Configurable maximum slot number for new images (5 by default; valid range 1..=255).
- Local filesystem persistence through OpenDAL.
- S3 and S3-compatible object storage.
- Runtime-selectable storage backend.
- Automated publication of versioned Linux `amd64` and `arm64` container images to GHCR, with architecture-specific and multi-architecture tags; the current `v0.1.0` baseline predates Docker distribution.
- Automated standalone Linux `amd64` and `arm64` release binaries with SHA-256 checksums; the current `v0.1.0` baseline contains the `amd64` binary only.
- Optional public base URL generation.
- Configurable Cache-Control with a 60-second default lifetime.
- JSON error responses.
- Backwards-compatible liveness endpoint plus separate liveness and storage-readiness probes.

## Quick Start

```bash
cp .env.example .env
cargo run --release
curl -F "image=@photo.jpg" -H "Authorization: Bearer replace-with-a-long-random-secret" \
  http://127.0.0.1:3000/api/images/products/845/1
```

The service refuses to start without `IMAGE_API_KEY` under the default private read/write policies. Public reads or fully unprotected access must be explicitly configured with `IMAGE_READ_AUTH=none` and/or `IMAGE_WRITE_AUTH=none`.

### Cargo feature builds

The default `images` feature preserves the complete HTTP API and is the supported service build. Cargo features can also produce smaller architecture builds:

```bash
cargo build --release --locked
cargo build --release --locked --no-default-features
cargo build --release --locked --no-default-features --features worker-protocol
cargo build --release --locked --no-default-features --features all
```

The no-image builds expose health/readiness, optional metrics, and the generic durable internal job catalog; image routes and local image processing are excluded. The `worker-protocol` feature includes versioned worker/job data types, but does not yet provide remote worker transport or dispatch. See [Architecture](docs/architecture.md#cargo-workspace-and-features).

## API Overview

```text
GET  /health
GET  /health/live
GET  /health/ready

GET  /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}/{slot}/info
HEAD /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}
PUT  /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}/presign
DELETE /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}
DELETE /api/images/{namespace}/{resource_id}

POST /api/image-jobs/{namespace}/{resource_id}/{slot}
PUT  /api/image-jobs/{namespace}/{resource_id}/{slot}
GET  /api/image-jobs/{job_id}

OPTIONS /api/image-uploads                          # when Tus is enabled
OPTIONS /api/image-uploads/{upload_id}              # when Tus is enabled
POST    /api/image-uploads/{namespace}/{resource_id}/{slot}
PUT     /api/image-uploads/{namespace}/{resource_id}/{slot}
HEAD    /api/image-uploads/{upload_id}
PATCH   /api/image-uploads/{upload_id}
DELETE  /api/image-uploads/{upload_id}

GET  /metrics                                        # when metrics are enabled

GET  /api/storage/usage?namespace=products&resource_id=845&slot=1
POST /api/storage/reconcile
POST /api/storage/audits
GET  /api/storage/audits/{audit_id}
GET  /api/storage/audits/{audit_id}/findings
```

Storage usage routes are opt-in through `IMAGE_STORAGE_USAGE_ENABLED=true` and
always require the configured API key, independently of image read
authentication. See [HTTP API](docs/api.md#storage-usage-reporting).

Example:

```text
POST /api/images/products/845/1
GET  /api/images/products/845/1
```

See the [HTTP API documentation](docs/api.md) for request, response, validation, and error details.

## Architecture

```mermaid
---
config:
  theme: base
  look: classic
  layout: dagre
  htmlLabels: true

  themeVariables:
    primaryColor: "#332717"
    primaryTextColor: "#e0c39d"
    primaryBorderColor: "#ffffff1a"
    lineColor: "#a0a0a0"
    edgeLabelBackground: "#322a2d"
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', Arial, sans-serif"
    fontSize: "10.5px"

  flowchart:
    rankSpacing: 60
    nodeSpacing: 45
    curve: basis

  themeCSS: |
    .node rect {
      fill: #332717;
      stroke: rgba(255,255,255,0.10);
      stroke-width: 0.75px;
      rx: 12px;
      ry: 12px;
    }

    .node .label,
    .node .label p,
    .node text {
      color: #e0c39d;
      fill: #e0c39d;
      font-size: 10.5px;
      font-weight: 600;
    }

    .node .label p {
      padding: 4.5px 9px;
      min-width: 210px;
      box-sizing: border-box;
      margin: 0;
    }

    .edgeLabel {
      background-color: transparent;
      text-align: center;
    }

    .edgeLabel p {
      background-color: #322a2d;
      color: #e0c39d;
      border: 0.75px solid #e0c39d;
      border-radius: 9.75px;
      padding: 7.5px 18px;
      min-width: 112.5px;
      box-sizing: border-box;
      margin: 0;
      font-size: 9.75px;
      font-weight: 600;
      line-height: 1.5;
      text-align: center;
    }

    .edgeLabel .label rect {
      fill: #322a2d;
      stroke: #e0c39d;
      rx: 9.75px;
      ry: 9.75px;
    }

    .flowchart-link {
      stroke: rgba(249,249,247,0.50);
      stroke-width: 0.75px;
    }

    .marker {
      fill: rgba(249,249,247,0.50);
      stroke: rgba(249,249,247,0.50);
    }
---
flowchart TB
    CLIENT[HTTP clients<br/>ERP, web apps, SDKs, and other services]
    BINARY[image_service executable]
    CORE[file-core<br/>Axum API, authentication, request admission<br/>image use cases, storage coordination]
    CONTRACT[processor-contract<br/>In-process processor interface]
    IMAGE[processor-image<br/>Decode, validate, and optimize WebP]
    STORAGE[OpenDAL<br/>Backend-independent storage]
    LOCAL[Local filesystem]
    S3[S3-compatible object storage]
    PROTOCOL[worker-protocol<br/>Versioned job and storage DTOs only]
    SCHEDULER[Durable job scheduler<br/>SQLite WAL, image processing and system queues]
    JOBDB[(Persistent job catalog)]

    subgraph FUTURE["Future extension — not implemented"]
        REMOTE[Remote transport<br/>UDS or gRPC]
        WORKERS[Remote workers<br/>Image, video, document, and blob capabilities]
        REMOTE --> WORKERS
    end

    CLIENT -->|HTTP| BINARY --> CORE
    CORE --> CONTRACT -->|In process| IMAGE
    IMAGE -->|Processed result| CORE
    CORE --> STORAGE
    STORAGE --> LOCAL
    STORAGE --> S3
    CORE -->|async image uploads and internal jobs| SCHEDULER --> JOBDB
    CORE -. "optional Cargo feature: DTOs only" .-> PROTOCOL
    SCHEDULER -. "future worker dispatch" .-> REMOTE
    PROTOCOL -. "future transport may use these types" .-> REMOTE
```

The solid paths show implemented components. The scheduler persists and dispatches `image.processing` jobs in-process, alongside storage inventory, mutation recovery, verification, maintenance, and read-only policy audit queues when storage usage is enabled. Dashed paths show the planned worker registry, UDS/gRPC transport, and remote worker runtime. `worker-protocol` provides versioned data types only.

The core rule is simple:

> HTTP-specific code stays at the edge of the application. Image processing and storage do not depend on Axum.

See [Architecture](docs/architecture.md) for module responsibilities and complete upload/fetch flows.

## Configuration

The current runtime configuration includes:

```text
IMAGE_BIND
IMAGE_STORAGE_SCHEME
IMAGE_STORAGE_*
IMAGE_MAX_CONCURRENCY
IMAGE_MAX_DECODE_DIMENSION
IMAGE_MAX_DECODE_ALLOCATION_BYTES
IMAGE_PROCESSING_MEMORY_BUDGET_BYTES
IMAGE_MAX_UPLOADS_IN_FLIGHT
IMAGE_UPLOAD_MEMORY_BUDGET_BYTES
IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES
IMAGE_UPLOAD_READ_TIMEOUT_SECONDS
IMAGE_RESUMABLE_UPLOADS_ENABLED
IMAGE_RESUMABLE_UPLOAD_TTL_SECONDS
IMAGE_RESUMABLE_UPLOAD_MAX_CHUNK_BYTES
IMAGE_MAX_DOWNLOADS_IN_FLIGHT
IMAGE_DOWNLOAD_STREAM_CHUNK_BYTES
IMAGE_SHUTDOWN_TIMEOUT_SECONDS
IMAGE_JOB_DB_PATH
IMAGE_BACKGROUND_JOB_CONCURRENCY
IMAGE_BACKGROUND_IDLE_SECONDS
IMAGE_BACKGROUND_WINDOW_UTC
IMAGE_JOB_RETENTION_DAYS
IMAGE_ASYNC_MAX_JOBS
IMAGE_ASYNC_MAX_STAGED_BYTES
IMAGE_STORAGE_USAGE_ENABLED
IMAGE_STORAGE_CATALOG_DB_PATH
IMAGE_STORAGE_INVENTORY_INTERVAL_SECONDS
IMAGE_MAX_INPUT_BYTES
IMAGE_MAX_OUTPUT_BYTES
IMAGE_OUTPUT_WIDTH
IMAGE_OUTPUT_HEIGHT
IMAGE_PROCESSING_PROFILES
IMAGE_NAMESPACE_PROFILES
IMAGE_MAX_SLOTS
IMAGE_API_KEY
IMAGE_READ_AUTH
IMAGE_WRITE_AUTH
IMAGE_RATE_LIMIT_READ_REQUESTS
IMAGE_RATE_LIMIT_WRITE_REQUESTS
IMAGE_RATE_LIMIT_WINDOW_SECONDS
IMAGE_METRICS_ENABLED
IMAGE_METRICS_AUTH
IMAGE_PRESIGN_TTL_SECONDS
IMAGE_CACHE_TTL_SECONDS
IMAGE_CACHE_CONTROL
IMAGE_PUBLIC_BASE_URL
RUST_LOG
```

Tus upload-session routes are disabled unless `IMAGE_RESUMABLE_UPLOADS_ENABLED=true`; `/metrics` is disabled unless `IMAGE_METRICS_ENABLED=true`. `IMAGE_STORAGE_*` also covers backend-specific OpenDAL options.

Input/output byte limits, target output dimensions, decoder safety limits, and the global maximum slot number for new images can be configured at runtime. Input defaults to 15 MiB for compatibility, output defaults to 100 KiB, and new slots default to `1..=5` (`IMAGE_MAX_SLOTS` accepts `1..=255`). Existing higher slots remain readable and deletable after lowering the setting; PUT can replace them but cannot create them. If quality 0 at the target dimensions still exceeds the byte limit, the service reduces dimensions while preserving the target aspect ratio. Decoder dimension and allocation defaults are 12,000 px and 256 MiB; the weighted processing memory budget defaults to 1,056 MiB for two concurrent 1024×1024 operations. These are configurable safeguards, not automatic measurements of host RAM; increasing dimensions, allocation limits, or concurrency requires planning for additional hardware use. Image GET and HEAD responses default to a 60-second cache lifetime; set `IMAGE_CACHE_TTL_SECONDS=0` to emit `Cache-Control: no-store`.

Processing profiles are optional. With no profile assigned, the current global environment settings remain in effect. A profile can override the input byte limit, WebP output byte limit, and/or target dimensions for selected namespaces; omitted profile fields inherit the corresponding global value, which itself falls back to the built-in default. See [Configuration](docs/configuration.md) for the JSON environment variable format.

Opt-in Tus resumable upload sessions and HTTP byte-range downloads are documented in the [API guide](docs/api.md). Tus is disabled by default; its SQLite database stores session state, offsets and checksums while confirmed chunks live in durable OpenDAL staging. GET streams bounded chunks from storage, and supported `Range` requests read only the requested byte interval. Stream concurrency and chunk size are configurable per instance.

Application logs are structured JSON written to stdout. The `level` field uses Rust tracing levels (`ERROR`, `WARN`, `INFO`, `DEBUG`, `TRACE`); `RUST_LOG` controls filtering and defaults to `info`. The service does not write or rotate log files: configure retention in Docker or the deployment platform's log collector. See [Configuration](docs/configuration.md#rust_log) for details.

See [Configuration](docs/configuration.md).

## Documentation

- [Architecture](docs/architecture.md) — module boundaries, responsibilities, project structure, upload/fetch flows, and design principles.
- [Background Jobs](docs/jobs.md) — registered jobs, triggers, idle scheduling, retries, persistence, and shutdown behavior.
- [Configuration](docs/configuration.md) — environment variables, resource rules, validation, processing limits, and concurrency.
- [HTTP API](docs/api.md) — endpoints, responses, headers, status codes, and examples.
- [Client Examples](docs/clients.md) — cURL, JavaScript Fetch, Java, Python, and Rust.
- [Security](docs/security.md) — current safeguards, trust boundaries, and production security considerations.
- [Deployment](docs/deployment.md) — running from source, release builds, storage configuration, and production deployment guidance.
- [Roadmap](ROADMAP.md) — storage evolution, processing profiles, configuration, observability, and future architecture.

## Roadmap

Implemented capabilities are listed above and documented in the linked guides. Remaining roadmap work includes:

- Immutable, content-versioned image URLs with long-lived cache headers.
- Additional output formats.
- More image operations; profiles already apply input/output limits and target dimensions by namespace. Future per-namespace policy work means auth, formats, slot limits, storage routing, quotas, and retention.
- Distributed rate limiting and per-namespace quotas; current rate limits are in-memory per process.
- OpenTelemetry and image-processing/storage metrics; current `/metrics` covers HTTP request counts, HTTP duration, and rate-limited requests only.
- Remote worker dispatch and direct-to-object-storage uploads.
- Persistent metadata, deduplication, content addressing, and storage migration tooling.
- Structured configuration files and a command-line configuration interface.

See [ROADMAP.md](ROADMAP.md) for the full roadmap.

## Contributing

Contributions are welcome.

Before opening a pull request, please read:

- [CONTRIBUTING.md](CONTRIBUTING.md)
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

Security vulnerabilities should be reported according to [SECURITY.md](SECURITY.md), not through public issues.

## License

Licensed under the [Apache License 2.0](LICENSE).
