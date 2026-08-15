[← Back to README](../README.md)

# Configuration

Image Service currently combines runtime environment configuration with compile-time image-policy constants.

Runtime values are loaded from the environment. Image-processing limits such as output size, resize rounds, and decoder limits currently live in `src/config.rs` and are intended to become fully runtime-configurable over time.

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

With local storage:

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

Uppercase names are deliberately rejected so that values such as:

```text
product
Product
PRODUCT
```

cannot become separate directories on case-sensitive filesystems.

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

Path separators and arbitrary filesystem characters are not accepted.

### Slot

Valid slots currently range from:

```text
1..=5
```

---

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
Maximum HTTP request:        16 MB
Maximum source image:        15 MB
Maximum output image:       400 KB
Maximum dimension:        12,000 px
Maximum decoder allocation: 256 MB
Minimum longest side:       512 px
Maximum resize rounds:       10
```

These values are currently compile-time configuration constants.

---

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

---

---

## Configuration

Copy `.env.example` to `.env` if desired.

```dotenv
IMAGE_BIND=127.0.0.1:3000

# IMAGE_SERVICE_DIR=/var/lib/image-service/images

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

### `IMAGE_SERVICE_DIR`

Overrides the local image-storage directory.

If omitted, the service uses the operating system's local application-data directory.

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

### `IMAGE_MAX_CONCURRENCY`

Maximum number of CPU-heavy image-processing operations that may execute concurrently.

Default:

```text
2
```

### `IMAGE_API_KEY`

Optional bearer token protecting write operations.

When configured, upload requests must contain:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

GET requests remain public in the current implementation.

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

---
