#!/usr/bin/env bash
set -Eeuo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
service_image="${IMAGE_SERVICE_IMAGE:-image-service:phase1-phase3}"
minio_image="${MINIO_IMAGE:-pgsty/minio:latest}"
image_file="${1:-$repo_dir/TEST_IMAGE_ORIGINAL.png}"

[[ -f "$image_file" ]] || {
    echo "Test image not found: $image_file" >&2
    exit 1
}

if ! docker image inspect "$minio_image" >/dev/null 2>&1; then
    docker pull "$minio_image"
fi

docker build --tag "$service_image" "$repo_dir"
bash "$repo_dir/scripts/smoke_test.sh" "$service_image" "$image_file"
IMAGE_SERVICE_IMAGE="$service_image" MINIO_IMAGE="$minio_image" \
    bash "$repo_dir/scripts/minio_smoke_test.sh" "$image_file"

echo "Phase 1, 2, and 3 verification completed successfully."
