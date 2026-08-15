# Roadmap

[← Back to README](README.md)

Image Service is intentionally small today. The long-term goal is to make deployment-specific behavior configurable while preserving a stable core pipeline:

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

The items below are planned directions, not promises that the functionality is already implemented.

## Future Features

The current implementation is intentionally small.

The long-term direction is to make the service **completely configurable**, including storage, image policy, security, caching, URL generation, and processing behavior.

The items below describe planned architectural directions, not currently implemented features.

---

### Pluggable Storage Backends

The current:

```text
images/store.rs
```

can evolve into:

```text
images/
    store/
        mod.rs
        local.rs
        s3.rs
        minio.rs
        r2.rs
```

A common interface could look conceptually like:

```rust
trait ImageStore {
    async fn write(
        &self,
        key: &ImageKey,
        bytes: &[u8],
    ) -> Result<(), StoreError>;

    async fn read(
        &self,
        key: &ImageKey,
    ) -> Result<Vec<u8>, StoreError>;

    async fn delete(
        &self,
        key: &ImageKey,
    ) -> Result<(), StoreError>;
}
```

Configuration could then select the backend:

```dotenv
IMAGE_STORAGE_BACKEND=local
```

or:

```dotenv
IMAGE_STORAGE_BACKEND=s3
```

#### Local backend

Possible configuration:

```dotenv
IMAGE_STORAGE_BACKEND=local
IMAGE_SERVICE_DIR=/var/lib/image-service/images
```

Implementation:

```text
store/local.rs
```

#### S3 backend

Possible configuration:

```dotenv
IMAGE_STORAGE_BACKEND=s3

IMAGE_S3_BUCKET=my-images
IMAGE_S3_REGION=us-east-1
IMAGE_S3_PREFIX=images/
```

Implementation:

```text
store/s3.rs
```

Authentication should preferably use the cloud provider's standard credential chain rather than custom credential parsing where possible.

#### MinIO backend

Possible configuration:

```dotenv
IMAGE_STORAGE_BACKEND=minio

IMAGE_S3_ENDPOINT=http://minio:9000
IMAGE_S3_BUCKET=images
IMAGE_S3_REGION=us-east-1
IMAGE_S3_FORCE_PATH_STYLE=true
```

Because MinIO is S3-compatible, the S3 implementation may eventually support both rather than requiring a completely independent backend.

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

with optional logical deletion where required by the consuming application.

The storage interface would then expose a delete operation independently of the selected backend.

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

- Sidecar JSON files.
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

The current API processes the image synchronously before returning success.

A future optional mode could support queued processing:

```text
POST upload
    |
    v
object storage
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

This could be useful for very large workloads while preserving the current synchronous mode for simpler deployments.

---

### Direct-to-Object-Storage Uploads

For S3-compatible backends, a future architecture may support presigned uploads:

```text
Client
  |
  +----> Image Service: request upload authorization
  |
  <----+ Presigned URL
  |
  +----> S3 / MinIO directly
```

A worker or callback could then process the source image.

This avoids routing very large source files through the application server.

---

### Database Integration

The image service currently does not require a database.

A future optional database layer could support:

- Image metadata.
- Ownership.
- Audit trails.
- Logical deletion.
- Processing state.
- Deduplication.
- Reference counting.
- Expiration policies.

The image binary itself should still preferably live in an object store or filesystem rather than a relational database unless a deployment has a specific reason to do otherwise.

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

instead of resource-based paths.

The application layer could then map resource slots to content hashes.

---

### Storage Migration

Once multiple storage backends exist, migration tools could support:

```text
local -> S3
S3 -> local
MinIO -> S3
bucket A -> bucket B
```

without changing application-facing image URLs.

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
backend = "s3"

[storage.s3]
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
├── backend
├── path / bucket
├── region
├── endpoint
├── prefix
└── credentials strategy

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
```

while making the concrete implementations and policies configurable.

---

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
    └── store/
        ├── mod.rs
        ├── local.rs
        ├── s3.rs
        └── minio.rs
```

This structure should only be introduced as complexity actually requires it. The current flatter design is intentionally simpler.

---

---

## Current Status

Current:

```text
HTTP upload        yes
HTTP fetch         yes
Local storage      yes
WebP reduction     yes
Optional API key   yes
S3 storage         no
MinIO storage      no
Delete endpoint    no
Database           no
Processing profiles no
Runtime-configurable processing policy partial
```

The architecture is intended to allow those capabilities to be added without collapsing HTTP, processing, and storage concerns into the same modules.
