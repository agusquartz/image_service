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

Deploy one Image Service instance for each customer. Give each instance its own API key, configuration, and storage credentials; use a separate filesystem location or bucket/prefix as appropriate. This separates customer data and helps keep application failures, configuration changes, and maintenance within that customer's deployment. A `namespace` is only a resource grouping inside an instance, not a security boundary. If deployments share a host, cluster, network, or storage system, those remain shared failure and security dependencies.

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

### Multi-architecture images and release binaries

The release build supports two Linux targets:

| Target | Intended systems |
|---|---|
| `linux/amd64` | x86-64 Linux servers and cloud instances. |
| `linux/arm64` | 64-bit ARM Linux, including Oracle Ampere instances, AWS Graviton, Azure Cobalt, and the Linux VM used by Docker Desktop on Apple Silicon. |

These providers use the same Linux ARM64 image format, so the service does not need vendor-specific builds. The build uses generic target settings rather than tuning the binary to the CPU that built it. Oracle documents Ampere Linux as `aarch64` ([OCI Arm-based compute](https://docs.oracle.com/en-us/iaas/Content/Compute/References/arm.htm)); AWS identifies Graviton as `arm64` ([EC2 instance specifications](https://docs.aws.amazon.com/ec2/latest/instancetypes/gp.html)); and Azure describes Cobalt as a 64-bit Arm CPU ([Azure Cobalt overview](https://learn.microsoft.com/en-us/azure/virtual-machines/sizes/cobalt-overview)). Docker selects the matching platform variant from a multi-platform image ([Docker multi-platform builds](https://docs.docker.com/build/building/multi-platform/)).

Apple Silicon is a macOS host, while these images are Linux containers; Docker Desktop runs the `linux/arm64` image in its Linux VM. This release does not include a native macOS executable.

Build either platform locally with Buildx:

```bash
docker buildx build --platform linux/amd64 --target runtime --tag image-service:local-amd64 --load .
docker buildx build --platform linux/arm64 --target runtime --tag image-service:local-arm64 --load .
```

On a version tag such as `v0.1.0`, GitHub Actions checks that the tag matches `Cargo.toml`, builds a standalone Linux binary and a Docker image for each target, publishes architecture-specific images to GHCR, and creates a multi-architecture manifest plus the GitHub release. Release assets include SHA-256 checksums. The manifest tag lets Docker select the correct image automatically; pull a specific architecture only when needed:

```bash
docker pull ghcr.io/agusquartz/image_service:v0.1.0
docker pull ghcr.io/agusquartz/image_service:v0.1.0-linux-amd64
docker pull ghcr.io/agusquartz/image_service:v0.1.0-linux-arm64
```

Version tags also update `latest`; ordinary pushes and merges do not publish releases. The standalone executables are dynamically linked GNU/Linux binaries built against Debian 12 and need a compatible glibc on the host. The container images carry their matching runtime libraries and are the more portable option across Linux distributions.

After the first image is published, set the GHCR package visibility to **Public** if customers should be able to pull it without registry credentials. Until then, GHCR may require authentication even though the repository itself is public.

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

- Docker Compose examples.
- Systemd guidance.
- Configurable caching and CDN behavior.
- Structured logging and metrics.
- Storage migration tooling.
- Direct-to-object-storage upload flows.
- Additional OpenDAL-backed storage services when required.

See the [Roadmap](../ROADMAP.md).

---
