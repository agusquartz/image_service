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
- Set an explicit write-authentication policy.
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
