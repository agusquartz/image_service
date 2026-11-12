# Image Service

[![CI](https://github.com/agusquartz/image_service/actions/workflows/ci.yml/badge.svg)](https://github.com/agusquartz/image_service/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/agusquartz/image_service)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)

**A self-hosted, single-tenant image processing and storage service built with Rust and Axum.**

Image Service accepts uploaded images, validates and decodes them with defensive limits, center-crops to the configured aspect ratio, and encodes WebP under the configured output-size limit. It uses the configured target dimensions unless further downscaling is needed to meet that byte limit.

Storage is provided through OpenDAL. The currently compiled backends are local filesystem and S3/S3-compatible object storage. HTTP handling, image processing, application orchestration, validation, and storage are intentionally separated so the service can evolve toward configurable processing policies and multiple storage backends.

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
flowchart TD
    A[Application] -->|HTTP| B[Image Service]
    B -->|"Validation<br/>↓<br/>Processing<br/>↓<br/>Storage"| C[Optimized image]
```

This keeps image-specific infrastructure outside the consuming application while leaving room for future storage and processing capabilities.

## Deployment Model and Production Use

Image Service is designed to run as a dedicated instance for one customer. Each customer can have separate API credentials, service configuration, and storage access. Keeping instances separate helps contain application-level failures and protects customer data from access through another customer's service instance. Shared hosts, clusters, networks, or storage infrastructure can still be common failure points. `namespace` organizes resources inside one customer's instance; it is not an access-control boundary.

The version currently on `main` is used in production by two companies running an ERP developed by the project maintainer. As of November 2026, no production errors or operational problems have been reported. This production experience refers to the version on `main`.

## Features

- Multipart image uploads.
- Image retrieval over HTTP; private reads are the secure default and public reads are configurable.
- Image information with storage and WebP dimensions metadata.
- Presigned read URLs when the storage backend supports them.
- ETag and Last-Modified validators with conditional GET/HEAD requests.
- Resource image listing, HEAD metadata checks, and idempotent deletion.
- POST creates only in an unused slot; PUT creates or replaces the complete image.
- Independent bearer-token policies for reads and writes; both are protected by default.
- Per-client read/write rate limits with `429` responses and `Retry-After`.
- Structured JSON request logs, server-generated request IDs, and an optional authenticated Prometheus endpoint.
- Source MIME validation against the detected file format.
- JPEG, PNG, WebP, GIF, BMP, and TIFF input.
- WebP output.
- Configurable input byte limit (15 MiB by default).
- Configurable WebP output byte limit (100 KiB by default).
- Configurable target output dimensions (1024×1024 by default, center-cropped without distortion; further downscaled only if needed to meet the byte limit).
- Optional named WebP processing profiles assigned by namespace; profiles override only explicitly configured input/output limits or dimensions and inherit the existing global settings for the rest.
- Lossy WebP quality search from the same resampled pixels to meet the maximum output byte limit.
- Decoder width, height, and allocation limits.
- Limited concurrent image-processing jobs.
- Resource-based image organization.
- Configurable maximum slot number for new images (5 by default; valid range 1..=255).
- Local filesystem persistence through OpenDAL.
- S3 and S3-compatible object storage.
- Runtime-selectable storage backend.
- Versioned Linux `amd64` and `arm64` container images with architecture-specific tags and a multi-architecture tag published to GHCR from version tags.
- Standalone Linux `amd64` and `arm64` release binaries with SHA-256 checksums.
- Optional public base URL generation.
- Configurable Cache-Control with a 60-second default lifetime.
- JSON error responses.
- Lightweight health endpoint.

## Quick Start

```bash
cp .env.example .env
cargo run --release
curl -F "image=@photo.jpg" -H "Authorization: Bearer replace-with-a-long-random-secret" \
  http://127.0.0.1:3000/api/images/products/845/1
```

The service refuses to start without `IMAGE_API_KEY` under the default private read/write policies. Public reads or fully unprotected access must be explicitly configured with `IMAGE_READ_AUTH=none` and/or `IMAGE_WRITE_AUTH=none`.

## API Overview

```text
GET  /health

GET  /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}/{slot}/info
HEAD /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}
PUT  /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}/presign
DELETE /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}
DELETE /api/images/{namespace}/{resource_id}
```

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
flowchart TD
    HTTP --> Handlers --> Service
    Service --> ImageProcessing[Image processing] --> Storage
```

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
```

Input/output byte limits, target output dimensions, and the global maximum slot number for new images can be configured at runtime. Input defaults to 15 MiB for compatibility, output defaults to 100 KiB, and new slots default to `1..=5` (`IMAGE_MAX_SLOTS` accepts `1..=255`). Existing higher slots remain readable and deletable after lowering the setting; PUT can replace them but cannot create them. If quality 0 at the target dimensions still exceeds the byte limit, the service reduces dimensions while preserving the target aspect ratio. Decoder allocation and source-dimension safeguards remain compile-time limits. Image GET and HEAD responses default to a 60-second cache lifetime; set `IMAGE_CACHE_TTL_SECONDS=0` to emit `Cache-Control: no-store`.

Processing profiles are optional. With no profile assigned, the current global environment settings remain in effect. A profile can override the input byte limit, WebP output byte limit, and/or target dimensions for selected namespaces; omitted profile fields inherit the corresponding global value, which itself falls back to the built-in default. See [Configuration](docs/configuration.md) for the JSON environment variable format.

Application logs are structured JSON written to stdout. The `level` field uses Rust tracing levels (`ERROR`, `WARN`, `INFO`, `DEBUG`, `TRACE`); `RUST_LOG` controls filtering and defaults to `info`. The service does not write or rotate log files: configure retention in Docker or the deployment platform's log collector. See [Configuration](docs/configuration.md#rust_log) for details.

See [Configuration](docs/configuration.md).

## Documentation

- [Architecture](docs/architecture.md) — module boundaries, responsibilities, project structure, upload/fetch flows, and design principles.
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
- Background processing and direct-to-object-storage uploads.
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
