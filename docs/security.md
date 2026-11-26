[← Back to README](../README.md)

# Security

Image Service processes untrusted binary uploads, so input validation and resource limits are treated as core parts of the service rather than optional application concerns.

## Current Safeguards

The service includes several defensive measures:

- Configurable request-body limits (`IMAGE_MAX_INPUT_BYTES` plus 1 MiB multipart framing).
- Configurable source-file size limits (`IMAGE_MAX_INPUT_BYTES`).
- Configurable WebP byte and target-dimension limits (`IMAGE_MAX_OUTPUT_BYTES`, `IMAGE_OUTPUT_WIDTH`, and `IMAGE_OUTPUT_HEIGHT`).
- Optional namespace-specific input/output byte and output-dimension limits through validated processing profiles; unspecified profile fields inherit global limits.
- MIME-to-format verification.
- Configurable maximum decode dimension and per-image decoder allocation limits, with defaults of 12,000 pixels and 256 MiB.
- A configurable weighted processing-memory admission budget; operators must tune it with decode/output dimensions and concurrency for the container's memory limit.
- Restricted namespace characters.
- Restricted resource-ID characters.
- Bounded image-processing concurrency.
- Separate upload admission and compressed-body budget before multipart reads; configurable whole-body read timeout.
- Synchronous multipart uploads spill from a bounded receive buffer to private backend staging after `IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES`; actual bytes are counted and the absolute profile/global input limit still applies. Temporary staging is removed on request failure or cancellation.
- Optional async image jobs stream uploaded source data into a service-reserved backend staging prefix, enforce the configured source-size limit, and reject detected unsupported or MIME-mismatched formats during receipt.
- Tus upload sessions are disabled by default. When enabled, read/write authentication applies to session creation, chunk writes, status, and cancellation. Session IDs are random UUIDs and sessions expire; SQLite stores session metadata, offsets, status, and chunk checksums, while confirmed bytes are held in the private `__image_transfer/resumable/` staging prefix in the configured storage backend. Protect and persist both locations. The service cleans expired session data and startup orphan objects, but storage access should still be restricted to the service.
- `X-Content-Type-Options: nosniff` on image responses.
- Required bearer-token protection for reads and writes by default; each policy can be explicitly set to `none`.
- Per-source-IP read/write rate limits with a `429` response and `Retry-After`.
- Server-generated request IDs and structured JSON request logs.
- An optional Prometheus metrics endpoint protected by the API key by default.

## Deployment Isolation

The intended deployment model is one service instance per customer. API keys, storage credentials, and service settings apply to the full instance; namespaces only organize that customer's resources and do not provide authorization boundaries. Separate instances and storage credentials keep customer data boundaries explicit and help limit application-level incidents to one deployment. Shared hosts, networks, clusters, or storage remain shared risks.

A production deployment should additionally consider:

- TLS termination.
- Reverse-proxy request limits.
- Audit logging.
- Distributed rate limiting when running multiple replicas.
- Reverse-proxy-aware client address handling.
- Storage quotas.
- Backup policy.
- Network restrictions.
- Secret management rather than long-lived keys in plain environment files.

---

## Authentication Boundary

Reads and writes require bearer authentication by default. The service refuses to start unless `IMAGE_API_KEY` is configured while either policy is `required`.

Configure read and write authentication independently:

```dotenv
IMAGE_READ_AUTH=required
IMAGE_WRITE_AUTH=required
```

The secure defaults are `required` for both. Set `IMAGE_READ_AUTH=none` for public GET/HEAD/list/info/presign access while keeping writes protected, or set `IMAGE_WRITE_AUTH=none` to leave mutations public while reads remain protected. Set both to `none` only for controlled environments. If either policy remains `required`, startup fails without `IMAGE_API_KEY`.

`GETMETHOD_AUTH` has been removed. If it remains in the environment, startup fails with guidance to use `IMAGE_READ_AUTH` and `IMAGE_WRITE_AUTH`. This is a deliberate configuration migration: deployments that previously relied on public reads or unprotected writes must choose those policies explicitly.

`/health`, `/health/live`, and `/health/ready` remain public so container and orchestrator probes can use them. `/health/ready` discloses only whether the configured storage check succeeds. `/metrics` is disabled by default. When enabled, it requires the API key by default through `IMAGE_METRICS_AUTH=true`, independently of read/write policies. Setting `IMAGE_METRICS_AUTH=false` makes metrics public and should only be used when network or proxy controls restrict access.

## Customer Isolation

The intended deployment is one service instance per customer. Authentication keys, authorization policies, and storage settings apply to an entire instance; they are not scoped to individual namespaces or resources. Separate instances and storage credentials make customer data boundaries explicit and limit the effect of an application-level failure to that deployment. Treat `namespace` only as an organization mechanism inside one customer's instance. Isolation at the service level does not remove risks from shared hosts, networks, clusters, or storage infrastructure.

The presign endpoint also uses the read-auth policy. A successful response contains a temporary bearer capability: anyone holding the URL can read the image until it expires. Treat presigned URLs as secrets and keep `IMAGE_PRESIGN_TTL_SECONDS` as short as the application allows.

The explicit `POST` and `PUT /api/image-jobs/...` endpoints follow
`IMAGE_WRITE_AUTH`, which is `required` by default, and stage unprocessed
uploads under the service-reserved `__image_jobs/` prefix. The job status route
follows the same policy because it exposes the destination and processing
result. The prefix does not create separate filesystem or bucket ACLs: restrict
direct access to the configured backend with its normal permission controls.
Keep the staging backend and SQLite job database on persistent,
access-controlled storage. Accepted jobs can survive process restarts only
when both are persistent; staging objects are removed after terminal
completion or during orphan cleanup at startup. The API does not expose a
general job listing or cancellation endpoint.

Storage usage reporting is opt-in through `IMAGE_STORAGE_USAGE_ENABLED`. When
enabled, all `/api/storage/*` usage, reconciliation, and audit endpoints always
require `IMAGE_API_KEY`, even if image reads are public. The endpoints return
`Cache-Control: no-store`; the aggregate storage totals are operational data,
not public image metadata. Enabling reporting without a key prevents startup.
The SQLite storage catalog contains object keys, namespace/resource/slot
identifiers, sizes, validators, mutation journal entries, policy verification
results, and audit findings. Protect and back up its volume with the same care
as the durable job database.

Explicit policy audits read and decode stored image contents in bounded
batches but do not modify them. Separately, automatic verification can enqueue
maintenance for images that violate configured processing rules. Maintenance
runs only through the idle/window-gated internal scheduler, uses the normal
image processor and memory/CPU limits, and conditionally replaces only the
object version it inspected. Invalid or uninspectable objects are not
automatically rewritten. A successful maintenance write is journaled and
verified again. Keep the catalog on persistent storage and restrict access to
it as operational data. Audit results use a captured inventory watermark;
storage can change between inventory and inspection, and objects without
validators cannot be proven unchanged.

## Rate Limiting and Observability

Read and write requests have separate fixed-window budgets per socket peer IP. Defaults are 600 reads and 60 writes per 60 seconds. Set the corresponding request count to `0` to disable that budget. The bearer key is not the limiter identity; requests with or without a key from the same source IP share the budget. Health routes bypass the limiter. This is not a per-person quota: CGNAT, corporate NAT, reverse proxies, and CDNs may make many legitimate users share one IP and can cause collective `429` responses. The service does not trust caller-supplied `X-Forwarded-For` or `Forwarded`; behind an intermediary, it sees the intermediary's address. Avoid low limits for shared public traffic, size them for the aggregate traffic, or enforce fairness at a trusted edge. Disabling the application limit with `0` removes that layer of protection. Counters are in-memory and per process, so replicas do not share them.

Every response includes a generated `X-Request-ID`. Structured JSON logs use Rust's `tracing` ecosystem and can be filtered with `RUST_LOG`; levels are `ERROR`, `WARN`, `INFO`, `DEBUG`, and `TRACE`. Request completion logs include the request ID, method, normalized route label, status, and duration. Logs and metric labels do not include API keys or image contents. The service writes logs to stdout and does not rotate files; configure retention in Docker, systemd/journald, or the hosting platform. The optional Prometheus endpoint is enabled with `IMAGE_METRICS_ENABLED=true` and exposes three HTTP-level metric families: `image_service_http_requests_total` (method and status-class labels), `image_service_http_request_duration_seconds` (histogram, sum, and count), and `image_service_rate_limited_requests_total` (429 counter). The request counter has no route, namespace, or resource labels. The endpoint requires bearer authentication by default; `IMAGE_METRICS_AUTH=false` is an explicit opt-out for deployments protected at the network or proxy layer.

These metrics do not measure image-processing/compression or storage operations. The service also does not currently export OpenTelemetry metrics or traces. JSON `tracing` logs are structured application logs; using the `tracing` crate does not by itself mean an OpenTelemetry exporter is configured.

## Upload Validation Boundary

Uploaded files are constrained by several independent checks:

- Axum request-body limit.
- Maximum source-image byte size.
- Detected image format.
- Declared MIME validation.
- Decoder width and height limits.
- Configured decoder allocation and weighted processing-memory budget.
- Bounded concurrent image-processing jobs.

The real image format is detected from file bytes rather than trusting the filename alone.

## Storage Key Safety

`namespace`, `resource_id`, and `slot` are validated before building storage object keys.

Path separators and arbitrary storage-key characters are rejected.

This prevents user-controlled route values from changing the intended
storage hierarchy or escaping into unintended object-key namespaces.

The same validation applies regardless of whether the configured backend
is a local filesystem or S3-compatible object storage.

See [Configuration](configuration.md) for the exact validation rules.

## Response Hardening

Stored images are always returned as WebP and currently include:

```http
Content-Type: image/webp
X-Content-Type-Options: nosniff
```

## Production Recommendations

The internal scheduler persists job payloads, policy snapshots, progress, and
errors in SQLite. Keep its database directory on a persistent local volume
with restrictive ownership; include the database and its WAL/SHM sidecars in
the backup and restore plan. Do not place the WAL database on NFS/SMB or share
it between service replicas. Job payloads must contain references and
operational metadata only, never image bytes, API keys, or presigned URLs.

Before exposing the service publicly, consider:

- TLS termination.
- Reverse-proxy request limits.
- Rate limiting.
- Explicit authentication/authorization policy.
- Audit and structured application logging.
- Metrics and alerting.
- Storage quotas.
- Backups.
- Network restrictions.
- Secret management rather than committing credentials to `.env`.
- A deliberate CORS policy if browser applications access a different origin.
- Cache policy appropriate for mutable image URLs.

## Vulnerability Reporting

Do not report security vulnerabilities through public GitHub issues.

Follow the repository-level instructions in [`SECURITY.md`](../SECURITY.md).
