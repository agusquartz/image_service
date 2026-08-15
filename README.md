# Image Service

[![CI](https://github.com/agusquartz/image_service/actions/workflows/ci.yml/badge.svg)](https://github.com/agusquartz/image_service/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/agusquartz/image_service)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)

**A self-hosted image processing and storage service built with Rust and Axum.**

Image Service accepts uploaded images, validates and decodes them with defensive limits, converts them to WebP, progressively reduces quality and dimensions to satisfy the configured output-size target, and stores the processed image for later retrieval.

The current implementation uses local filesystem storage. HTTP handling, image processing, application orchestration, validation, and storage are intentionally separated so the service can evolve toward configurable processing policies and multiple storage backends.

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

This keeps image-specific infrastructure outside the consuming application while leaving room for future storage and processing backends.

## Features

- Multipart image uploads.
- Public image retrieval over HTTP.
- Optional bearer-token protection for write operations.
- Source MIME validation against the detected file format.
- JPEG, PNG, WebP, GIF, BMP, and TIFF input.
- WebP output.
- Progressive WebP quality reduction.
- Progressive dimension reduction when quality alone is insufficient.
- Maximum source-size protection.
- Decoder width, height, and allocation limits.
- Limited concurrent image-processing jobs.
- Resource-based image organization.
- Up to five image slots per resource.
- Local filesystem persistence.
- Optional public base URL generation.
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
POST /api/images/{namespace}/{resource_id}/{slot}
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
IMAGE_SERVICE_DIR
IMAGE_MAX_CONCURRENCY
IMAGE_API_KEY
IMAGE_PUBLIC_BASE_URL
```

Processing limits such as the maximum source size, target WebP size, decoder allocation limit, resize scale, and slot range are currently compile-time constants.

See [Configuration](docs/configuration.md).

## Documentation

- [Architecture](docs/architecture.md) — module boundaries, responsibilities, project structure, upload/fetch flows, and design principles.
- [Configuration](docs/configuration.md) — environment variables, resource rules, validation, processing limits, and concurrency.
- [HTTP API](docs/api.md) — endpoints, responses, headers, status codes, and examples.
- [Client Examples](docs/clients.md) — cURL, JavaScript Fetch, Java, Python, and Rust.
- [Security](docs/security.md) — current safeguards, trust boundaries, and production security considerations.
- [Deployment](docs/deployment.md) — running from source, release builds, filesystem locations, and deployment guidance.
- [Roadmap](ROADMAP.md) — planned storage backends, processing profiles, configuration, observability, and future architecture.

## Roadmap

The long-term goal is to make deployment-specific behavior configurable without changing application code, including:

- Local and S3-compatible storage backends.
- Runtime-configurable image policies.
- Additional output formats.
- Processing profiles.
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
