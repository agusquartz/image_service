# Image Service

[![CI](https://github.com/agusquartz/image_service/actions/workflows/ci.yml/badge.svg)](https://github.com/agusquartz/image_service/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/agusquartz/image_service)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)

**A self-hosted image processing and storage service built with Rust and Axum.**

Image Service accepts uploaded images, validates and decodes them with defensive limits, center-crops to the configured aspect ratio, and encodes WebP under the configured output-size limit. It uses the configured target dimensions unless further downscaling is needed to meet that byte limit.

Storage is provided through OpenDAL. The currently compiled backends are local filesystem and S3/S3-compatible object storage. HTTP handling, image processing, application orchestration, validation, and storage are intentionally separated so the service can evolve toward configurable processing policies and multiple storage backends.

## Why Image Service?

Applications often need image uploads without wanting image decoding, optimization, storage layout, and delivery logic embedded in the main application.

Image Service isolates that responsibility behind a small HTTP API:

```text
Application
    |
    | HTTP
    v
Image Service
    |
    +----> Validation
    +----> Processing
    +----> Storage
    |
    v
Optimized image
```

This keeps image-specific infrastructure outside the consuming application while leaving room for future storage and processing capabilities.

## Features

- Multipart image uploads.
- Public image retrieval over HTTP.
- Image information with storage and WebP dimensions metadata.
- Presigned read URLs when the storage backend supports them.
- ETag and Last-Modified validators with conditional GET/HEAD requests.
- Resource image listing, HEAD metadata checks, and idempotent deletion.
- POST creates only in an unused slot; PUT creates or replaces the complete image.
- Optional bearer-token protection for write operations.
- Optional bearer-token protection for reads, disabled by default for backwards compatibility.
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
- Optional public base URL generation.
- Configurable Cache-Control with a 60-second default lifetime.
- JSON error responses.
- Lightweight health endpoint.

## Quick Start

```bash
cp .env.example .env
cargo run --release
curl -F "image=@photo.jpg" http://127.0.0.1:3000/api/images/products/845/1
```

If `IMAGE_API_KEY` is configured, add:

```bash
-H "Authorization: Bearer <your-api-key>"
```

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

```text
HTTP
  |
  v
Handlers
  |
  v
Service
  |
  +------> Image processing
  |
  +------> Storage
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
GETMETHOD_AUTH
IMAGE_PRESIGN_TTL_SECONDS
IMAGE_CACHE_TTL_SECONDS
IMAGE_CACHE_CONTROL
IMAGE_PUBLIC_BASE_URL
```

Input/output byte limits, target output dimensions, and the global maximum slot number for new images can be configured at runtime. Input defaults to 15 MiB for compatibility, output defaults to 100 KiB, and new slots default to `1..=5` (`IMAGE_MAX_SLOTS` accepts `1..=255`). Existing higher slots remain readable and deletable after lowering the setting; PUT can replace them but cannot create them. If quality 0 at the target dimensions still exceeds the byte limit, the service reduces dimensions while preserving the target aspect ratio. Decoder allocation and source-dimension safeguards remain compile-time limits. Image GET and HEAD responses default to a 60-second cache lifetime; set `IMAGE_CACHE_TTL_SECONDS=0` to emit `Cache-Control: no-store`.

Processing profiles are optional. With no profile assigned, the current global environment settings remain in effect. A profile can override the input byte limit, WebP output byte limit, and/or target dimensions for selected namespaces; omitted profile fields inherit the corresponding global value, which itself falls back to the built-in default. See [Configuration](docs/configuration.md) for the JSON environment variable format.

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

The long-term goal is to make deployment-specific behavior configurable without changing application code, including:

- Runtime-configurable image policies.
- Additional output formats.
- Additional processing profile options and selection rules.
- Cache and CDN behavior.
- Authentication strategies.
- Per-namespace policies.
- Metadata and deletion.
- Observability and rate limiting.
- Background processing and direct-to-object-storage uploads.

See [ROADMAP.md](ROADMAP.md) for the full roadmap.

## Contributing

Contributions are welcome.

Before opening a pull request, please read:

- [CONTRIBUTING.md](CONTRIBUTING.md)
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

Security vulnerabilities should be reported according to [SECURITY.md](SECURITY.md), not through public issues.

## License

Licensed under the [Apache License 2.0](LICENSE).
