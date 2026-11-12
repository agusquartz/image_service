FROM rust:1.88-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked

# Export a platform-specific Linux executable for GitHub release assets.
FROM scratch AS binary

COPY --from=builder /app/target/release/image_service /image_service

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

ARG BUILD_VERSION=dev

LABEL org.opencontainers.image.source="https://github.com/agusquartz/image_service" \
      org.opencontainers.image.title="Image Service" \
      org.opencontainers.image.description="Self-hosted single-tenant image processing and storage service" \
      org.opencontainers.image.version="${BUILD_VERSION}"

COPY --from=builder /app/target/release/image_service /image_service

EXPOSE 3000

ENTRYPOINT ["/image_service"]
