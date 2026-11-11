[← Back to README](../README.md)

# Configuration

Image Service combines runtime environment configuration with a small set of compile-time safety limits. Upload byte limits, output dimensions, and resize behavior are configurable through environment variables.

Storage is provided through OpenDAL. The currently compiled storage backends are:

- Local filesystem.
- S3.
- S3-compatible object storage such as MinIO and Cloudflare R2.

---

## Resource Organization

Images are grouped using three values:

```text
namespace
resource_id
slot
```

For example:

```text
products / 845 / 1
products / 845 / 2
users    / 42  / 1
articles / abc / 1
```

These values are converted into a backend-independent storage object key.

For example:

```text
products/845/01.webp
products/845/02.webp
users/42/01.webp
articles/abc/01.webp
```

The same logical object key is used regardless of whether the configured backend is a local filesystem or S3-compatible object storage.

With local filesystem storage, the resulting layout looks like:

```text
<root>/
├── products/
│   └── 845/
│       ├── 01.webp
│       └── 02.webp
│
├── users/
│   └── 42/
│       └── 01.webp
│
└── articles/
    └── abc/
        └── 01.webp
```

Slot numbers are one-based values from `1` through `255`. `IMAGE_MAX_SLOTS` defaults to `5` and limits the highest slot number that may be created; it does not hide existing higher-numbered slots.

`POST` creates an image only in an empty slot and within the configured limit. `PUT` creates within the limit or replaces an existing image, including one above the limit.

---

## Validation Rules

### Namespace

A namespace:

- Must not be empty.
- Must not exceed 32 characters.
- May contain lowercase ASCII letters.
- May contain numbers.
- May contain `-`.
- May contain `_`.

Examples:

```text
products
product-images
product_images
catalog2026
```

Uppercase names are deliberately rejected so values such as:

```text
product
Product
PRODUCT
```

cannot become separate storage namespaces.

### Resource ID

A resource ID:

- Must not be empty.
- Must not exceed 100 characters.
- May contain ASCII letters.
- May contain numbers.
- May contain `-`.
- May contain `_`.

Examples:

```text
845
product-845
01JABCXYZ
external_id_42
```

Path separators and arbitrary storage-key characters are not accepted.

This prevents route values from changing the intended storage hierarchy.

### Slot

Slot numbers are one-based `u8` values (`1..=255`). `IMAGE_MAX_SLOTS` defaults to `5` and sets the highest slot number allowed when creating a new image. It is global, not namespace-specific.

The setting does not hide or block access to existing slots above the configured value. GET, HEAD, image info, presigned reads, resource listings, and DELETE continue to work for those images. PUT may replace an existing high-numbered slot, but returns `400 Bad Request` if that slot does not exist and would have to be created. POST above the configured maximum returns `400 Bad Request`.

For example, allowing new images in slots 1 through 12:

```text
IMAGE_MAX_SLOTS=12
```

Slots use two-digit formatting in storage object keys:

```text
1 -> 01.webp
2 -> 02.webp
5 -> 05.webp
12 -> 12.webp
```

---

## Image Processing Strategy

The service targets the configured WebP dimensions and enforces a maximum encoded byte size. Defaults are:

```text
1024 × 1024 px; 100 KiB
```

The source is center-cropped to the target aspect ratio and resampled to the configured target dimensions. This crops the sides when needed and never distorts the image by changing its aspect ratio. The same resampled pixels are encoded repeatedly at different lossy WebP qualities; a binary search selects the highest quality that fits the byte limit. If quality 0 still exceeds the limit, the service reduces dimensions in further steps, always resizing from the original cropped pixels and keeping the target aspect ratio.

WebP quality can be reduced all the way to `0`. The service reduces dimensions only when needed to fit the byte limit, and returns `422` only if the configured limit is too small even for a 1×1 WebP image.

The default processing limits are:

```text
Maximum source image:         15 MiB
Maximum output image:        100 KiB
Target output dimensions:    1024 × 1024 px
Maximum decoder dimension:   12,000 px
Maximum decoder allocation:  256 MiB
```

Input and output byte limits, target output dimensions, and the global slot maximum are runtime-configurable. Decoder dimension and allocation limits remain compile-time safety bounds.

### Per-namespace processing profiles

Processing profiles are optional. If neither profile variable is configured, processing remains exactly on the existing global policy. Profiles currently support WebP output and can override the input byte limit, output byte limit, and target dimensions for selected namespaces. Every omitted profile field inherits its matching global setting (`IMAGE_MAX_INPUT_BYTES`, `IMAGE_MAX_OUTPUT_BYTES`, `IMAGE_OUTPUT_WIDTH`, or `IMAGE_OUTPUT_HEIGHT`). Each global setting in turn uses its built-in default when unset. Slot maximum and processing concurrency remain global.

Configure profile definitions and namespace assignments as JSON objects:

```dotenv
IMAGE_PROCESSING_PROFILES='{"product":{"max_input_bytes":18874368,"max_output_bytes":409600,"output_width":2000,"output_height":2000},"avatar":{"max_output_bytes":102400,"output_width":512,"output_height":512}}'
IMAGE_NAMESPACE_PROFILES='{"products":"product","users":"avatar"}'
```

The first object maps profile names to partial processing policies. The second maps a validated namespace to a profile name. For any field the precedence is: profile value, then the matching global environment setting, then the built-in default. For example, an unset input limit defaults to 15 MiB; setting `IMAGE_MAX_INPUT_BYTES=17825792` makes the global value 17 MiB; a profile with `max_input_bytes: 18874368` raises only its assigned namespaces to 18 MiB. A profile without `max_input_bytes` inherits the global value. The HTTP body limit accounts for the largest input limit assigned to any namespace plus multipart overhead; the selected namespace's source-image limit is checked separately. Namespaces without an assignment keep the global policy and report `"profile": "default"` in image information. Profile names use lowercase letters, digits, `-`, and `_`; `default` is reserved. Profile width and height must be set together and each must be `1..=12000`; input and output byte limits must be positive. Invalid JSON, unknown profile fields, invalid identifiers, or references to undefined profiles reject startup. Changing a profile affects future uploads; stored images are not automatically reprocessed.

---

## Concurrency Model

Image decoding, resizing, and WebP encoding are CPU-intensive operations.

Running these directly inside an asynchronous task could block Tokio worker threads and degrade the HTTP service.

The service therefore uses:

```rust
tokio::task::spawn_blocking(...)
```

for CPU-heavy image work.

A semaphore limits how many image-processing jobs may execute simultaneously:

```dotenv
IMAGE_MAX_CONCURRENCY=2
IMAGE_MAX_INPUT_BYTES=15728640
IMAGE_MAX_OUTPUT_BYTES=102400
IMAGE_OUTPUT_WIDTH=1024
IMAGE_OUTPUT_HEIGHT=1024
IMAGE_MAX_SLOTS=5
```

This is particularly important when processing large source images because several concurrent decodes may consume significant CPU and memory.

Storage operations are handled asynchronously through OpenDAL and are independent of the image-processing semaphore.

## Authentication

`IMAGE_API_KEY` optionally protects write operations (`POST`, `PUT`, and `DELETE`). When configured, clients send:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

Image reads remain public by default for compatibility with existing clients. Set `GETMETHOD_AUTH=true` to also require the same bearer token for `GET`, `HEAD`, resource listing, image information, and presigned URL creation. `GETMETHOD_AUTH` defaults to `false`; startup fails if it is enabled without `IMAGE_API_KEY`.

## Cache and Presigned Read URLs

Image GET and HEAD responses use a 60-second cache lifetime by default. Public reads get `Cache-Control: public, max-age=60`; when `GETMETHOD_AUTH=true`, the default is `private, max-age=60` so shared caches cannot expose authenticated images. Set `IMAGE_CACHE_TTL_SECONDS` to change the lifetime; `0` emits `Cache-Control: no-store`. `IMAGE_CACHE_CONTROL` overrides the complete header, so do not set it to `public` for protected images unless a shared-cache design explicitly accounts for authorization. A fresh cached response may remain visible for the configured TTL after a PUT. ETag and Last-Modified let a cache validate after the response becomes stale; they do not invalidate a still-fresh response. Avoid `immutable` while a slot can be overwritten at the same URL.

The current GET implementation buffers the stored WebP before responding. With the default 100 KiB output limit, that is usually simpler than streaming. If deployments raise the output limit substantially or expect many concurrent large downloads, consider an optional `IMAGE_STREAM_GET` setting, disabled by default, backed by OpenDAL's reader stream and Axum's streaming response body. This setting is a future recommendation, not implemented yet. Streaming can bound per-request buffering, but memory still depends on stream buffers and concurrent requests; errors that occur after response headers are sent also terminate the response rather than becoming a normal JSON error.

For a storefront or other public image catalog, the 60-second public default lets shared caches and CDNs reuse popular images during traffic spikes. A replacement at the same URL can remain stale for up to that minute. For private ERP images, the default is `private` and only user-agent caches may reuse them; use `no-store` for sensitive content. A versioned URL avoids that freshness tradeoff by changing the URL whenever the image is replaced, so each URL continues to identify the same bytes.

`IMAGE_PRESIGN_TTL_SECONDS` controls the validity period of presigned read URLs. It defaults to `3600` seconds and accepts values from `1` through `604800` seconds. Presigning only works when the configured OpenDAL backend supports `presign_read`; filesystem storage returns `501 Not Implemented`.

---

## Runtime Configuration

Copy `.env.example` to `.env` if desired:

```bash
cp .env.example .env
```

A minimal local configuration is:

```dotenv
IMAGE_BIND=127.0.0.1:3000

IMAGE_STORAGE_SCHEME=fs

IMAGE_MAX_CONCURRENCY=2

# IMAGE_API_KEY=change-me

# IMAGE_PUBLIC_BASE_URL=https://images.example.com
```

### `IMAGE_BIND`

Address on which the service listens.

Default:

```text
127.0.0.1:3000
```

Example:

```dotenv
IMAGE_BIND=0.0.0.0:3000
```

---

## Storage Configuration

Storage configuration uses two concepts:

```text
IMAGE_STORAGE_SCHEME
IMAGE_STORAGE_*
```

`IMAGE_STORAGE_SCHEME` selects the OpenDAL backend.

All other environment variables beginning with:

```text
IMAGE_STORAGE_
```

are converted into backend-specific OpenDAL configuration options.

For example:

```text
IMAGE_STORAGE_BUCKET
```

becomes:

```text
bucket
```

and:

```text
IMAGE_STORAGE_ACCESS_KEY_ID
```

becomes:

```text
access_key_id
```

Empty storage configuration values are ignored.

### `IMAGE_STORAGE_SCHEME`

Selects the OpenDAL storage backend.

Default:

```dotenv
IMAGE_STORAGE_SCHEME=fs
```

Currently compiled schemes:

```text
fs
s3
```

S3-compatible services also use:

```dotenv
IMAGE_STORAGE_SCHEME=s3
```

with a custom endpoint when required.

---

## Filesystem Storage

Basic configuration:

```dotenv
IMAGE_STORAGE_SCHEME=fs
```

If no root is explicitly configured, Image Service uses the operating system's local application-data directory.

Typical defaults are:

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

### `IMAGE_STORAGE_ROOT`

Optional filesystem storage root.

Example:

```dotenv
IMAGE_STORAGE_ROOT=/var/lib/image-service/images
```

The service process must have permission to create directories and write files below this location.

With:

```dotenv
IMAGE_STORAGE_ROOT=/var/lib/image-service/images
```

an image such as:

```text
products/845/01.webp
```

is physically stored as:

```text
/var/lib/image-service/images/products/845/01.webp
```

### `IMAGE_STORAGE_ATOMIC_WRITE_DIR`

Optional temporary directory used by OpenDAL for atomic filesystem writes.

When filesystem storage is selected and this option is omitted, Image Service automatically uses:

```text
<IMAGE_STORAGE_ROOT>/.tmp
```

For example:

```text
/var/lib/image-service/images/.tmp
```

It can be overridden explicitly:

```dotenv
IMAGE_STORAGE_ATOMIC_WRITE_DIR=/var/lib/image-service/tmp
```

Atomic filesystem writes ensure that readers observe a complete previous image or a complete replacement image rather than a partially written destination file.

This option applies to filesystem storage and should not normally be configured for S3.

---

## S3 Storage

S3 storage uses the same `ImageStore` and logical object keys as filesystem storage.

Example:

```dotenv
IMAGE_STORAGE_SCHEME=s3

IMAGE_STORAGE_BUCKET=my-images
IMAGE_STORAGE_REGION=us-east-1

IMAGE_STORAGE_ACCESS_KEY_ID=...
IMAGE_STORAGE_SECRET_ACCESS_KEY=...
```

An image still uses the logical key:

```text
products/845/01.webp
```

but is stored as an object inside the configured bucket rather than as a local file.

Common options include:

```text
IMAGE_STORAGE_BUCKET
IMAGE_STORAGE_REGION
IMAGE_STORAGE_ENDPOINT
IMAGE_STORAGE_ACCESS_KEY_ID
IMAGE_STORAGE_SECRET_ACCESS_KEY
```

The exact available options depend on the OpenDAL S3 backend.

Storage credentials are secrets and must never be committed to the repository.

---

## S3-Compatible Storage

S3-compatible services use the `s3` scheme with an appropriate endpoint.

Generic example:

```dotenv
IMAGE_STORAGE_SCHEME=s3

IMAGE_STORAGE_BUCKET=my-images
IMAGE_STORAGE_REGION=us-east-1
IMAGE_STORAGE_ENDPOINT=https://s3-compatible.example.com

IMAGE_STORAGE_ACCESS_KEY_ID=...
IMAGE_STORAGE_SECRET_ACCESS_KEY=...
```

This approach can be used with services such as:

- MinIO.
- Cloudflare R2.
- Other S3-compatible object stores.

Provider-specific values such as region, endpoint, and credential configuration depend on the selected service.

---

## Other Runtime Options

### `IMAGE_MAX_CONCURRENCY`

Maximum number of CPU-heavy image-processing operations that may execute concurrently.

Default:

```text
2
```

Example:

```dotenv
IMAGE_MAX_CONCURRENCY=4
```

Invalid, empty, or zero values fall back to the default.

### `IMAGE_MAX_INPUT_BYTES`

Maximum source image payload size accepted by upload methods (`POST` and `PUT`), in bytes. The multipart request limit adds 1 MiB for headers and framing. Must be a positive integer.

Default: `15728640` bytes (15 MiB), preserving the previous limit.

### `IMAGE_MAX_OUTPUT_BYTES`

Maximum size of the processed WebP object stored by the service, in bytes. Lossy quality is binary-searched from 100 to 0 using the same resampled image pixels. Must be a positive integer.

Default: `102400` bytes (100 KiB).

### `IMAGE_OUTPUT_WIDTH` and `IMAGE_OUTPUT_HEIGHT`

Exact output width and height in pixels. The service center-crops the input to match this aspect ratio and resamples once to these dimensions without distortion. Each value must be between `1` and `12000`, the decoder's compile-time safety bound.

Defaults: `1024` pixels for both width and height.

### `IMAGE_PROCESSING_PROFILES` and `IMAGE_NAMESPACE_PROFILES`

Optional JSON objects that define named WebP processing profiles and assign them to namespaces. Profiles may set `max_input_bytes`, `max_output_bytes`, `output_width`, and `output_height`. Omitted values inherit the corresponding global environment setting, which falls back to its built-in default when unset. See [Per-namespace processing profiles](#per-namespace-processing-profiles) for a complete example and validation behavior.

### `IMAGE_API_KEY`

Optional bearer token protecting write operations.

Example:

```dotenv
IMAGE_API_KEY=change-me
```

When configured, upload requests must contain:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

Image reads (`GET`, `HEAD`, and resource listing) remain public unless `GETMETHOD_AUTH=true`.

If no API key is configured, uploads are accepted without authentication.

Production deployments should make this choice deliberately.

### `GETMETHOD_AUTH`

When set to `true`, require `Authorization: Bearer <IMAGE_API_KEY>` for image GET, HEAD, resource-list, image-info, and presign requests. It defaults to `false` for backwards compatibility. Startup fails if it is enabled without `IMAGE_API_KEY`.

### `IMAGE_PRESIGN_TTL_SECONDS`

Lifetime in seconds for presigned GET URLs. Defaults to `3600`; valid values are `1..=604800`.

### `IMAGE_CACHE_CONTROL`

Optional full Cache-Control override for image GET and HEAD responses. By default, cache lifetime comes from `IMAGE_CACHE_TTL_SECONDS` (60 seconds); responses use `public` when reads are unauthenticated and `private` when `GETMETHOD_AUTH=true`. Do not use `immutable` while slot URLs can be overwritten.

### `IMAGE_CACHE_TTL_SECONDS`

Cache lifetime in seconds for image GET and HEAD responses. Defaults to `60`. Set to `0` to emit `Cache-Control: no-store`. `IMAGE_CACHE_CONTROL`, when set, overrides this generated policy.

### `IMAGE_PUBLIC_BASE_URL`

Optional public origin used to create image URLs.

Example:

```dotenv
IMAGE_PUBLIC_BASE_URL=https://images.example.com
```

Without this option:

```text
/api/images/products/845/1
```

With this option:

```text
https://images.example.com/api/images/products/845/1
```

A trailing `/` in the configured value is removed automatically.

---

## Storage Configuration Summary

Local filesystem:

```dotenv
IMAGE_STORAGE_SCHEME=fs

# Optional
IMAGE_STORAGE_ROOT=/var/lib/image-service/images

# Optional
IMAGE_STORAGE_ATOMIC_WRITE_DIR=/var/lib/image-service/images/.tmp
```

Amazon S3:

```dotenv
IMAGE_STORAGE_SCHEME=s3
IMAGE_STORAGE_BUCKET=my-images
IMAGE_STORAGE_REGION=us-east-1

IMAGE_STORAGE_ACCESS_KEY_ID=...
IMAGE_STORAGE_SECRET_ACCESS_KEY=...
```

S3-compatible:

```dotenv
IMAGE_STORAGE_SCHEME=s3
IMAGE_STORAGE_BUCKET=my-images
IMAGE_STORAGE_REGION=us-east-1
IMAGE_STORAGE_ENDPOINT=https://s3-compatible.example.com

IMAGE_STORAGE_ACCESS_KEY_ID=...
IMAGE_STORAGE_SECRET_ACCESS_KEY=...
```

The HTTP API and logical image identifiers do not change when switching storage backends.

---
