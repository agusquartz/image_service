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

Local filesystem storage       yes
S3 storage                     yes
S3-compatible storage          yes

OpenDAL storage abstraction    yes
Runtime-selectable storage     yes
Atomic filesystem writes       yes

WebP reduction                 yes
Optional API key               yes

Delete endpoint                no
Database                       no
Processing profiles            no
Runtime processing policy      partial
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

### Fully Configurable Image Policy

Values that are currently compile-time constants could become runtime configuration.

For example:

```dotenv
IMAGE_MAX_REQUEST_MB=16
IMAGE_MAX_SOURCE_MB=15
IMAGE_MAX_OUTPUT_KB=400

IMAGE_MAX_WIDTH=12000
IMAGE_MAX_HEIGHT=12000
IMAGE_MAX_DECODE_MEMORY_MB=256

IMAGE_MIN_LONGEST_SIDE=512
IMAGE_RESIZE_SCALE=0.85
IMAGE_MAX_RESIZE_ROUNDS=10
```

WebP qualities could also become configurable:

```dotenv
IMAGE_WEBP_QUALITIES=88,80,72,64,56,48,42
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

Different namespaces could eventually use different processing profiles.

For example:

```text
products -> WebP, max 400 KB
avatars  -> WebP, max 120 KB, square crop
banners  -> AVIF, max width 1920
```

---

### Processing Profiles

Instead of one global policy, configuration may define named profiles:

```text
product
thumbnail
avatar
hero
original
```

Conceptually:

```toml
[profiles.product]
format = "webp"
max_output_kb = 400
max_width = 2000

[profiles.avatar]
format = "webp"
max_output_kb = 100
width = 512
height = 512
fit = "cover"
```

Namespaces or routes could then choose a profile.

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

A future endpoint could support:

```text
DELETE /api/images/{namespace}/{resource_id}/{slot}
```

The storage layer could delegate deletion to OpenDAL independently of the selected backend.

Logical deletion could be introduced separately if ownership, auditing, or retention requirements eventually require metadata persistence.

---

### Resource-Level Operations

Potential APIs:

```text
GET    /api/images/{namespace}/{resource_id}
DELETE /api/images/{namespace}/{resource_id}
```

These could:

- List existing slots.
- Return image metadata.
- Remove every image belonging to a resource.

Any listing behavior should account for differences between filesystem and object-storage semantics while keeping those differences outside the HTTP and application layers.

---

### Metadata

A future version may persist metadata such as:

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

HTTP caching could become configurable.

For example:

```dotenv
IMAGE_CACHE_CONTROL=public,max-age=31536000,immutable
```

Other planned options may include:

- ETag generation.
- Last-Modified.
- Conditional requests.
- CDN-aware cache headers.
- Immutable versioned image URLs.

---

### Configurable Authentication

The current write authentication model is a single optional bearer token.

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
- Maximum source size.
- Maximum output size.
- Number of slots.
- Processing profile.
- Public/private read access.
- Storage backend.
- Storage prefix.
- Retention rules.

---

### Configurable Slot Limits

The current global limit is:

```text
1..=5
```

A future configuration could expose:

```dotenv
IMAGE_MAX_SLOTS=5
```

or namespace-specific values.

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
