[← Back to README](../README.md)

# HTTP API

This document describes the public HTTP surface of Image Service.

## API Overview

The service exposes the following endpoints:

```text
GET  /health

GET  /api/images/{namespace}/{resource_id}/{slot}
HEAD /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}
DELETE /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}
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
POST /api/images/{namespace}/{resource_id}/{slot}
```

The request must use `multipart/form-data` and provide an image field named:

```text
image
```

If write authentication is enabled, include:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

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

Uploading another image to the same namespace/resource/slot replaces the existing image.

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
```

Reads are public by default for backwards compatibility. Set `GETMETHOD_AUTH=true` to require the configured `IMAGE_API_KEY` bearer token for GET, HEAD, and resource listing.

---

## List Images for a Resource

```text
GET /api/images/{namespace}/{resource_id}
```

Returns the existing WebP slots in ascending order. An empty resource returns `200 OK` with an empty `images` array.

```json
{
  "namespace": "products",
  "resource_id": "845",
  "images": [
    { "slot": 1, "url": "/api/images/products/845/1" },
    { "slot": 2, "url": "/api/images/products/845/2" }
  ]
}
```

## Check an Image

```text
HEAD /api/images/{namespace}/{resource_id}/{slot}
```

Returns `200 OK`, `Content-Type: image/webp`, and `Content-Length` when the image exists. The response has no body. Missing images return `404 Not Found`.

## Delete an Image

```text
DELETE /api/images/{namespace}/{resource_id}/{slot}
```

If write authentication is enabled, include `Authorization: Bearer <IMAGE_API_KEY>`. Deleting an existing or missing slot returns `204 No Content`, making the operation idempotent.

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
400 Bad Request
401 Unauthorized
404 Not Found
413 Payload Too Large
415 Unsupported Media Type
422 Unprocessable Entity
500 Internal Server Error
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

Current valid range:

```text
1..=5
```

See [Configuration](configuration.md) for the complete validation and processing policy.

---

## Client Examples

See [Client Examples](clients.md) for cURL, JavaScript, Java, Python, and Rust examples.
