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
- Optional bearer-token protection for writes.
- Optional bearer-token protection for image reads.

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

If `IMAGE_API_KEY` is configured, mutation requests (`POST`, `PUT`, and `DELETE`) must provide:

```http
Authorization: Bearer <IMAGE_API_KEY>
```

If no API key is configured, write operations are currently allowed without authentication. Production deployments should make this choice deliberately and should not expose unauthenticated upload endpoints unintentionally.

Image reads (`GET`, `HEAD`, and resource listing) are public by default to preserve existing integrations. Set `GETMETHOD_AUTH=true` to require the same bearer token for these endpoints. This setting requires `IMAGE_API_KEY`; the service rejects that configuration at startup if the key is missing. `/health` is not protected by this setting.

The presign endpoint also uses the read-auth policy. A successful response contains a temporary bearer capability: anyone holding the URL can read the image until it expires. Treat presigned URLs as secrets and keep `IMAGE_PRESIGN_TTL_SECONDS` as short as the application allows.

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
