[← Back to README](../README.md)

# Security

Image Service processes untrusted binary uploads, so input validation and resource limits are treated as core parts of the service rather than optional application concerns.

## Current Safeguards

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

## Authentication Boundary

Write authentication is currently optional.

If `IMAGE_API_KEY` is configured, mutation requests must provide:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

If no API key is configured, write operations are currently allowed without authentication. Production deployments should make this choice deliberately and should not expose unauthenticated upload endpoints unintentionally.

GET image requests are currently public.

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

## Path Safety

`namespace`, `resource_id`, and `slot` are validated before building filesystem paths.

Path separators and arbitrary filesystem characters are rejected. This prevents user-controlled route values from being used directly as unrestricted filesystem paths.

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
