[← Back to README](../README.md)

# Architecture

---

This document describes the current project structure, module responsibilities, request flows, and architectural principles.

## Deployment and Isolation Model

The service is designed as a single-tenant deployment: one running service instance serves one customer. Its API key, read/write policies, rate limits, processing settings, and storage configuration apply to that whole instance. A dedicated instance gives each customer a separate application failure and maintenance boundary, and allows separate storage credentials and data locations.

Namespaces group resources within that customer's instance. They do not create separate identities or access-control boundaries. Deployments that share a host, cluster, network, or storage system still depend on those shared components.

---

## Project Structure

```text
src/main.rs                       # Minimal executable entry point
crates/
├── file-core/src/                # HTTP, auth, config, storage, lifecycle, use cases
│   ├── auth.rs
│   ├── config.rs
│   ├── router.rs
│   ├── state.rs
│   └── images/                   # Image API and storage adapters; enabled by `images`
├── processor-contract/src/       # Processor capability and in-process request/result types
├── processor-image/src/          # Decode, crop, resize, WebP encoding
├── job-scheduler/src/            # SQLite/WAL job catalog, leases, retries, local dispatcher
└── worker-protocol/src/          # Versioned worker/job/storage-reference DTOs
```

The layout follows a simple rule:

> HTTP-specific code should remain at the edge of the application. Image processing and storage should not depend on Axum.

## Cargo Workspace and Features

The existing root `image_service` binary delegates startup to the generic
`file-core` crate. The default `images` feature enables the current image routes
and the `processor-image` crate, preserving service behavior and configuration
by default. The image processor implements the shared `Processor` contract;
the Core retains HTTP
admission, the bounded Tokio blocking-pool handoff, storage, and response
handling.

The root package forwards `images` and `worker-protocol` features to the Core. Supported
builds are checked in CI:

```text
default (`images`)                 compatible image service
--no-default-features              core, health/readiness, optional metrics, durable job catalog
--no-default-features --features worker-protocol
                                   core, durable job catalog, protocol types, no local processor
--no-default-features --features all
                                   image service, durable job catalog, and worker protocol types
```

The `worker-protocol` crate defines versioned registration, job, result, and
`StorageRef` data types for image, video, document, and blob capabilities. The
`job-scheduler` crate provides a durable SQLite/WAL catalog and bounded
in-process dispatcher. When storage usage reporting is enabled, `file-core`
registers `storage.catalog`, `storage.inventory`, `storage.verify`,
`storage.maintenance`, and `storage.audit` queues. Image builds also register
`image.processing` for the explicit asynchronous image-job API. A separate
SQLite catalog keeps completed inventory, the
mutation journal, verification state, and audit findings. HTTP mutations update
usage totals promptly; periodic inventory discovers changes made directly in
LocalFS or S3. Verification skips decoding when object validators and the
effective policy fingerprint are unchanged. Repairable violations are
reprocessed in low-priority jobs with conditional publication and post-write
verification. Unix socket connections, worker registration, remote dispatch,
and worker failover remain future work. A no-image build can run the generic
job catalog, but cannot register image storage handlers.

### Current Runtime and Planned Worker Boundary

The running service handles HTTP requests in `file-core`, invokes
`processor-image` in-process through the `Processor` interface, and persists
the result through OpenDAL. Tokio's blocking-pool handoff and the image
processing admission limit bound CPU-heavy synchronous work. Normal
`/api/images/...` operations remain synchronous. The explicit
`/api/image-jobs/...` API streams source bytes to private durable staging,
records a processing job in SQLite/WAL, and returns a job ID; a bounded local
dispatcher processes it and exposes a narrow job status endpoint.

The optional `worker-protocol` feature compiles versioned worker DTOs. The
durable scheduler stores recurring interval definitions and job queue, kind,
payload, policy snapshot, dependencies, lease, retry, progress, and result state
in SQLite/WAL. Due schedules materialize idempotent jobs; the in-memory
dispatcher claims only a bounded number and recovers expired leases after
restart. System background work waits for a quiet HTTP period and can also be
restricted to a daily UTC window. Staged image-processing jobs bypass the
quiet-period gate so accepted uploads can complete during traffic, but remain
bounded by scheduler concurrency, upload-memory admission, and the processor
semaphore; they still obey readiness, leases, retries, and an optional UTC
window. With `IMAGE_STORAGE_USAGE_ENABLED=true`, the
service journals POST/PUT/DELETE before calling OpenDAL and applies the
resulting catalog update before a successful HTTP response. Pending journal
entries are recovered into `storage.catalog` after restart. The recurring
inventory streams object listings and promotes a completed scan only after the
full listing succeeds, then queues candidates for verification. Verification
records policy compliance and schedules repairable violations; maintenance
reprocesses one captured image version and uses conditional writes to avoid
replacing a newer upload. Usage queries read only the catalog and never list
storage on the request path. Direct storage edits become visible after
reconciliation. UDS/gRPC worker registration and remote dispatch remain future
work. Synchronous and queued image processing both currently use the
in-process processor.

See [Background Jobs](jobs.md) for the current registered work, scheduling
gates, persistence and recovery behavior, and guidance for adding internal
handlers.

---

---

## Layer Responsibilities

The current architecture is divided into five main layers with a separate process-lifecycle boundary.

### HTTP Layer

Files:

```text
router.rs
health.rs
auth.rs
observability.rs
rate_limit.rs
images/handler/
images/multipart.rs
```

Responsibilities:

- Define routes.
- Preserve `/health` as a liveness-compatible route and provide separate liveness and storage-readiness probes.
- Extract path parameters.
- Read request headers.
- Parse multipart requests.
- Apply independent read/write bearer-authentication policies.
- Enforce per-source-IP read/write rate limits before handlers run; shared NAT/proxy addresses share the budget.
- Add request IDs, structured request logs, and low-cardinality metrics.
- Expose Prometheus metrics at `/metrics` when enabled, with independent authentication enabled by default.
- Convert application results into HTTP responses.
- Set response headers.

The HTTP layer should not implement image compression or storage backend operations directly.
Synchronous multipart input is received through the Core transfer manager: it
counts actual bytes, keeps up to `IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES` in a
bounded buffer, and spills larger bodies into private `__image_transfer/`
staging before processing. The absolute configured input limit remains in
force. After receive completes, the service reserves the whole source size and
loads it for image decoding; staging bounds request reception, not decoder
memory. Temporary transfer objects are removed on success, failure, timeout,
or request cancellation. Async jobs use their separate `__image_jobs/` prefix.

### Process Lifecycle

Files: root `src/main.rs`, `file-core/src/lib.rs`, `server.rs`, and lifecycle state in `state.rs`.

`main.rs` loads `.env` and delegates startup to `file_core::run`, which builds the Tokio runtime with a bounded shutdown wait. `server.rs` handles SIGTERM/SIGINT, marks the shared lifecycle state as not ready, and asks Axum to stop accepting connections and drain active requests until the configured deadline. Readiness checks the configured OpenDAL backend; liveness only reports whether the process is running.

### Application / Service Layer

File:

```text
images/service.rs
```

Responsibilities:

- Coordinate the image upload use case.
- Coordinate image retrieval, deletion, metadata stat, and resource listing.
- Call the reduction layer.
- Call the storage layer.
- Build the final public image URL.
- Produce response DTOs.

The service layer represents application behavior rather than transport or infrastructure details.

### Image Processing Layer

File:

```text
processor-image/src/lib.rs
```

Responsibilities:

- Detect the real image format from bytes.
- Verify that the declared MIME type matches the detected format.
- Apply decoder safety limits.
- Resolve the optional namespace processing profile, inheriting unspecified values from global configuration.
- Decode source images.
- Preserve a fully validated WebP unchanged when its orientation and dimensions already match policy and its bytes fit the output limit.
- Encode non-WebP inputs to WebP; re-encode WebP only when output dimensions or byte limits require it.
- Reduce WebP quality.
- Crop or resize images only when necessary.
- Enforce the final output-size target.
- Move CPU-heavy work away from the async executor.
- Respect the image-processing concurrency limit.

This crate has no knowledge of HTTP routes, namespaces, storage backends, or Axum. Its synchronous `Processor` contract is called by the Core through `spawn_blocking`; the Core owns admission and concurrency permits.

Conceptually:

```text
source bytes -> detect format/MIME -> decode and validate
                                      |
                                      v
                              check output policy
                               /             \
           compliant static WebP             needs adjustment
                    |                         |
                    v                         v
           reuse original bytes        crop/resize/encode
                               \             /
                                v           v
                                 stored WebP
```

### Storage Layer

File:

```text
images/store.rs
```

Responsibilities:

- Convert ImageKey into a backend-independent object key.
- Read image bytes through OpenDAL.
- Write image bytes through OpenDAL.
- Delete image objects through OpenDAL.
- Read object metadata with `stat` and list slots under a resource prefix.
- Coordinate uploads and deletes per resource in-process. Uploads capture resource/slot generations before consuming the body; a later slot or resource delete invalidates older uploads before they can publish.
- Read GET bodies as metadata/body snapshots. The resource lock excludes this process's writes/deletes; backends with conditional-read support also use the metadata ETag as `If-Match`.
- Confirm conditional GET validators before returning a bodyless response: S3-compatible stores issue a one-byte conditional range read, while LocalFS rechecks metadata under the resource lock.
- Generate presigned read requests only when the backend advertises that capability.
- Translate storage backend errors into application errors.

Example object key:

```text
products/845/01.webp
```

No compression logic belongs in this layer.

The operation coordinator is an in-memory consistency mechanism for one service process. It serializes publication/deletion for the same resource and keeps unrelated resources independent. The SQLite mutation journal records service writes for catalog recovery, but does not coordinate multiple replicas or make OpenDAL and SQLite one transaction. See [Background Jobs](jobs.md) for recovery and consistency details.

### Configuration and Shared State

Files:

```text
config.rs
state.rs
```

Responsibilities:

- Load environment variables.
- Define processing limits.
- Configure the storage backend and backend-specific options.
- Configure authentication.
- Configure concurrency.
- Configure the public base URL.
- Configure presigned URL lifetime and optional cache headers.
- Construct dependencies shared by handlers.

---

---

## Upload Flow

An upload request follows this path:

```text
Client
  |
  | POST (create) / PUT (replace) multipart/form-data
  v
Router
  |
  v
Upload Handler
  |
  +----> Write authentication
  |
  +----> Path validation / ImageKey
  |
  +----> Multipart extraction
  +----> Bounded RAM buffer, then private staging above configured threshold
  |
  v
Image Service
  |
  v
Reduction Layer
  |
  +----> Detect format
  +----> Verify MIME
  +----> Decode with safety limits
  +----> Keep compliant static WebP bytes unchanged
  +----> Otherwise crop/resize only if dimensions require it
  +----> Encode non-WebP, or re-encode WebP if limits require it
  +----> Reduce quality/dimensions only as needed for the byte limit
  |
  v
Storage Layer
  |
  +----> Build backend-independent object key
  +----> Write through OpenDAL
  |
  v
UploadResponse
  |
  v
JSON response
```

A successful upload response looks like:

```json
{
  "namespace": "products",
  "resource_id": "845",
  "slot": 2,
  "url": "/api/images/products/845/2",
  "size_bytes": 183421
}
```

If `IMAGE_PUBLIC_BASE_URL` is configured:

```dotenv
IMAGE_PUBLIC_BASE_URL=https://images.example.com
```

the URL becomes:

```json
{
  "url": "https://images.example.com/api/images/products/845/2"
}
```

---

---

## Fetch Flow

Retrieval is intentionally much simpler:

```text
Client
  |
  | GET
  v
Router
  |
  v
Fetch Handler
  |
  +----> Path validation / ImageKey
  |
  v
Image Service
  |
  v
Storage Layer
  |
  v
WebP bytes
  |
  v
HTTP response
```

The response includes:

```text
Content-Type: image/webp
X-Content-Type-Options: nosniff
```

## Delete, HEAD, and Resource Listing

These operations use the same validated `ImageKey` and OpenDAL-backed storage boundary:

- `DELETE /api/images/{namespace}/{resource_id}/{slot}` follows `IMAGE_WRITE_AUTH` (required by default) and treats a missing object as a successful idempotent delete.
- `DELETE /api/images/{namespace}/{resource_id}` follows `IMAGE_WRITE_AUTH` and deletes recursively under the exact resource prefix. OpenDAL keeps directory and object-prefix details inside the storage layer; empty and missing resources are idempotent.
- `HEAD /api/images/{namespace}/{resource_id}/{slot}` calls `stat` to return content type and length without reading image bytes.
- `GET /api/images/{namespace}/{resource_id}` lists existing `NN.webp` slots under the resource prefix in ascending order, includes available storage metadata, and returns `max_creatable_slot`. The list includes existing slots above `IMAGE_MAX_SLOTS` so lowering the limit never hides stored images. OpenDAL normalizes LocalFS and object-storage listing behavior before it reaches the application layer.

`IMAGE_READ_AUTH` and `IMAGE_WRITE_AUTH` independently control bearer authentication; both default to `required`, and startup requires `IMAGE_API_KEY` for either required policy. `GETMETHOD_AUTH` has been removed. `/health` remains public. `/metrics` is optional (`IMAGE_METRICS_ENABLED=false` by default) and has its own `IMAGE_METRICS_AUTH=true` default; startup requires an API key if metrics are enabled and metrics authentication remains on.
`/health/live` is independent of storage, while `/health/ready` performs a bounded storage probe. The legacy `/health` route aliases liveness.

---

---

## Design Principles

The project intentionally follows a few architectural rules.

### Handlers stay small

Handlers should deal primarily with HTTP concerns.

They should not contain WebP algorithms, storage backend logic, or complex business behavior.

### Processing is transport-independent

The reduction code receives image bytes and metadata and returns processed bytes.

This makes the processing layer usable from:

- HTTP handlers.
- Background jobs.
- CLI tools.
- Batch imports.
- Message-queue consumers.

without rewriting the image algorithm.

### Storage is an infrastructure concern

The application service asks storage to read or write an image.

The rest of the application should not care whether those bytes ultimately live on:

- Local disk.
- Amazon S3.
- MinIO.
- Cloudflare R2.
- Azure Blob Storage.
- Google Cloud Storage.

The current implementation provides filesystem and S3/S3-compatible
storage through OpenDAL. Additional backends can be introduced without
changing the application service or HTTP layers.

### Configuration belongs in one place

Runtime and processing configuration should progressively move into `AppConfig` instead of being scattered through handlers or infrastructure modules.

---
