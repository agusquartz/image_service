[← Back to README](../README.md)

# Deployment

This document covers the current source-based deployment model and the runtime environment expected by Image Service.

## Requirements

- A Rust toolchain compatible with the project `rust-version`.
- Filesystem write access to the configured image directory.
- Network access to the configured bind address.

The service is a self-hosted server application and is not intended to be published as a reusable crates.io library.

## Environment File

For local or simple deployments:

```bash
cp .env.example .env
```

The application loads `.env` through `dotenvy` when present.

Do not commit the real `.env` file.

## Running from Source

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

## Release Build

For a production-oriented local binary:

```bash
cargo build --release --locked
```

The resulting executable is created under:

```text
target/release/
```

The repository should commit `Cargo.lock`, and automated builds should use `--locked` so dependency resolution cannot silently change during CI or deployment.

## Storage Directory

By default, Image Service uses the operating system's local application-data directory.

Typical locations are:

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

Override the storage directory with:

```dotenv
IMAGE_SERVICE_DIR=/var/lib/image-service/images
```

The service process must be able to create directories and write files below that path.

## Bind Address

Default:

```dotenv
IMAGE_BIND=127.0.0.1:3000
```

Binding to loopback is appropriate when a reverse proxy runs on the same machine.

If the service must listen on all interfaces:

```dotenv
IMAGE_BIND=0.0.0.0:3000
```

Do this only with an appropriate firewall, reverse proxy, authentication policy, and TLS strategy.

## Reverse Proxy

A reverse proxy can provide:

- TLS termination.
- Request-size enforcement.
- Access logging.
- Rate limiting.
- Hostname routing.
- Additional security headers.
- Optional caching/CDN integration.

The application-level body and decoder limits should remain enabled even when equivalent proxy limits exist.

## Health Check

Use:

```text
GET /health
```

A healthy HTTP process returns:

```text
204 No Content
```

## Production Checklist

Before public deployment:

- Use a release build.
- Keep `Cargo.lock` committed.
- Run CI successfully.
- Set an explicit write-authentication policy.
- Set a deliberate bind address.
- Verify the storage path and permissions.
- Put TLS in front of the service.
- Apply proxy/request limits.
- Configure logs and monitoring.
- Protect secrets.
- Plan backups and storage quotas.
- Review [`security.md`](security.md).

## Future Deployment Targets

Planned deployment improvements include:

- Container images.
- Docker Compose examples.
- Systemd guidance.
- S3-compatible object storage.
- Configurable caching and CDN behavior.
- Structured logging and metrics.

See the [Roadmap](../ROADMAP.md).
