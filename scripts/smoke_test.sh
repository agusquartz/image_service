#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
image_name="${1:-image-service:phase1}"
image_file="${2:-$repo_dir/TEST_IMAGE_ORIGINAL.png}"

if ! command -v docker >/dev/null 2>&1; then
    echo "docker is required" >&2
    exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
    echo "curl is required" >&2
    exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required" >&2
    exit 1
fi

if [[ ! -f "$image_file" ]]; then
    echo "Test image not found: $image_file" >&2
    echo "Pass the path to a local PNG/JPEG with: $0 [image-name] /path/to/image" >&2
    exit 1
fi

tmp_dir="$(mktemp -d)"
container_id=""
api_key="smoke-test-api-key"

cleanup() {
    if [[ -n "$container_id" ]]; then
        docker rm --force "$container_id" >/dev/null 2>&1 || true
    fi
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    if [[ -n "$container_id" ]]; then
        docker logs "$container_id" >&2 || true
    fi
    exit 1
}

request_status() {
    local output_file="$1"
    shift
    curl --silent --show-error --output "$output_file" --write-out '%{http_code}' "$@"
}

expect_status() {
    local expected="$1"
    local output_file="$2"
    shift 2
    local actual
    actual="$(request_status "$output_file" "$@")" || fail "request failed: $*"
    if [[ "$actual" != "$expected" ]]; then
        fail "expected HTTP $expected, got HTTP $actual for: $* (body: $(cat "$output_file" 2>/dev/null || true))"
    fi
}

start_service() {
    local auth_mode="$1"
    local -a args=(
        docker run --detach --rm --read-only
        --tmpfs /tmp/image-storage:rw,nosuid,size=64m,uid=65532,gid=65532,mode=0755
        --publish 127.0.0.1::3000
        --env IMAGE_BIND=0.0.0.0:3000
        --env IMAGE_STORAGE_SCHEME=fs
        --env IMAGE_STORAGE_ROOT=/tmp/image-storage
        --env IMAGE_STORAGE_ATOMIC_WRITE_DIR=/tmp/image-storage/.tmp
        --env IMAGE_API_KEY="$api_key"
    )

    case "$auth_mode" in
        default) ;;
        false) args+=(--env GETMETHOD_AUTH=false) ;;
        true) args+=(--env GETMETHOD_AUTH=true) ;;
        *) fail "unknown auth mode: $auth_mode" ;;
    esac

    container_id="$("${args[@]}" "$image_name")" || fail "could not start container in $auth_mode mode"
    local host_port
    host_port="$(docker port "$container_id" 3000/tcp | awk -F: 'NR == 1 {print $NF}')"
    [[ -n "$host_port" ]] || fail "could not determine the published port"
    base_url="http://127.0.0.1:$host_port"

    for _ in $(seq 1 50); do
        if curl --silent --output /dev/null "$base_url/health"; then
            return
        fi
        sleep 0.2
    done
    fail "service did not become healthy in $auth_mode mode"
}

stop_service() {
    docker rm --force "$container_id" >/dev/null
    container_id=""
}

test_mode() {
    local auth_mode="$1"
    local headers=()
    if [[ "$auth_mode" == "true" ]]; then
        headers=(-H "Authorization: Bearer $api_key")
    fi

    start_service "$auth_mode"
    local resource="smoke-${auth_mode}"
    local route="$base_url/api/images/products/$resource"
    local image_route="$route/1"

    expect_status 204 "$tmp_dir/health" "$base_url/health"
    expect_status 401 "$tmp_dir/unauthorized-upload" \
        --request POST --form "image=@$image_file" "$image_route"
    expect_status 200 "$tmp_dir/upload" \
        --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$image_route"

    if [[ "$auth_mode" == "true" ]]; then
        expect_status 401 "$tmp_dir/unauthorized-get" "$image_route"
        expect_status 401 "$tmp_dir/unauthorized-head" --head "$image_route"
        expect_status 401 "$tmp_dir/unauthorized-list" "$route"
    fi

    expect_status 200 "$tmp_dir/get" "${headers[@]}" "$image_route"
    [[ -s "$tmp_dir/get" ]] || fail "GET returned an empty image body in $auth_mode mode"
    expect_status 200 "$tmp_dir/head" --head "${headers[@]}" "$image_route"
    grep -qi '^Content-Type: image/webp' "$tmp_dir/head" || fail "HEAD did not return image/webp"
    grep -qi '^Content-Length: [1-9][0-9]*' "$tmp_dir/head" || fail "HEAD did not return a positive Content-Length"
    expect_status 200 "$tmp_dir/list" "${headers[@]}" "$route"
    python3 - "$tmp_dir/list" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["namespace"] == "products", payload
assert payload["images"][0]["slot"] == 1, payload
PY

    expect_status 401 "$tmp_dir/unauthorized-delete" --request DELETE "$image_route"
    expect_status 204 "$tmp_dir/delete" --request DELETE -H "Authorization: Bearer $api_key" "$image_route"
    expect_status 204 "$tmp_dir/delete-again" --request DELETE -H "Authorization: Bearer $api_key" "$image_route"
    expect_status 404 "$tmp_dir/get-after-delete" "${headers[@]}" "$image_route"
    expect_status 200 "$tmp_dir/empty-list" "${headers[@]}" "$route"
    python3 - "$tmp_dir/empty-list" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["images"] == [], payload
PY

    stop_service
    echo "PASS: endpoint and auth smoke tests ($auth_mode GETMETHOD_AUTH)"
}

if docker run --rm --read-only --env GETMETHOD_AUTH=true "$image_name" >"$tmp_dir/invalid-config.log" 2>&1; then
    fail "GETMETHOD_AUTH=true without IMAGE_API_KEY unexpectedly started"
fi
grep -q 'GETMETHOD_AUTH=true requires IMAGE_API_KEY' "$tmp_dir/invalid-config.log" \
    || fail "invalid auth configuration did not report the expected startup error"
echo "PASS: invalid auth configuration is rejected"

test_mode default
test_mode false
test_mode true

echo "All Image Service smoke tests passed. Containers used volatile tmpfs storage only."
