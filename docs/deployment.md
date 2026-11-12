[← Back to README](../README.md)

# Deployment

This document covers the current source-based deployment model and the runtime environment expected by Image Service.

Storage is provided through OpenDAL. The currently compiled storage backends are:

- Local filesystem.
- S3.
- S3-compatible object storage.

The selected storage backend is a deployment concern and does not change the public HTTP API.

## Requirements

All deployments require:

- A Rust toolchain compatible with the project `rust-version`.
- Network access to the configured bind address.
- Access to the configured storage backend.

Filesystem deployments additionally require:

- Permission to create directories and write files below the configured storage root.

S3 and S3-compatible deployments additionally require:

- Network access to the object-storage service.
- Access to the configured bucket.
- Valid storage credentials or another authentication mechanism supported by the configured backend.

The service is a self-hosted server application and is not intended to be published as a reusable crates.io library.

## Customer Deployment Isolation

Deploy one Image Service instance per customer, with separate API credentials, service configuration, and storage credentials or locations. This keeps customer data boundaries explicit and helps contain application failures and maintenance within one deployment. A `namespace` only groups resources within an instance; it is not a security boundary. Shared hosts, clusters, networks, or storage remain common dependencies.

The version currently on `main` is used in production by two companies running an ERP developed by the project maintainer. As of November 2026, no production errors or operational problems have been reported for that version.

---

## Environment File

For local or simple deployments:

```bash
cp .env.example .env
```

The application loads `.env` through `dotenvy` when present.

Do not commit the real `.env` file.

Storage credentials, API keys, and other secrets should be supplied through an appropriate secret-management mechanism in production.

---

## Running from Source

Install Rust using your preferred Rust toolchain, then run:

```bash
cargo run
```

For a release build:

```bash
cargo build --release
```

Then run the generated binary.

The default bind address is:

```text
http://127.0.0.1:3000
```

Check the service:

```bash
curl -i http://127.0.0.1:3000/health
```

Expected status:

```text
204 No Content
```

---

## Release Build

For a production-oriented binary:

```bash
cargo build --release --locked
```

The resulting executable is created under:

```text
target/release/
```

The repository should commit `Cargo.lock`, and automated builds should use `--locked` so dependency resolution cannot silently change during CI or deployment.

## Distroless Docker Image

Build the multi-stage image:

```bash
docker build -t image-service:local .
```

The runtime stage uses Distroless and runs as its non-root user. The HTTP server must bind to `0.0.0.0` inside the container:

```bash
docker run --rm --read-only \
  --publish 127.0.0.1:3000:3000 \
  --tmpfs /tmp/image-storage:rw,nosuid,size=64m,uid=65532,gid=65532,mode=0755 \
  --env IMAGE_BIND=0.0.0.0:3000 \
  --env IMAGE_STORAGE_SCHEME=fs \
  --env IMAGE_STORAGE_ROOT=/tmp/image-storage \
  --env IMAGE_STORAGE_ATOMIC_WRITE_DIR=/tmp/image-storage/.tmp \
  image-service:local
```

This `tmpfs` setup is intended for temporary testing: image data disappears when the container stops. Use durable storage for real deployments.

The filesystem endpoint smoke test exercises the private/private default, public-read/private-write, private-read/public-write, explicitly unprotected policies, and explicitly public metrics; missing-key and removed-setting startup failures; rate-limit `429`/`Retry-After`; request IDs; authenticated-by-default Prometheus metrics; POST create-only behavior; PUT replacement; GET, HEAD, info, listing, slot/resource DELETE; processing profiles; and invalid configuration. It needs Docker, `curl`, Python 3, and a local test image:

```bash
docker build -t image-service:local .
bash scripts/smoke_test.sh image-service:local ./TEST_IMAGE_ORIGINAL.png
```

The image file is a local fixture and is excluded from Git and the Docker build context. Pass another local PNG/JPEG path as the script's second argument when needed.

### MinIO S3-compatible smoke test

The MinIO test starts a MinIO-compatible server and Image Service on a private Docker network. Both use ephemeral containers and a `tmpfs` object directory; the script removes the containers and network on exit. It checks create-only and replace uploads, S3 GET/HEAD and conditional GET, metadata-bearing listings including a legacy slot above the configured limit, a namespace-specific processing profile, direct download through a presigned URL, slot deletion, and recursive resource-level deletion. The resource deletion test verifies idempotence and confirms that objects under a similarly named resource are preserved.

```bash
bash scripts/verify_phase1_phase2.sh ./TEST_IMAGE_ORIGINAL.png
```

The combined script builds the Distroless service image, runs the filesystem smoke test, pulls the MinIO-compatible test image if needed, and runs the S3-compatible smoke test. Both test backends use volatile storage and clean up their containers. Set `MINIO_IMAGE` to use a different MinIO-compatible image and `IMAGE_SERVICE_IMAGE` to choose the service image tag. The individual scripts can also be run separately.

## Versioned Releases

The current `main` baseline declares version `0.1.0` in `Cargo.toml`. The initial `v0.1.0` release is created by manually running the **Release** GitHub Actions workflow on `main`; it attaches a Linux `amd64` executable and its SHA-256 checksum.

For later releases, merge a package-version update in `Cargo.toml` (and the corresponding `Cargo.lock` update, when required) into `main`. GitHub Actions compares the package version with the previous `main` commit and publishes a GitHub Release only when that version changed. Ordinary merges that keep the same version do not publish releases. Release notes are generated from merged pull requests and can be edited on GitHub after publication.

Download the executable and checksum from the matching GitHub Release. Verify the file with:

```bash
sha256sum -c image_service-v0.1.0-linux-amd64.sha256
```

The standalone executable is a Linux `amd64` GNU/Linux build and requires a compatible glibc on the host.

---

## Storage

Image Service uses OpenDAL to provide backend-independent image storage.

The backend is selected at runtime using:

```dotenv
IMAGE_STORAGE_SCHEME=fs
```

or:

```dotenv
IMAGE_STORAGE_SCHEME=s3
```

Application-facing image identifiers remain unchanged when switching backends.

For example:

```text
products/845/01.webp
```

is a local filesystem path relative to the storage root when using `fs`, and an object key when using `s3`.

---

### Filesystem Deployment

Filesystem storage is the default backend:

```dotenv
IMAGE_STORAGE_SCHEME=fs
```

If no storage root is configured, Image Service uses the operating system's local application-data directory.

Typical locations are:

Linux:

```text
~/.local/share/image-service/images
```

Windows:

```text
%LOCALAPPDATA%\image-service\images
```

macOS:

```text
~/Library/Application Support/image-service/images
```

A production server will usually configure an explicit location:

```dotenv
IMAGE_STORAGE_ROOT=/var/lib/image-service/images
```

The service process must be able to create directories and write files below that path.

#### Atomic Writes

Filesystem storage uses OpenDAL atomic writes.

Unless explicitly configured otherwise, the atomic-write directory is:

```text
<IMAGE_STORAGE_ROOT>/.tmp
```

For example:

```text
/var/lib/image-service/images/.tmp
```

It can be overridden:

```dotenv
IMAGE_STORAGE_ATOMIC_WRITE_DIR=/var/lib/image-service/tmp
```

The service process must also have permission to create and write to this directory.

A typical filesystem deployment therefore looks like:

```dotenv
IMAGE_BIND=127.0.0.1:3000

IMAGE_STORAGE_SCHEME=fs
IMAGE_STORAGE_ROOT=/var/lib/image-service/images

IMAGE_MAX_CONCURRENCY=2

IMAGE_API_KEY=change-me
```

---

### S3 Deployment

To store processed images in S3:

```dotenv
IMAGE_STORAGE_SCHEME=s3

IMAGE_STORAGE_BUCKET=my-images
IMAGE_STORAGE_REGION=us-east-1

IMAGE_STORAGE_ACCESS_KEY_ID=...
IMAGE_STORAGE_SECRET_ACCESS_KEY=...
```

The service process must be able to:

- Reach the configured S3 service.
- Authenticate successfully.
- Read objects.
- Write objects.
- Replace existing objects.

The bucket should exist and should have an access policy appropriate for the service.

Do not commit credentials to `.env` or source control.

In production, prefer the secret-management or credential mechanism appropriate to the deployment platform.

---

### S3-Compatible Deployment

S3-compatible services use the same backend with a custom endpoint when required.

Example:

```dotenv
IMAGE_STORAGE_SCHEME=s3

IMAGE_STORAGE_BUCKET=my-images
IMAGE_STORAGE_REGION=us-east-1
IMAGE_STORAGE_ENDPOINT=https://s3-compatible.example.com

IMAGE_STORAGE_ACCESS_KEY_ID=...
IMAGE_STORAGE_SECRET_ACCESS_KEY=...
```

This can be used with providers such as MinIO, Cloudflare R2, and other S3-compatible object stores.

Provider-specific endpoint, region, and authentication settings depend on the selected service.

---

## Bind Address

Default:

```dotenv
IMAGE_BIND=127.0.0.1:3000
```

Binding to loopback is appropriate when a reverse proxy runs on the same machine.

If the service must listen on all interfaces:

```dotenv
IMAGE_BIND=0.0.0.0:3000
```

Do this only with an appropriate firewall, reverse proxy, authentication policy, and TLS strategy.

---

## Reverse Proxy

A reverse proxy can provide:

- TLS termination.
- Request-size enforcement.
- Access logging.
- Rate limiting.
- Hostname routing.
- Additional security headers.
- Optional caching/CDN integration.

The application-level body and decoder limits should remain enabled even when equivalent proxy limits exist.

For filesystem deployments, the reverse proxy does not need direct access to stored images because retrieval continues through Image Service.

For S3 deployments, the current API also retrieves stored images through Image Service. Direct object-store delivery or presigned URLs are future possibilities rather than current behavior.

---

## Health Check

Use:

```text
GET /health
```

A healthy HTTP process returns:

```text
204 No Content
```

The current health endpoint verifies that the HTTP application is running. It should not be treated as a comprehensive external-storage health check.

---

## Storage Backups

Backup strategy depends on the selected backend.

### Filesystem

Consider:

- Filesystem snapshots.
- Scheduled backup jobs.
- Replication.
- Storage quotas.
- Disk-space monitoring.

### S3-Compatible Storage

Consider:

- Bucket versioning.
- Replication.
- Lifecycle policies.
- Retention rules.
- Provider-specific durability settings.
- Storage quotas and billing alerts.

Image Service itself does not currently manage backup policies.

---

## Production Checklist

Before public deployment:

- Use a release build.
- Keep `Cargo.lock` committed.
- Run CI successfully.
- Keep the private-read/private-write default and configure a strong `IMAGE_API_KEY`.
- Only set `IMAGE_READ_AUTH=none` or `IMAGE_WRITE_AUTH=none` when the exposure is intentional.
- Configure read/write rate limits for expected traffic and monitor `429` responses.
- Keep `IMAGE_METRICS_AUTH=true` unless a trusted network or proxy protects `/metrics`; when enabled, metrics authentication requires `IMAGE_API_KEY`.
- Set `RUST_LOG` to the desired operational log level.
- Configure stdout log retention/rotation in the runtime or platform. For Docker's `json-file` driver, set `max-size` and `max-file`; the service itself does not write or rotate log files.
- Set a deliberate bind address.
- Select the intended storage backend.
- Verify storage connectivity and permissions.
- Verify filesystem permissions when using `fs`.
- Verify bucket access and credentials when using `s3`.
- Protect API keys and storage credentials.
- Put TLS in front of the service.
- Apply proxy/request limits.
- Configure logs and monitoring.
- Plan backups and storage quotas.
- Review [`security.md`](security.md).

---

## Future Deployment Improvements

Planned deployment improvements include:

- Container images.
- Docker Compose examples.
- Systemd guidance.
- Configurable caching and CDN behavior.
- Structured logging and metrics.
- Storage migration tooling.
- Direct-to-object-storage upload flows.
- Additional OpenDAL-backed storage services when required.

See the [Roadmap](../ROADMAP.md).

---
