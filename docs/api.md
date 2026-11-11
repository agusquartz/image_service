[← Back to README](../README.md)

# HTTP API

This document describes the public HTTP surface of Image Service.

## API Overview

The service exposes the following endpoints:

```text
GET  /health

GET  /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}/{slot}/info
HEAD /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}
PUT  /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}/presign
DELETE /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}
DELETE /api/images/{namespace}/{resource_id}
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

The corresponding backend-independent storage key is:

```text
products/845/02.webp
```

With the filesystem backend, this resolves to:

```text
<image-root>/products/845/02.webp
```

With S3 storage, `products/845/02.webp` is used as the object key.

---

## Upload Image

```text
POST  /api/images/{namespace}/{resource_id}/{slot}
PUT   /api/images/{namespace}/{resource_id}/{slot}
```

Both methods accept the same complete-image `multipart/form-data` request with an image field named `image`:

```text
image
```

If write authentication is enabled, include:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

The uploaded image payload is limited by `IMAGE_MAX_INPUT_BYTES` (15 MiB by default). The complete multipart request may include up to 1 MiB of framing overhead beyond that limit. The converted WebP is constrained by `IMAGE_MAX_OUTPUT_BYTES` (100 KiB by default) and targets the `IMAGE_OUTPUT_WIDTH` × `IMAGE_OUTPUT_HEIGHT` dimensions (1024×1024 by default). It center-crops to the target aspect ratio without distortion. Lossy WebP quality is searched from 100 down to 0. If quality 0 at the configured dimensions still exceeds the byte limit, the service reduces dimensions in further steps, preserving the target aspect ratio and always resizing from the same cropped source pixels.

`POST` creates an image only when that slot is empty and returns `201 Created`. If the slot already contains an image, it returns `409 Conflict` and leaves the stored image unchanged. `PUT` creates a missing image with `201 Created` or replaces an existing image with `200 OK`; use it when replacing a slot. A successful upload stays within the configured output byte limit. The service can return `422 Unprocessable Entity` only if the configured byte limit is too small even for a 1×1 WebP image.

There is no `PATCH` upload method because uploads contain the complete image file; this API has no partial image representation to patch.

The service validates the route parameters, reads the multipart image, verifies the source format, processes the image, and stores the resulting WebP in the requested slot.

A successful response looks like:

```json
{
  "namespace": "products",
  "resource_id": "845",
  "slot": 2,
  "url": "/api/images/products/845/2",
  "size_bytes": 183421
}
```

If `IMAGE_PUBLIC_BASE_URL` is configured, `url` is absolute.

Repeated `POST` requests do not replace an image; use `PUT` to replace it.

---

## Fetch Image

```text
GET /api/images/{namespace}/{resource_id}/{slot}
```

Successful responses return the stored WebP bytes.

Current response headers include:

```http
Content-Type: image/webp
X-Content-Type-Options: nosniff
ETag: "..."
Last-Modified: ...
```

Reads are public by default for backwards compatibility. Set `GETMETHOD_AUTH=true` to require the configured `IMAGE_API_KEY` bearer token for GET, HEAD, resource listing, image information, and presigned URL creation.

GET and HEAD return `Cache-Control` with a 60-second default lifetime. Responses are `public` when `GETMETHOD_AUTH=false` and `private` when reads require authentication. `IMAGE_CACHE_TTL_SECONDS=0` sets `no-store`; `IMAGE_CACHE_CONTROL` can override the complete policy. GET supports `If-None-Match` and `If-Modified-Since`, returning `304 Not Modified` when the stored representation has not changed. ETag falls back to a SHA-256 content validator when the storage backend does not provide one. A fresh cached response can remain in use for up to its TTL after a PUT; ETag does not force an earlier check. `immutable` is unsuitable while uploads can replace content at the same URL.

## Image Information

```text
GET /api/images/{namespace}/{resource_id}/{slot}/info
```

Returns storage size and available validators plus the output format and dimensions. The current built-in processing policy is reported as `default`.

```json
{
  "namespace": "products",
  "resource_id": "845",
  "slot": 2,
  "size_bytes": 183421,
  "format": "webp",
  "width": 1024,
  "height": 1024,
  "profile": "default",
  "etag": "\"a9f2...\"",
  "last_modified": "Mon, 09 Nov 2026 12:30:00 GMT"
}
```

---

## List Images for a Resource

```text
GET /api/images/{namespace}/{resource_id}
```

Returns the existing WebP slots in ascending order. Each entry includes its URL, byte size, and available ETag and Last-Modified metadata. Use the per-image `/info` endpoint for dimensions and complete image metadata. An empty resource returns `200 OK` with an empty `images` array.
The response includes `max_creatable_slot`, the current `IMAGE_MAX_SLOTS` ceiling. It does not mean that slot is vacant; POST to an occupied slot still returns `409 Conflict`. Existing slots above the ceiling are still listed and remain readable/deletable.

```json
{
  "namespace": "products",
  "resource_id": "845",
  "max_creatable_slot": 5,
  "images": [
    {
      "slot": 1,
      "url": "/api/images/products/845/1",
      "size_bytes": 183421,
      "etag": "\"a9f2...\"",
      "last_modified": "Mon, 09 Nov 2026 12:30:00 GMT"
    }
  ]
}
```

Listing uses OpenDAL's prefix listing and metadata, so the HTTP and application layers behave the same with LocalFS and S3-compatible storage.

## Check an Image

```text
HEAD /api/images/{namespace}/{resource_id}/{slot}
```

Returns `200 OK`, `Content-Type: image/webp`, and `Content-Length` when the image exists. The response has no body. Available `ETag` and `Last-Modified` validators are returned. Conditional HEAD requests can return `304 Not Modified`. Missing images return `404 Not Found`.

## Delete an Image

```text
DELETE /api/images/{namespace}/{resource_id}/{slot}
```

If write authentication is enabled, include `Authorization: Bearer <IMAGE_API_KEY>`. Deleting an existing or missing slot returns `204 No Content`, making the operation idempotent.

## Delete a Resource

```text
DELETE /api/images/{namespace}/{resource_id}
```

Requires write authentication when `IMAGE_API_KEY` is configured. It recursively removes every object under the exact resource prefix and returns `204 No Content`, including when the resource is empty or already deleted. The prefix boundary prevents deleting similarly named resources. OpenDAL handles filesystem directory and object-storage prefix semantics behind the storage layer.

## Presigned Read URL

```text
POST /api/images/{namespace}/{resource_id}/{slot}/presign
```

Returns a short-lived URL that allows a client to GET the stored image directly from object storage:

```json
{
  "method": "GET",
  "url": "https://storage.example.com/...",
  "headers": {},
  "expires_in": 3600
}
```

The expiration is configured with `IMAGE_PRESIGN_TTL_SECONDS` (default `3600`, maximum `604800`). Presigned reads require the backend to support OpenDAL's `presign_read` capability. Filesystem storage returns `501 Not Implemented`; no presigned write URL is provided.

---

## Health

```text
GET /health
```

Expected successful status:

```text
204 No Content
```

The health endpoint is intentionally lightweight and only indicates that the HTTP service is running.

---

## Error Responses

Errors are returned as JSON.

Example:

```json
{
  "error": "Image not found"
}
```

Common HTTP statuses include:

```text
201 Created
200 OK
409 Conflict
304 Not Modified
400 Bad Request
401 Unauthorized
404 Not Found
413 Payload Too Large
415 Unsupported Media Type
422 Unprocessable Entity
500 Internal Server Error
501 Not Implemented
```

Internal storage-backend or application details are not returned directly to clients.

---

## Route Parameter Rules

The API validates all image identifiers before they reach storage.

### `namespace`

- Required.
- Maximum 32 characters.
- Lowercase ASCII letters, digits, `-`, and `_` only.

### `resource_id`

- Required.
- Maximum 100 characters.
- ASCII letters, digits, `-`, and `_` only.

### `slot`

Slots are one-based values from `1` to `255`. `IMAGE_MAX_SLOTS` defaults to `5` and controls the highest slot number that can be created with POST or with PUT when no image exists there. Lowering it does not affect GET, HEAD, info, presigned reads, listings, DELETE, or PUT replacement of an existing higher slot.

```text
IMAGE_MAX_SLOTS=12  # newly creatable slots are 1..=12
```

See [Configuration](configuration.md) for the complete validation and processing policy.

---

## Client Examples

See [Client Examples](clients.md) for cURL, JavaScript, Java, Python, and Rust examples.
