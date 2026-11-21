[← Back to README](../README.md)

# HTTP API

This document describes the public HTTP surface of Image Service.

## API Overview

The service exposes the following endpoints:

```text
GET  /health
GET  /health/live
GET  /health/ready

GET  /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}/{slot}/info
HEAD /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}
PUT  /api/images/{namespace}/{resource_id}/{slot}
POST /api/images/{namespace}/{resource_id}/{slot}/presign
DELETE /api/images/{namespace}/{resource_id}/{slot}
GET  /api/images/{namespace}/{resource_id}
DELETE /api/images/{namespace}/{resource_id}

POST /api/image-jobs/{namespace}/{resource_id}/{slot}
PUT  /api/image-jobs/{namespace}/{resource_id}/{slot}
GET  /api/image-jobs/{job_id}

GET  /api/storage/usage
POST /api/storage/reconcile
POST /api/storage/audits
GET  /api/storage/audits/{audit_id}
GET  /api/storage/audits/{audit_id}/findings
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

## Storage Usage Reporting

Storage usage reporting and its background catalog pipeline are opt-in. Set
`IMAGE_STORAGE_USAGE_ENABLED=true` to enable the catalog, inventory,
verification, maintenance, and audit jobs together with the routes. The endpoints always require
`Authorization: Bearer <IMAGE_API_KEY>`, even when image reads are configured
as public. Enabling the feature without an API key prevents startup.

`GET /api/storage/usage` returns counts and byte totals from the current
catalog snapshot. Service writes are counted immediately, including before
the first full inventory finishes; `scan_status: "never"` and `stale: true`
make clear that direct storage changes have not yet been reconciled. Optional
query parameters narrow the result:

```text
GET /api/storage/usage?namespace=products
GET /api/storage/usage?namespace=products&resource_id=845
GET /api/storage/usage?namespace=products&resource_id=845&slot=1
```

The endpoint only reads the local SQLite catalog; it never scans LocalFS or
S3 synchronously. The response includes scan status, the last completed scan
time, snapshot age, and a `stale` flag. Counts cover objects with canonical
image-slot key paths following `{namespace}/{resource_id}/{slot}.webp`. POST,
PUT, and DELETE update catalog totals after their storage operation succeeds;
the mutation is journaled first so startup recovery can reconcile it if the
process exits between the storage write and catalog update. Direct external
storage edits become visible after the next successful full reconciliation,
so totals are prompt for service writes but are not a transactionally current
view of arbitrary backend changes.

The hourly inventory lists object metadata and schedules verification jobs.
Verification skips image decoding when object size, ETag, and effective
processing-policy fingerprint match the previous check. New or changed objects
are inspected, and repairable policy violations may be reprocessed in the
background during idle periods. Maintenance uses a conditional write so a
concurrent newer upload is not replaced. These per-object states and job
progress are internal; this API currently reports aggregate bytes and counts.

The response reports actual service storage usage; it does not contain a
customer subscription quota. The calling application can compare these totals
with its own contract or tenant records. This is a reporting endpoint, not an
atomic quota reservation: applications that must prevent concurrent uploads
from exceeding an entitlement need to reserve capacity in their own write
flow.

`POST /api/storage/reconcile` requests an immediate reconciliation and returns
`202 Accepted` (request queued for background processing) with a durable job ID. The job still obeys the configured idle
period and UTC window. Repeated active manual requests are deduplicated.

The recurring interval defaults to one hour. The inventory writes a staging
snapshot in batches and promotes it only after the full scan succeeds; failed
or interrupted jobs leave the previous complete snapshot queryable. Keep both
the job database and catalog database on persistent local storage. The default
catalog path is `storage-catalog.sqlite3` beside `IMAGE_JOB_DB_PATH`.

### Storage policy audits

The same opt-in and API-key requirements apply to policy audits. Start an
asynchronous read-only audit using the current completed inventory snapshot:

```http
POST /api/storage/audits
Authorization: Bearer <IMAGE_API_KEY>
Content-Type: application/json

{"namespace":"products","resource_id":"845"}
```

The scope object may be empty to audit all inventoried image slots. `resource_id`
requires `namespace`; `slot` requires both. A successful request returns
`202 Accepted`, an audit ID, durable job ID, inventory watermark, stale flag,
and a fingerprint for the effective policy snapshot. If no completed inventory
exists, the request returns `409 Conflict` and asks for reconciliation first.

```text
GET /api/storage/audits/{audit_id}
GET /api/storage/audits/{audit_id}/findings?limit=50
GET /api/storage/audits/{audit_id}/findings?limit=50&after={next_cursor}
```

The status resource reports `queued`, `running`, `complete`, `retry_wait`,
`failed`, or `cancelled` plus counters for inspected, compliant, violating,
changed, and inspection-error assets. Findings are ordered by object key and
code; `limit` defaults to 50 and is capped at 200. Pass the returned URL-safe
opaque `next_cursor` as `after` to continue. These endpoints return
`Cache-Control: no-store`.

Audits capture the effective global and namespace-profile settings when they
start. They inspect objects from that inventory snapshot in small batches,
checkpoint progress in SQLite, wait for foreground traffic to become idle,
and resume from the last recorded object after a restart or retry. They decode
stored images without re-encoding them. Findings include non-WebP or animated
images, dimensions above the captured output bounds, output bytes above the
captured limit, invalid image data, and objects that changed or disappeared
after inventory. Oversized objects that exceed the service's configured
in-memory request ceiling are reported as `too_large_to_inspect` rather than
read into unbounded memory. The audit never modifies or repairs an object.

The inventory snapshot and object itself can change between inventory and
inspection. ETags and size are compared when available; a missing validator
means the audit cannot prove the object version was unchanged. The response
exposes the inventory time and staleness so results are not mistaken for a
transactional view of storage. Run reconciliation first when the newest
available inventory is needed.

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

When write authentication is required (the default), include:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

The uploaded image payload is limited by `IMAGE_MAX_INPUT_BYTES` (15 MiB by default). The complete multipart request may include up to 1 MiB of framing overhead beyond that limit. Output is constrained by `IMAGE_MAX_OUTPUT_BYTES` (100 KiB by default) and targets `IMAGE_OUTPUT_WIDTH` × `IMAGE_OUTPUT_HEIGHT` (1024×1024 by default). The service fully decodes and validates every input. A WebP with no orientation transform that already has the exact target dimensions and fits the output byte limit is stored unchanged, without another lossy encode. Otherwise, the service center-crops to the target aspect ratio and only resizes when dimensions do not match; non-WebP inputs are encoded to WebP. Lossy WebP quality is searched from 100 down to 0. If quality 0 at the configured dimensions still exceeds the byte limit, the service reduces dimensions in further steps, preserving the target aspect ratio and always resizing from the same cropped source pixels.

The source byte limit is counted from received multipart chunks, including when the request has no `Content-Length`; the selected namespace/profile limit is enforced as the chunks arrive. `Content-Length` and the part's `Content-Type` are optional hints. The service detects the format from the incoming bytes as they arrive; if a MIME type is supplied, it must match the detected format. The full image is then decoded and validated before processing. Missing or incorrect hints do not bypass the actual byte limit or format checks. Synchronous `POST` and `PUT` uploads retain at most `IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES` of received source in their own buffer (8 MiB by default); after crossing it, bytes are written to private backend staging until the multipart body is complete. Staged sources are read back for processing only after reserving memory for their full observed size. The threshold does not change the absolute input limit or the synchronous response contract.

`POST` creates an image only when that slot is empty and returns `201 Created`. If the slot already contains an image, it returns `409 Conflict` and leaves the stored image unchanged. `PUT` creates a missing image with `201 Created` or replaces an existing image with `200 OK`; use it when replacing a slot. A successful upload stays within the configured output byte limit. The service can return `422 Unprocessable Entity` only if the configured byte limit is too small even for a 1×1 WebP image.

For a slot already known to exist, `POST` checks for the conflict before reading and processing the multipart image. A final conditional storage write still protects against two concurrent create requests. Operations on one resource are coordinated within this service process: successful uploads publish in storage order, and a later successful `PUT` replaces the prior value. A slot `DELETE` invalidates uploads that began for that slot before the delete; a resource `DELETE` invalidates uploads that began for any slot in that resource. Those superseded uploads return `409 Conflict` and cannot recreate the deleted image after the delete response. Requests that begin after the delete can create or replace images normally. Coordination is in-memory and per process; it is not a durable job log or cross-replica lock.

There is no `PATCH` upload method because uploads contain the complete image file; this API has no partial image representation to patch.

The service validates the route parameters, reads the multipart image, verifies the source format, processes the image, and stores the resulting WebP in the requested slot.
Upload admission reserves an upload slot and the configured receive-memory threshold before reading multipart data. A staged image extends that reservation to cover its complete source before it is loaded for decoding; if the additional budget is unavailable, the request returns `503 Service Unavailable` with `Retry-After: 1` and the temporary source is removed. When upload slots or the initial memory budget are unavailable, `POST` and `PUT` also return `503 Service Unavailable` with `Retry-After: 1`; the JSON error is `Upload processing capacity is temporarily busy; retry later.` This indicates temporary server capacity pressure, not that the image exceeded `IMAGE_MAX_INPUT_BYTES`; an image over that configured input limit returns `413 Payload Too Large`. A multipart read that exceeds `IMAGE_UPLOAD_READ_TIMEOUT_SECONDS` returns `408 Request Timeout`.
Processing uses the optional profile assigned to the namespace. Profiles can override the input byte limit, output byte limit, and target dimensions; fields they omit inherit the existing global environment settings. Unassigned namespaces keep the current global behavior.

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

### Asynchronous image processing

The synchronous endpoints above keep their existing behavior. To explicitly
queue processing instead, use the separate image-job routes:

```text
POST /api/image-jobs/{namespace}/{resource_id}/{slot}  # create-only
PUT  /api/image-jobs/{namespace}/{resource_id}/{slot}  # create or replace
GET  /api/image-jobs/{job_id}                          # get status/result
```

The request uses the same multipart `image` field, configured write-auth
policy (`IMAGE_WRITE_AUTH`, `required` by default), namespace
profile, input limits, slot rules, and detected-format checks as synchronous
uploads. The API streams the original into the configured storage backend's
service-reserved `__image_jobs/` staging prefix, then persists a processing job in the
SQLite/WAL scheduler. It returns `202 Accepted` only after both staging and job
creation succeed. The response contains `job_id`, `status_url`, and the
destination; `Location` points to the status URL and `Retry-After: 2` suggests
when to poll next. A full queue returns `503 Service Unavailable` with
`Retry-After: 1`.

`202 Accepted` means queued, not fully decoded and published. Path, slot,
capacity, byte-count, and any detectable unsupported-format or MIME mismatch
checks happen while receiving the body. Full decode and image-policy
processing happen in the worker, so malformed or undecodable content can
later produce a `failed` job. On success, the status
result includes the final image URL and output size. A browser can preview its
local selected file while the job is pending, then load the returned service
URL after status becomes `succeeded`.

`GET /api/image-jobs/{job_id}` follows `IMAGE_WRITE_AUTH` (`required` by
default) and returns only image-processing job details; it is not a general
scheduler inspection API.
Statuses are `queued`, `processing`, `retry_wait`, `succeeded`, `failed`,
`cancelled`, and `superseded`. POST conflicts if the object exists or any
active async job already owns the target slot. A new PUT supersedes an older active async job for
the same resource; target locking and cancellation checks prevent the older
job from publishing after the newer one. Synchronous POST/PUT/DELETE operations
also cancel queued work for their target. Resource DELETE cancels active image
jobs for that resource. During an async PUT, GET continues to serve the
previously published image until the replacement is ready.

The `__image_jobs/` prefix is reserved by the service but does not establish
separate filesystem or bucket ACLs. Restrict direct access to the configured
storage backend using its normal permission controls.

`Idempotency-Key` is optional and accepts 1 to 128 visible ASCII characters.
Repeating the same key with the same method, destination, and content returns
the existing job; reusing it for a different request returns `409 Conflict`.
Queue capacity is bounded by `IMAGE_ASYNC_MAX_JOBS` and
`IMAGE_ASYNC_MAX_STAGED_BYTES`. Staged objects and SQLite job records must live
on persistent storage if accepted work must survive container restarts.

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

Reads require the configured `IMAGE_API_KEY` bearer token by default. Set `IMAGE_READ_AUTH=none` to make GET, HEAD, resource listing, image information, and presigned URL creation public. `IMAGE_WRITE_AUTH` independently controls POST, PUT, and DELETE. The service fails to start if a required policy has no API key.

GET and HEAD return `Cache-Control` with a 60-second default lifetime. Responses are `public` when `IMAGE_READ_AUTH=none` and `private` when reads require authentication. `IMAGE_CACHE_TTL_SECONDS=0` sets `no-store`; `IMAGE_CACHE_CONTROL` can override the complete policy. GET supports `If-None-Match` and `If-Modified-Since`, returning `304 Not Modified` when the stored representation has not changed. ETag falls back to a SHA-256 content validator when the storage backend does not provide one. A fresh cached response can remain in use for up to its TTL after a PUT; ETag does not force an earlier check. `immutable` is unsuitable while uploads can replace content at the same URL.

When a conditional GET matches storage metadata, the service confirms that version before returning `304 Not Modified` (the cached representation is still valid). S3-compatible storage with ETags uses a conditional one-byte range read; LocalFS checks again while holding the resource operation lock. This avoids reading the full image while preventing stale metadata from producing a false 304. If storage does not provide an ETag, the SHA-256 fallback can only be compared after reading the body. For a `200 OK` response, the service reads metadata and bytes as one snapshot: its per-resource lock coordinates local writes/deletes, and storage backends that support conditional reads use `If-Match` to reject bytes from a different ETag. Repeated changes during a read return `503 Service Unavailable` (retry the request) rather than pairing one version's body with another version's validators.

## Image Information

```text
GET /api/images/{namespace}/{resource_id}/{slot}/info
```

Returns storage size and available validators plus the output format, dimensions, and active namespace processing profile. Unassigned namespaces report `default`.

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

The `profile` field reports the profile currently assigned to the namespace; it is not persisted per image. Changing a profile affects future uploads and does not reprocess images already stored.

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

When `IMAGE_WRITE_AUTH=required` (the default), include `Authorization: Bearer <IMAGE_API_KEY>`. Deleting an existing or missing slot returns `204 No Content`, making the operation idempotent.

## Delete a Resource

```text
DELETE /api/images/{namespace}/{resource_id}
```

Follows `IMAGE_WRITE_AUTH` (required by default). It recursively removes every object under the exact resource prefix and returns `204 No Content`, including when the resource is empty or already deleted. The prefix boundary prevents deleting similarly named resources. OpenDAL handles filesystem directory and object-storage prefix semantics behind the storage layer.

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
GET /health/live
GET /health/ready
```

`/health` remains a backwards-compatible alias for `/health/live`. Both return `204 No Content` (the process is running) without checking storage, so an orchestrator can restart the process only when it is unresponsive.

`/health/ready` returns `204 No Content` (the instance is ready) only while the server is accepting requests and a bounded OpenDAL storage check succeeds. It returns `503 Service Unavailable` (the instance is not ready) while shutting down, when storage is unavailable, or when the storage check exceeds its two-second timeout. The check lists at most one entry at the storage root; S3 credentials must allow the corresponding bucket-list operation. Health routes bypass application rate limits and authentication.

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
