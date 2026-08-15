# Image Service

A small HTTP image service written in Rust with Axum.

The service accepts uploaded images, validates their real format, decodes them with defensive limits, converts them to WebP, progressively reduces quality and dimensions until the configured output-size target is satisfied, and stores the result for later retrieval.

The current implementation uses the local filesystem as its storage backend. The architecture deliberately separates HTTP handling, image processing, application orchestration, validation, and storage so that those pieces can evolve independently.

---

## Features

Current behavior includes:

- Multipart image uploads.
- Public image retrieval over HTTP.
- Optional bearer-token protection for write operations.
- Source MIME type validation against the detected file format.
- Support for JPEG, PNG, WebP, GIF, BMP, and TIFF input.
- WebP output.
- Progressive WebP quality reduction.
- Progressive dimension reduction when quality reduction alone is insufficient.
- Maximum source size protection.
- Decoder width, height, and allocation limits.
- Limited concurrent image-processing jobs.
- Resource-based image organization.
- Up to five image slots per resource.
- Local filesystem persistence.
- Optional public base URL generation.
- JSON error responses.
- A lightweight health endpoint.

---

## API Overview

The service exposes the following endpoints:

```text
GET  /health

GET  /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}
```

Example image identifier:

```text
namespace   = products
resource_id = 845
slot        = 2
```

The corresponding API URL is:

```text
/api/images/products/845/2
```

With the local filesystem backend, the image is stored as:

```text
<image-root>/products/845/02.webp
```

---

## Project Structure

```text
src/
├── main.rs
├── auth.rs
├── config.rs
├── error.rs
├── health.rs
├── router.rs
├── state.rs
│
└── images/
    ├── mod.rs
    ├── model.rs
    ├── multipart.rs
    ├── reduction.rs
    ├── service.rs
    ├── store.rs
    ├── validation.rs
    │
    ├── dtos/
    │   ├── mod.rs
    │   └── upload_response.rs
    │
    └── handler/
        ├── mod.rs
        ├── fetch.rs
        └── upload.rs
```

The layout follows a simple rule:

> HTTP-specific code should remain at the edge of the application. Image processing and storage should not depend on Axum.

---

## Architecture

The current architecture is divided into five main layers.

### HTTP Layer

Files:

```text
router.rs
health.rs
auth.rs
images/handler/
images/multipart.rs
```

Responsibilities:

- Define routes.
- Extract path parameters.
- Read request headers.
- Parse multipart requests.
- Perform write authorization.
- Convert application results into HTTP responses.
- Set response headers.

The HTTP layer should not implement image compression or filesystem operations directly.

### Application / Service Layer

File:

```text
images/service.rs
```

Responsibilities:

- Coordinate the image upload use case.
- Coordinate image retrieval.
- Call the reduction layer.
- Call the storage layer.
- Build the final public image URL.
- Produce response DTOs.

The service layer represents application behavior rather than transport or infrastructure details.

### Image Processing Layer

File:

```text
images/reduction.rs
```

Responsibilities:

- Detect the real image format from bytes.
- Verify that the declared MIME type matches the detected format.
- Apply decoder safety limits.
- Decode source images.
- Encode WebP.
- Reduce WebP quality.
- Resize images when necessary.
- Enforce the final output-size target.
- Move CPU-heavy work away from the async executor.
- Respect the image-processing concurrency limit.

This layer has no knowledge of HTTP routes, namespaces, storage paths, or Axum.

Conceptually:

```text
source bytes + declared MIME
             |
             v
      format validation
             |
             v
           decode
             |
             v
      WebP compression
             |
             v
      resize if needed
             |
             v
        WebP bytes
```

### Storage Layer

File:

```text
images/store.rs
```

Responsibilities:

- Convert an `ImageKey` into a physical path.
- Create required directories.
- Write image bytes.
- Read image bytes.
- Translate filesystem errors into application errors.

The current backend is local storage.

No compression logic belongs in this layer.

### Configuration and Shared State

Files:

```text
config.rs
state.rs
```

Responsibilities:

- Load environment variables.
- Define processing limits.
- Configure storage location.
- Configure authentication.
- Configure concurrency.
- Configure the public base URL.
- Construct dependencies shared by handlers.

---

## Upload Flow

An upload request follows this path:

```text
Client
  |
  | POST multipart/form-data
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
  +----> Encode as WebP
  +----> Reduce quality
  +----> Resize if necessary
  |
  v
Storage Layer
  |
  +----> Create directories
  +----> Write .webp file
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

## Running the Service

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

# Client Examples

The examples below use:

```text
Base URL:    http://127.0.0.1:3000
Namespace:   products
Resource ID: 845
Slot:        1
Image:       ./photo.jpg
```

If API authentication is disabled, remove the `Authorization` header from upload examples.

---

## cURL

### Upload

```bash
curl \
  -X POST \
  "http://127.0.0.1:3000/api/images/products/845/1" \
  -H "Authorization: Bearer change-me" \
  -F "image=@./photo.jpg"
```

Example response:

```json
{
  "namespace": "products",
  "resource_id": "845",
  "slot": 1,
  "url": "/api/images/products/845/1",
  "size_bytes": 193284
}
```

### Fetch

Save the image:

```bash
curl \
  "http://127.0.0.1:3000/api/images/products/845/1" \
  --output product.webp
```

Inspect the headers:

```bash
curl \
  -I \
  "http://127.0.0.1:3000/api/images/products/845/1"
```

---

## JavaScript Fetch API

These examples use the standard Fetch API available in modern browsers and recent Node.js versions.

### Upload from a browser file input

HTML:

```html
<input id="image" type="file" accept="image/*">
```

JavaScript:

```javascript
const input = document.querySelector("#image");
const file = input.files[0];

const form = new FormData();
form.append("image", file);

const response = await fetch(
  "http://127.0.0.1:3000/api/images/products/845/1",
  {
    method: "POST",
    headers: {
      Authorization: "Bearer change-me",
    },
    body: form,
  },
);

if (!response.ok) {
  const error = await response.json();
  throw new Error(error.error);
}

const result = await response.json();

console.log(result);
```

Do not manually set `Content-Type` when sending `FormData`.

The Fetch implementation generates the multipart boundary automatically.

### Fetch an image

```javascript
const response = await fetch(
  "http://127.0.0.1:3000/api/images/products/845/1",
);

if (!response.ok) {
  throw new Error(`HTTP ${response.status}`);
}

const blob = await response.blob();
const objectUrl = URL.createObjectURL(blob);

document.querySelector("img").src = objectUrl;
```

For a public image URL used directly in HTML:

```html
<img
  src="http://127.0.0.1:3000/api/images/products/845/1"
  alt="Product"
/>
```

---

## Java

The following example uses `java.net.http.HttpClient`.

### Upload

```java
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.UUID;

public class UploadImage {
    public static void main(String[] args) throws Exception {
        var client = HttpClient.newHttpClient();

        var imagePath = Path.of("./photo.jpg");
        var imageBytes = Files.readAllBytes(imagePath);

        var boundary = "----ImageServiceBoundary" + UUID.randomUUID();

        var prefix = (
            "--" + boundary + "\r\n" +
            "Content-Disposition: form-data; name=\"image\"; filename=\"photo.jpg\"\r\n" +
            "Content-Type: image/jpeg\r\n\r\n"
        ).getBytes(StandardCharsets.UTF_8);

        var suffix = (
            "\r\n--" + boundary + "--\r\n"
        ).getBytes(StandardCharsets.UTF_8);

        var body = new byte[
            prefix.length + imageBytes.length + suffix.length
        ];

        System.arraycopy(
            prefix,
            0,
            body,
            0,
            prefix.length
        );

        System.arraycopy(
            imageBytes,
            0,
            body,
            prefix.length,
            imageBytes.length
        );

        System.arraycopy(
            suffix,
            0,
            body,
            prefix.length + imageBytes.length,
            suffix.length
        );

        var request = HttpRequest.newBuilder()
            .uri(
                URI.create(
                    "http://127.0.0.1:3000/api/images/products/845/1"
                )
            )
            .header(
                "Authorization",
                "Bearer change-me"
            )
            .header(
                "Content-Type",
                "multipart/form-data; boundary=" + boundary
            )
            .POST(
                HttpRequest.BodyPublishers.ofByteArray(body)
            )
            .build();

        var response = client.send(
            request,
            HttpResponse.BodyHandlers.ofString()
        );

        System.out.println(response.statusCode());
        System.out.println(response.body());
    }
}
```

For larger production clients, a higher-level HTTP library with native multipart support may be preferable.

### Fetch

```java
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Files;
import java.nio.file.Path;

public class FetchImage {
    public static void main(String[] args) throws Exception {
        var client = HttpClient.newHttpClient();

        var request = HttpRequest.newBuilder()
            .uri(
                URI.create(
                    "http://127.0.0.1:3000/api/images/products/845/1"
                )
            )
            .GET()
            .build();

        var response = client.send(
            request,
            HttpResponse.BodyHandlers.ofByteArray()
        );

        if (response.statusCode() != 200) {
            throw new RuntimeException(
                "HTTP " + response.statusCode()
            );
        }

        Files.write(
            Path.of("product.webp"),
            response.body()
        );
    }
}
```

---

## Python

The following examples use the popular `requests` package.

Install it if necessary:

```bash
python -m pip install requests
```

### Upload

```python
from pathlib import Path

import requests

url = (
    "http://127.0.0.1:3000"
    "/api/images/products/845/1"
)

headers = {
    "Authorization": "Bearer change-me",
}

path = Path("./photo.jpg")

with path.open("rb") as image:
    response = requests.post(
        url,
        headers=headers,
        files={
            "image": (
                path.name,
                image,
                "image/jpeg",
            )
        },
        timeout=30,
    )

response.raise_for_status()

print(response.json())
```

### Fetch

```python
from pathlib import Path

import requests

url = (
    "http://127.0.0.1:3000"
    "/api/images/products/845/1"
)

response = requests.get(
    url,
    timeout=30,
)

response.raise_for_status()

Path("product.webp").write_bytes(
    response.content
)
```

---

## Rust Client

The following examples use `reqwest`.

Example dependencies:

```toml
[dependencies]
reqwest = {
    version = "0.12",
    features = [
        "json",
        "multipart",
    ],
}
serde_json = "1"
tokio = {
    version = "1",
    features = ["full"],
}
```

### Upload

```rust
use reqwest::{
    multipart,
    Client,
};

#[tokio::main]
async fn main()
    -> Result<(), Box<dyn std::error::Error>>
{
    let client =
        Client::new();

    let bytes =
        tokio::fs::read(
            "./photo.jpg"
        )
        .await?;

    let part =
        multipart::Part::bytes(bytes)
            .file_name("photo.jpg")
            .mime_str("image/jpeg")?;

    let form =
        multipart::Form::new()
            .part(
                "image",
                part,
            );

    let response =
        client
            .post(
                "http://127.0.0.1:3000/api/images/products/845/1"
            )
            .bearer_auth(
                "change-me"
            )
            .multipart(form)
            .send()
            .await?;

    let status =
        response.status();

    let body =
        response.text().await?;

    println!("{status}");
    println!("{body}");

    Ok(())
}
```

### Fetch

```rust
use reqwest::Client;

#[tokio::main]
async fn main()
    -> Result<(), Box<dyn std::error::Error>>
{
    let client =
        Client::new();

    let response =
        client
            .get(
                "http://127.0.0.1:3000/api/images/products/845/1"
            )
            .send()
            .await?
            .error_for_status()?;

    let bytes =
        response
            .bytes()
            .await?;

    tokio::fs::write(
        "product.webp",
        bytes,
    )
    .await?;

    Ok(())
}
```

---

# Error Responses

Errors are returned as JSON.

Example:

```json
{
  "error": "Image not found"
}
```

Common HTTP statuses include:

```text
400 Bad Request
401 Unauthorized
404 Not Found
413 Payload Too Large
415 Unsupported Media Type
422 Unprocessable Entity
500 Internal Server Error
```

Internal filesystem or application details are not returned directly to clients.

---

# Security Considerations

The service includes several defensive measures:

- Request-body size limits.
- Source-file size limits.
- MIME-to-format verification.
- Maximum decode dimensions.
- Decoder memory-allocation limits.
- Restricted namespace characters.
- Restricted resource-ID characters.
- Bounded image-processing concurrency.
- `X-Content-Type-Options: nosniff` on image responses.
- Optional bearer-token protection for writes.

A production deployment should additionally consider:

- TLS termination.
- Reverse-proxy request limits.
- Rate limiting.
- Authentication and authorization policy.
- Audit logging.
- Structured application logging.
- Metrics.
- Storage quotas.
- Backup policy.
- Network restrictions.
- Secret management rather than long-lived keys in plain environment files.

---

# Design Principles

The project intentionally follows a few architectural rules.

## Handlers stay small

Handlers should deal primarily with HTTP concerns.

They should not contain WebP algorithms, filesystem layout logic, or complex business behavior.

## Processing is transport-independent

The reduction code receives image bytes and metadata and returns processed bytes.

This makes the processing layer usable from:

- HTTP handlers.
- Background jobs.
- CLI tools.
- Batch imports.
- Message-queue consumers.

without rewriting the image algorithm.

## Storage is an infrastructure concern

The application service asks storage to read or write an image.

The rest of the application should not care whether those bytes ultimately live on:

- Local disk.
- Amazon S3.
- MinIO.
- Cloudflare R2.
- Azure Blob Storage.
- Google Cloud Storage.

The current implementation only provides local storage, but the boundary is already present.

## Configuration belongs in one place

Runtime and processing configuration should progressively move into `AppConfig` instead of being scattered through handlers or infrastructure modules.

---

# Future Features

The current implementation is intentionally small.

The long-term direction is to make the service **completely configurable**, including storage, image policy, security, caching, URL generation, and processing behavior.

The items below describe planned architectural directions, not currently implemented features.

---

## Pluggable Storage Backends

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

### Local backend

Possible configuration:

```dotenv
IMAGE_STORAGE_BACKEND=local
IMAGE_SERVICE_DIR=/var/lib/image-service/images
```

Implementation:

```text
store/local.rs
```

### S3 backend

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

### MinIO backend

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

## Fully Configurable Image Policy

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

## Configurable Output Formats

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

## Processing Profiles

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

## More Image Operations

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

## Delete Operations

A future endpoint could support:

```text
DELETE /api/images/{namespace}/{resource_id}/{slot}
```

with optional logical deletion where required by the consuming application.

The storage interface would then expose a delete operation independently of the selected backend.

---

## Resource-Level Operations

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

## Metadata

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

## Cache Configuration

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

## Configurable Authentication

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

## Per-Namespace Policies

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

## Configurable Slot Limits

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

## Observability

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

## Rate Limiting and Quotas

Future deployments may need configurable limits for:

- Requests per IP.
- Requests per API key.
- Uploads per namespace.
- Storage used per namespace.
- Processing CPU budget.
- Daily upload quotas.

---

## Background Processing

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

## Direct-to-Object-Storage Uploads

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

## Database Integration

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

## Deduplication

Possible future support:

```text
SHA-256(source bytes)
```

or a hash of normalized image contents.

This could prevent storing the same image repeatedly.

---

## Content Addressing

An alternative storage strategy could use hashes:

```text
ab/cd/abcdef....webp
```

instead of resource-based paths.

The application layer could then map resource slots to content hashes.

---

## Storage Migration

Once multiple storage backends exist, migration tools could support:

```text
local -> S3
S3 -> local
MinIO -> S3
bucket A -> bucket B
```

without changing application-facing image URLs.

---

## Configuration File Support

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

## Command-Line Configuration

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

## Complete Configuration Goal

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

# Possible Future Project Structure

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

# Status

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
