[← Back to README](../README.md)

# HTTP API

This document describes the public HTTP surface of Image Service.

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

The current GET endpoint is public.

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

Internal filesystem or application details are not returned directly to clients.

---

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
