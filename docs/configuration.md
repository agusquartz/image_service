[← Back to README](../README.md)

# Configuration

Image Service combines runtime environment configuration with compile-time image-processing policy constants.

Runtime values are loaded from the environment. Image-processing limits such as output size, resize rounds, WebP qualities, and decoder limits currently live in `src/config.rs` and are intended to become fully runtime-configurable over time.

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

The service currently supports slots `1` through `5`.

Uploading to an existing slot replaces the previous image.

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

Valid slots currently range from:

```text
1..=5
```

Slots use two-digit formatting in storage object keys:

```text
1 -> 01.webp
2 -> 02.webp
5 -> 05.webp
```

---

## Image Processing Strategy

The service attempts to produce a WebP file no larger than:

```text
400 KB
```

The current algorithm starts with the original dimensions and tries the following WebP quality levels:

```text
88
80
72
64
56
48
42
```

If none of those results fits within the target size, the image dimensions are reduced to approximately:

```text
85%
```

of the previous width and height.

The quality sequence is then attempted again.

This process continues until:

- The encoded image satisfies the size target.
- The resize-round limit is reached.
- The longest side reaches the configured minimum.

Current relevant limits are:

```text
Maximum HTTP request:         16 MB
Maximum source image:         15 MB
Maximum output image:        400 KB
Maximum dimension:         12,000 px
Maximum decoder allocation:  256 MB
Minimum longest side:        512 px
Maximum resize rounds:        10
```

These values are currently compile-time configuration constants.

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
```

This is particularly important when processing large source images because several concurrent decodes may consume significant CPU and memory.

Storage operations are handled asynchronously through OpenDAL and are independent of the image-processing semaphore.

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

GET image requests remain public in the current implementation.

If no API key is configured, uploads are accepted without authentication.

Production deployments should make this choice deliberately.

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
