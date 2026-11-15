#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
service_image="${PLAYGROUND_IMAGE:-image-service:minio-playground}"
minio_image="${MINIO_IMAGE:-pgsty/minio:latest}"
playground_port="${PLAYGROUND_PORT:-4173}"
api_key="${PLAYGROUND_API_KEY:-minio-playground-local-key}"
read_auth="${IMAGE_READ_AUTH:-required}"
write_auth="${IMAGE_WRITE_AUTH:-required}"
metrics_auth="${IMAGE_METRICS_AUTH:-true}"
metrics_enabled="${IMAGE_METRICS_ENABLED:-true}"
max_input="${IMAGE_MAX_INPUT_BYTES:-15728640}"
max_output="${IMAGE_MAX_OUTPUT_BYTES:-102400}"
output_width="${IMAGE_OUTPUT_WIDTH:-1024}"
output_height="${IMAGE_OUTPUT_HEIGHT:-1024}"
max_slots="${IMAGE_MAX_SLOTS:-5}"
rate_read="${IMAGE_RATE_LIMIT_READ_REQUESTS:-600}"
rate_write="${IMAGE_RATE_LIMIT_WRITE_REQUESTS:-60}"
rate_window="${IMAGE_RATE_LIMIT_WINDOW_SECONDS:-60}"
profiles="${IMAGE_PROCESSING_PROFILES:-{\"playground-avatar\":{\"max_output_bytes\":25600,\"output_width\":128,\"output_height\":128}}}"
namespace_profiles="${IMAGE_NAMESPACE_PROFILES:-{\"avatars\":\"playground-avatar\"}}"
network="image-service-playground-net-$$"
minio_name="image-service-playground-minio-$$"
service_name="image-service-playground-api-$$"
server_pid=""
tmp_dir="$(mktemp -d)"

cleanup() {
    if [[ -n "$server_pid" ]]; then kill "$server_pid" >/dev/null 2>&1 || true; fi
    docker rm --force "$service_name" "$minio_name" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
    rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

fail() { echo "ERROR: $*" >&2; exit 1; }
for cmd in docker curl python3; do command -v "$cmd" >/dev/null 2>&1 || fail "$cmd es requerido"; done
docker info >/dev/null 2>&1 || fail "Docker no está ejecutándose o no es accesible"

case "$(uname -m)" in
    arm64|aarch64) arch="arm64" ;;
    x86_64|amd64) arch="amd64" ;;
    *) fail "Arquitectura no soportada: $(uname -m). Se admite arm64 o amd64." ;;
esac
platform="linux/$arch"

echo "Construyendo Image Service para $platform..."
docker buildx build --platform "$platform" --target runtime --tag "$service_image" --load "$repo_dir"
if ! docker image inspect "$minio_image" >/dev/null 2>&1; then
    echo "Descargando MinIO compatible ($minio_image)..."
    docker pull "$minio_image"
fi

docker network create "$network" >/dev/null
minio_access_key="minio-playground-user"
minio_secret_key="minio-playground-secret-123"
bucket="image-service-playground"
docker run --detach --rm \
    --name "$minio_name" --network "$network" --network-alias minio \
    --tmpfs /data:rw,nosuid,size=512m,mode=1777 \
    --publish 127.0.0.1::9000 \
    --env "MINIO_ROOT_USER=$minio_access_key" \
    --env "MINIO_ROOT_PASSWORD=$minio_secret_key" \
    "$minio_image" server /data --console-address :9001 >/dev/null

minio_port="$(docker port "$minio_name" 9000/tcp | awk -F: 'NR == 1 {print $NF}')"
minio_url="http://127.0.0.1:$minio_port"
for _ in $(seq 1 100); do
    curl --fail --silent "$minio_url/minio/health/live" >/dev/null && break
    sleep 0.2
done
curl --fail --silent "$minio_url/minio/health/live" >/dev/null || fail "MinIO no inició"

curl --fail --silent --show-error \
    --aws-sigv4 "aws:amz:us-east-1:s3" \
    --user "$minio_access_key:$minio_secret_key" \
    --request PUT "$minio_url/$bucket" >/dev/null

docker run --detach --rm --read-only \
    --name "$service_name" --network "$network" \
    --publish 127.0.0.1::3000 \
    --env IMAGE_BIND=0.0.0.0:3000 \
    --env IMAGE_API_KEY="$api_key" \
    --env "IMAGE_READ_AUTH=$read_auth" \
    --env "IMAGE_WRITE_AUTH=$write_auth" \
    --env "IMAGE_METRICS_AUTH=$metrics_auth" \
    --env "IMAGE_METRICS_ENABLED=$metrics_enabled" \
    --env "IMAGE_RATE_LIMIT_READ_REQUESTS=$rate_read" \
    --env "IMAGE_RATE_LIMIT_WRITE_REQUESTS=$rate_write" \
    --env "IMAGE_RATE_LIMIT_WINDOW_SECONDS=$rate_window" \
    --env IMAGE_STORAGE_SCHEME=s3 \
    --env "IMAGE_STORAGE_BUCKET=$bucket" \
    --env IMAGE_STORAGE_REGION=us-east-1 \
    --env IMAGE_STORAGE_ENDPOINT=http://minio:9000 \
    --env IMAGE_STORAGE_ENABLE_VIRTUAL_HOST_STYLE=false \
    --env "IMAGE_STORAGE_ACCESS_KEY_ID=$minio_access_key" \
    --env "IMAGE_STORAGE_SECRET_ACCESS_KEY=$minio_secret_key" \
    --env "IMAGE_MAX_INPUT_BYTES=$max_input" \
    --env "IMAGE_MAX_OUTPUT_BYTES=$max_output" \
    --env "IMAGE_OUTPUT_WIDTH=$output_width" \
    --env "IMAGE_OUTPUT_HEIGHT=$output_height" \
    --env "IMAGE_MAX_SLOTS=$max_slots" \
    --env "IMAGE_PROCESSING_PROFILES=$profiles" \
    --env "IMAGE_NAMESPACE_PROFILES=$namespace_profiles" \
    "$service_image" >/dev/null

service_port="$(docker port "$service_name" 3000/tcp | awk -F: 'NR == 1 {print $NF}')"
service_url="http://127.0.0.1:$service_port"
healthy=false
for _ in $(seq 1 100); do
    if curl --fail --silent "$service_url/health" >/dev/null; then healthy=true; break; fi
    sleep 0.2
done
[[ "$healthy" == true ]] || { docker logs "$service_name" >&2 || true; fail "Image Service no inició"; }

export PLAYGROUND_PORT="$playground_port"
export PLAYGROUND_SERVICE_URL="$service_url"
export PLAYGROUND_MINIO_PORT="$minio_port"
export PLAYGROUND_SERVICE_CONTAINER="$service_name"
export PLAYGROUND_MINIO_ACCESS_KEY="$minio_access_key"
export PLAYGROUND_MINIO_SECRET_KEY="$minio_secret_key"
export PLAYGROUND_MINIO_BUCKET="$bucket"
export PLAYGROUND_API_KEY="$api_key"
export PLAYGROUND_PLATFORM="$platform"
export PLAYGROUND_IMAGE="$service_image"
export PLAYGROUND_MINIO_IMAGE="$minio_image"
export IMAGE_API_KEY="$api_key"
export IMAGE_READ_AUTH="$read_auth"
export IMAGE_WRITE_AUTH="$write_auth"
export IMAGE_METRICS_ENABLED="$metrics_enabled"
export IMAGE_METRICS_AUTH="$metrics_auth"
export IMAGE_MAX_INPUT_BYTES="$max_input"
export IMAGE_MAX_OUTPUT_BYTES="$max_output"
export IMAGE_OUTPUT_WIDTH="$output_width"
export IMAGE_OUTPUT_HEIGHT="$output_height"
export IMAGE_MAX_SLOTS="$max_slots"
export IMAGE_PROCESSING_PROFILES="$profiles"
export IMAGE_NAMESPACE_PROFILES="$namespace_profiles"
export IMAGE_RATE_LIMIT_READ_REQUESTS="$rate_read"
export IMAGE_RATE_LIMIT_WRITE_REQUESTS="$rate_write"
export IMAGE_RATE_LIMIT_WINDOW_SECONDS="$rate_window"

python3 "$script_dir/minio_playground_server.py" >"$tmp_dir/playground.log" 2>&1 &
server_pid="$!"
for _ in $(seq 1 50); do
    if curl --silent "http://127.0.0.1:$playground_port/__config" >/dev/null; then break; fi
    sleep 0.1
done
curl --fail --silent "http://127.0.0.1:$playground_port/__config" >/dev/null || { cat "$tmp_dir/playground.log" >&2; fail "No se pudo iniciar el servidor del playground"; }

cat <<EOF
MinIO Playground listo:
  URL:       http://127.0.0.1:$playground_port
  API:       $service_url
  MinIO:     $minio_url
  Plataforma: $platform
  API key:   $api_key
  Profile:   namespace 'avatars' usa 128x128 y máximo 25 KiB

El servicio y MinIO usan almacenamiento efímero. Presiona Ctrl+C para limpiar.
EOF
wait "$server_pid"
