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
- Maximum decode dimensions.
- Decoder memory-allocation limits.
- Restricted namespace characters.
- Restricted resource-ID characters.
- Bounded image-processing concurrency.
- `X-Content-Type-Options: nosniff` on image responses.
- Required bearer-token protection for reads and writes by default; each policy can be explicitly set to `none`.
- Per-client read/write rate limits with a `429` response and `Retry-After`.
- Server-generated request IDs and structured JSON request logs.
- An optional Prometheus metrics endpoint protected by the API key by default.

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

`/health` remains public for container and orchestrator health checks. `/metrics` is disabled by default. When enabled, it requires the API key by default through `IMAGE_METRICS_AUTH=true`, independently of read/write policies. Setting `IMAGE_METRICS_AUTH=false` makes metrics public and should only be used when network or proxy controls restrict access.

## Customer Isolation

The intended deployment is one service instance per customer. Authentication keys, authorization policies, and storage settings apply to an entire instance; they are not scoped to individual namespaces or resources. Separate instances and storage credentials make customer data boundaries explicit and limit the effect of an application-level failure to that deployment. Treat `namespace` only as an organization mechanism inside one customer's instance. Isolation at the service level does not remove risks from shared hosts, networks, clusters, or storage infrastructure.

The presign endpoint also uses the read-auth policy. A successful response contains a temporary bearer capability: anyone holding the URL can read the image until it expires. Treat presigned URLs as secrets and keep `IMAGE_PRESIGN_TTL_SECONDS` as short as the application allows.

## Rate Limiting and Observability

Read and write requests have separate fixed-window budgets per client source IP. Defaults are 600 reads and 60 writes per 60 seconds. Set the corresponding request count to `0` to disable that budget. The limiter uses the socket peer address and does not trust caller-supplied `X-Forwarded-For`; behind a proxy, configure network-level limits or a trusted-proxy design before relying on client IP fairness. The limiter is in-memory and applies per process, so replicas do not share counters.

Every response includes a generated `X-Request-ID`. Structured JSON logs use Rust's `tracing` ecosystem and can be filtered with `RUST_LOG`; levels are `ERROR`, `WARN`, `INFO`, `DEBUG`, and `TRACE`. Logs and metric labels do not include API keys or image contents. The service writes logs to stdout and does not rotate files; configure retention in Docker, systemd/journald, or the hosting platform. The optional Prometheus endpoint is enabled with `IMAGE_METRICS_ENABLED=true` and exposes bounded method/status counters, request duration, and rate-limit counters. It requires bearer authentication by default; `IMAGE_METRICS_AUTH=false` is an explicit opt-out for deployments protected at the network or proxy layer.

## Upload Validation Boundary

Uploaded files are constrained by several independent checks:

- Axum request-body limit.
- Maximum source-image byte size.
- Detected image format.
- Declared MIME validation.
- Decoder width and height limits.
- Decoder allocation limit.
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
