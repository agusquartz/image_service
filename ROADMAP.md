# Roadmap

[← Back to README](README.md)

Image Service is intentionally small. The long-term goal is to make deployment-specific behavior configurable while preserving a stable core pipeline:

```text
HTTP
  |
  v
Application Service
  |
  +----> Processing
  |
  +----> Storage
```

The project should remain simple where possible. New abstractions should be introduced only when they solve a real deployment or application requirement.

---

## Current Capabilities

The current implementation includes:

```text
HTTP upload                    yes
HTTP fetch                     yes
HTTP HEAD                      yes
HTTP resource listing          yes
HTTP image deletion            yes
HTTP image information         yes
Presigned read URLs            yes (backend capability required)
Cache validators               yes
Upload methods                 POST (create-only), PUT (create or replace)

Local filesystem storage       yes
S3 storage                     yes
S3-compatible storage          yes

OpenDAL storage abstraction    yes
Runtime-selectable storage     yes
Atomic filesystem writes       yes

WebP reduction                 yes
Configurable image limits      yes (input/output bytes, output width/height)
Optional API key               yes
Optional read authentication   yes

Delete endpoint                yes
Database                       no
Processing profiles            yes (optional namespace-specific WebP settings)
Runtime processing policy      yes (profiles override input/output byte limits and dimensions; omitted fields inherit global settings and built-in defaults)
```

S3-compatible storage can be configured for providers such as MinIO and Cloudflare R2 using provider-specific endpoint and credential settings.

Storage is implemented through OpenDAL.

The application uses backend-independent image object keys such as:

```text
products/845/01.webp
```

The configured OpenDAL operator determines where those bytes are physically stored.

Conceptually:

```text
Image Service
     |
     v
 ImageStore
     |
     v
OpenDAL Operator
     |
     +----> Filesystem
     |
     +----> S3 / S3-compatible storage
```

This means the HTTP, processing, and application layers do not need to know whether image bytes live on local disk, Amazon S3, or another compatible object store.

---

## Implemented: Processing Profiles

Phase 3 adds optional named WebP processing profiles through `IMAGE_PROCESSING_PROFILES`, assigned to namespaces with `IMAGE_NAMESPACE_PROFILES`. A profile can override the input byte limit, output byte limit, and target dimensions. Each omitted field inherits its matching global setting, then the built-in default. Namespaces without a profile keep the existing global behavior, so profiles remain opt-in and backward compatible.

Verification covers profile overrides and fallback behavior in unit tests, profile-specific input limits and inherited global limits in LocalFS smoke tests, and namespace profile processing in MinIO smoke tests. The tested Docker containers use ephemeral storage and are removed after the run. See [configuration](docs/configuration.md#per-namespace-processing-profiles) and [deployment testing](docs/deployment.md) for configuration and commands.

## Future Features

The long-term direction is to make the service increasingly configurable, including image policy, security, caching, URL generation, processing behavior, storage operations, and observability.

The items below describe planned architectural directions rather than functionality that should be assumed to exist today.

---

### Storage Evolution

Filesystem and S3-compatible storage are already supported through OpenDAL.

Future storage work should therefore focus on capabilities around the abstraction rather than implementing separate storage modules for every provider.

Possible future improvements include:

- Additional OpenDAL storage services when required.
- Storage migration tooling.
- Storage validation commands.
- Per-namespace storage policies.
- Storage observability.
- Storage quotas.
- Object prefixes.
- Storage lifecycle integration.
- Credential-management improvements.

The current storage path should remain conceptually simple:

```text
ImageKey
   |
   v
backend-independent object key
   |
   v
ImageStore
   |
   v
OpenDAL
```

Provider-specific implementations such as:

```text
local.rs
s3.rs
minio.rs
r2.rs
```

should not be introduced unless OpenDAL cannot provide a required capability.

---

### Processing Policy Beyond Global Output Settings

Global input/output byte limits and target output dimensions are now runtime-configurable. If the byte limit cannot be met at the target dimensions, output dimensions are reduced while preserving the target aspect ratio. Remaining compile-time policies that could become configurable include decoder allocation, accepted source dimensions, supported formats, and processing concurrency policy.

For example:

```dotenv
IMAGE_MAX_DECODE_MEMORY_MB=256
IMAGE_MAX_SOURCE_DIMENSION=12000
IMAGE_SUPPORTED_FORMATS=jpeg,png,webp,gif,bmp,tiff
```

This would allow the same binary to run under different image policies without recompilation.

---

### Configurable Output Formats

The current service always produces WebP.

A future version could support:

```dotenv
IMAGE_OUTPUT_FORMAT=webp
```

with additional choices such as:

```text
webp
avif
jpeg
png
```

Namespace-selected WebP processing profiles are implemented; unmapped namespaces keep the current global settings, and profile fields can override input/output byte limits and exact target dimensions. Selecting a different output format remains future work.

For example:

```text
products -> WebP, max 400 KiB, 2000 × 2000
avatars  -> WebP, max 100 KiB, 512 × 512
```

---

### More Image Operations

Possible processing capabilities include:

- Thumbnail generation.
- Fixed-size resizing.
- Maximum-width or maximum-height resizing.
- Crop modes.
- Center crop.
- Smart crop.
- Orientation normalization.
- EXIF orientation handling.
- Metadata removal.
- Optional metadata retention.
- Transparent-background handling.
- Watermarks.
- Multiple derivatives from a single upload.
- Placeholder generation.
- Blur hashes.
- Dominant-color extraction.

---

### Delete Operations

Implemented:

```text
DELETE /api/images/{namespace}/{resource_id}/{slot}
DELETE /api/images/{namespace}/{resource_id}
```

The operation uses the OpenDAL-backed `ImageStore`, requires write authentication when `IMAGE_API_KEY` is configured, and returns `204 No Content` for both existing and missing slots.

### Resource Listing and HEAD

Implemented APIs:

```text
GET  /api/images/{namespace}/{resource_id}
HEAD /api/images/{namespace}/{resource_id}/{slot}
```

Resource listing returns existing WebP slots in ascending order. HEAD uses storage `stat` and returns content type and length without reading image bytes.

The storage layer could delegate deletion to OpenDAL independently of the selected backend.

Logical deletion could be introduced separately if ownership, auditing, or retention requirements eventually require metadata persistence.

---

### Resource-Level Operations

Implemented APIs:

```text
GET    /api/images/{namespace}/{resource_id}
DELETE /api/images/{namespace}/{resource_id}
```

These:

- List existing slots in ascending order with available byte size, ETag, and Last-Modified metadata; use the per-image `/info` route for dimensions.
- Remove every object under the exact resource prefix, idempotently.

OpenDAL handles the filesystem directory and object-storage prefix semantics inside the storage layer, outside the HTTP and application layers.

---

### Presigned Read URLs

Implemented at `POST /api/images/{namespace}/{resource_id}/{slot}/presign`. The endpoint signs GET only, uses `IMAGE_PRESIGN_TTL_SECONDS` (default 3600, maximum 604800), and checks OpenDAL's `presign_read` capability. Filesystem storage returns `501 Not Implemented`; presigned writes remain out of scope because they would bypass image processing.

### Metadata

On-demand image information is implemented at `GET /api/images/{namespace}/{resource_id}/{slot}/info`. It returns WebP dimensions, size, ETag, and Last-Modified when available. Durable metadata persistence and source-format history remain future work.

Future versions may persist additional metadata such as:

```json
{
  "format": "webp",
  "width": 1280,
  "height": 853,
  "size_bytes": 183421,
  "source_format": "jpeg",
  "created_at": "...",
  "updated_at": "..."
}
```

Metadata storage should remain independent of image-byte storage.

Possible implementations include:

- Sidecar JSON objects.
- SQL database.
- Key-value database.
- Object-store metadata.
- Application-owned database tables.

---

### Cache Configuration

Image GET and HEAD support ETag, Last-Modified when available, and conditional requests (`If-None-Match`, `If-Modified-Since`) that can return `304 Not Modified`. Cache responses default to 60 seconds, public for unauthenticated reads and private when GET auth is enabled. `IMAGE_CACHE_TTL_SECONDS=0` disables storage with `no-store`; `IMAGE_CACHE_CONTROL` overrides the full policy. `immutable` remains inappropriate for mutable slot URLs.

Example:

```dotenv
IMAGE_CACHE_TTL_SECONDS=60
```

Future cache work may include:

- CDN-aware cache headers.
- **Versioned URLs for immutable caching (optional):** add a URL keyed by the SHA-256 digest of the final stored WebP and preserve the existing mutable slot URL for compatibility. Return the versioned URL additively from uploads so storefronts can store and use it. Apply a long lifetime such as `public, max-age=31536000, immutable` only to versioned URLs; keep a short TTL on the mutable URL. Before implementation, define retention and deletion for old versions, including storage lifecycle/cost and how resource deletion affects cached copies. Do not use an object-store ETag as the content hash unless the backend guarantees that meaning.
- Optional `IMAGE_STREAM_GET` responses for deployments with larger image limits or high concurrent download volume.

---

### Configurable Authentication

The current write authentication model is a single optional bearer token.

Read authentication is implemented through `GETMETHOD_AUTH`. It is disabled by default to preserve existing public image URLs; when enabled, GET, HEAD, and resource listing require the configured `IMAGE_API_KEY`.

Possible future strategies:

```text
none
static bearer token
JWT
API keys
reverse-proxy authentication
mTLS
application callback / policy service
```

Configuration might select the mechanism:

```dotenv
IMAGE_AUTH_MODE=bearer
```

Authorization could also become namespace-aware.

---

### Per-Namespace Policies

Different namespaces may eventually use different rules:

```text
products
users
articles
documents
```

Possible namespace-specific configuration:

- Allowed source formats.
- Maximum source and output sizes beyond the limits available in processing profiles.
- Number of slots.
- Processing profile selection (basic namespace assignment is implemented; richer namespace policies remain future work).
- Public/private read access.
- Storage backend.
- Storage prefix.
- Retention rules.

---

### Configurable Slot Limits

Implemented as the global `IMAGE_MAX_SLOTS` setting. It defaults to `5` and accepts `1..=255`. It limits creation: POST cannot create above the configured maximum; PUT may replace an existing higher slot but cannot create one. GET, HEAD, info, presign, list, and DELETE continue to work for existing slots above the setting. Resource listings report `max_creatable_slot`. Namespace-specific slot limits remain future work.

```dotenv
IMAGE_MAX_SLOTS=5
```

Reducing the configured limit does not hide or delete existing higher-numbered slots. They remain accessible and can be deleted or replaced with PUT; new slots above the limit are rejected.

---

### Observability

Planned operational improvements may include:

- `tracing`.
- Structured JSON logs.
- Request IDs.
- Prometheus metrics.
- Processing-duration metrics.
- Compression-ratio metrics.
- Storage latency.
- Storage backend labels.
- Failure counters.
- OpenTelemetry.
- Distributed tracing.

Potential metrics:

```text
image_upload_total
image_upload_failed_total
image_processing_duration_seconds
image_source_bytes
image_output_bytes
image_resize_rounds
image_store_duration_seconds
image_store_failed_total
```

---

### Rate Limiting and Quotas

Future deployments may need configurable limits for:

- Requests per IP.
- Requests per API key.
- Uploads per namespace.
- Storage used per namespace.
- Processing CPU budget.
- Daily upload quotas.

---

### Background Processing

The current API processes images synchronously before returning success.

A future optional mode could support queued processing:

```text
POST upload
    |
    v
source storage
    |
    v
queue
    |
    v
worker
    |
    v
processed derivative
```

This could be useful for large workloads while preserving the current synchronous mode for simpler deployments.

---

### Direct-to-Object-Storage Uploads

Because S3-compatible storage is already supported, a future architecture could allow clients to upload source files directly to object storage through presigned URLs.

Conceptually:

```text
Client
  |
  +----> Image Service: request upload authorization
  |
  <----+ Presigned URL
  |
  +----> Object storage directly
```

A worker or callback could then process the source image.

This would avoid routing large source uploads through the application server.

The current implementation does not provide presigned upload flows.

---

### Database Integration

Image Service currently does not require a database.

A future optional database layer could support:

- Image metadata.
- Ownership.
- Audit trails.
- Logical deletion.
- Processing state.
- Deduplication.
- Reference counting.
- Expiration policies.

Image binaries should normally continue to live in object storage or filesystem storage rather than a relational database unless a deployment has a specific reason to do otherwise.

---

### Deduplication

Possible future support:

```text
SHA-256(source bytes)
```

or a hash of normalized image contents.

This could prevent storing the same image repeatedly.

---

### Content Addressing

An alternative storage strategy could use hashes:

```text
ab/cd/abcdef....webp
```

instead of resource-based object keys.

The application layer could then map resource slots to content hashes.

This would be a different logical storage strategy while still remaining compatible with the OpenDAL storage abstraction.

---

### Storage Migration

Because multiple storage backends now exist, migration tooling is a realistic future feature.

Possible migrations include:

```text
filesystem -> S3
S3 -> filesystem
MinIO -> S3
S3 -> R2
bucket A -> bucket B
```

A migration tool should preserve logical object keys so application-facing image URLs do not need to change.

Possible commands could eventually include:

```bash
image-service migrate-storage
image-service verify-storage
```

---

### Configuration File Support

Environment variables are convenient for deployments, but future versions may support a structured configuration file:

```text
image-service.toml
```

Example direction:

```toml
[server]
bind = "0.0.0.0:3000"

[storage]
scheme = "s3"

[storage.options]
bucket = "images"
region = "us-east-1"

[processing]
format = "webp"
max_source_mb = 15
max_output_kb = 400
max_concurrency = 4

[auth]
mode = "bearer"
```

Environment variables could override configuration-file values.

A possible precedence model:

```text
defaults
   <
configuration file
   <
environment variables
   <
command-line arguments
```

---

### Command-Line Configuration

A future CLI may provide:

```bash
image-service \
  --bind 0.0.0.0:3000 \
  --config ./image-service.toml
```

Other commands could include:

```bash
image-service serve
image-service validate-config
image-service migrate-storage
image-service verify-storage
image-service inspect
```

---

### Complete Configuration Goal

The long-term objective is that deployment-specific behavior can be changed without modifying or recompiling application code.

Eventually, configuration should be able to control:

```text
Server
├── bind address
├── public URL
├── proxy awareness
└── request limits

Storage
├── scheme
├── path / bucket
├── region
├── endpoint
├── prefix
├── credentials strategy
├── migration behavior
└── per-namespace policy

Processing
├── source limits
├── decoder limits
├── output format
├── output size
├── dimensions
├── resize strategy
├── quality levels
├── concurrency
└── profiles

HTTP
├── cache headers
├── CORS
├── compression policy
└── security headers

Authentication
├── mode
├── keys
├── JWT settings
└── namespace policies

Observability
├── logging format
├── log level
├── metrics
└── tracing
```

The goal is to keep the core pipeline stable:

```text
HTTP
  |
  v
Application Service
  |
  +----> Processing
  |
  +----> Storage
               |
               v
            OpenDAL
```

while making deployment-specific behavior configurable.

---

## Possible Future Project Structure

As the service grows, a reasonable direction could be:

```text
src/
├── main.rs
├── config/
│   ├── mod.rs
│   ├── server.rs
│   ├── storage.rs
│   └── processing.rs
│
├── http/
│   ├── router.rs
│   ├── auth.rs
│   └── error.rs
│
└── images/
    ├── model.rs
    ├── service.rs
    ├── validation.rs
    │
    ├── handler/
    │   ├── upload.rs
    │   ├── fetch.rs
    │   └── delete.rs
    │
    ├── processing/
    │   ├── mod.rs
    │   ├── decode.rs
    │   ├── webp.rs
    │   └── resize.rs
    │
    └── store.rs
```

If storage construction becomes sufficiently complex, a dedicated infrastructure module could be introduced:

```text
src/
├── storage.rs
```

or:

```text
src/
├── storage/
│   └── mod.rs
```

That module could construct and configure OpenDAL operators while `images/store.rs` remains concerned only with image object keys and image storage operations.

Provider-specific modules should not be introduced unless they are actually necessary.

---

## Architectural Direction

The current architecture already provides the storage boundary that the original roadmap anticipated.

Future work should preserve this separation:

```text
HTTP
  |
  v
Application Service
  |
  +----> Processing
  |
  +----> ImageStore
             |
             v
          OpenDAL
```

Handlers should remain unaware of storage providers.

Image processing should remain independent of storage.

`ImageStore` should remain concerned with logical image storage rather than provider-specific infrastructure details.

OpenDAL should absorb backend differences wherever possible.

This allows storage capabilities to evolve without collapsing HTTP, processing, and infrastructure concerns into the same modules.

---
