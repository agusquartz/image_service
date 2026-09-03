[← Back to README](../README.md)

# Architecture

---

This document describes the current project structure, module responsibilities, request flows, and architectural principles.

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

---

## Layer Responsibilities

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

The HTTP layer should not implement image compression or storage backend operations directly.

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

This layer has no knowledge of HTTP routes, namespaces, storage backends, or Axum.

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

- Convert ImageKey into a backend-independent object key.
- Read image bytes through OpenDAL.
- Write image bytes through OpenDAL.
- Translate storage backend errors into application errors.

Example object key:

```text
products/845/01.webp
```

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
- Configure the storage backend and backend-specific options.
- Configure authentication.
- Configure concurrency.
- Configure the public base URL.
- Construct dependencies shared by handlers.

---

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
