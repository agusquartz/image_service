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
storage_volume=""
api_key="smoke-test-api-key"

cleanup() {
    if [[ -n "$container_id" ]]; then
        docker rm --force "$container_id" >/dev/null 2>&1 || true
    fi
    if [[ -n "$storage_volume" ]]; then
        docker volume rm "$storage_volume" >/dev/null 2>&1 || true
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

request_status_with_headers() {
    local output_file="$1"
    local headers_file="$2"
    shift 2
    curl --silent --show-error --output "$output_file" --dump-header "$headers_file" --write-out '%{http_code}' "$@"
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

expect_read_status() {
    local expected="$1"
    local output_file="$2"
    shift 2
    if [[ "$auth_mode" == "true" ]]; then
        expect_status "$expected" "$output_file" "$@" -H "Authorization: Bearer $api_key"
    else
        expect_status "$expected" "$output_file" "$@"
    fi
}

start_service() {
    local auth_mode="$1"
    local -a args=(docker run --detach --rm --read-only)
    if [[ "$auth_mode" == "max-slots" ]]; then
        storage_volume="image-service-smoke-$$-$RANDOM"
        docker volume create --driver local \
            --opt type=tmpfs --opt device=tmpfs \
            --opt o=rw,nosuid,size=64m,uid=65532,gid=65532,mode=0755 \
            "$storage_volume" >/dev/null
        args+=(--mount "type=volume,source=$storage_volume,target=/tmp/image-storage")
    else
        args+=(--tmpfs /tmp/image-storage:rw,nosuid,size=64m,uid=65532,gid=65532,mode=0755)
    fi
    args+=(
        --publish 127.0.0.1::3000
        --env IMAGE_BIND=0.0.0.0:3000
        --env IMAGE_STORAGE_SCHEME=fs
        --env IMAGE_STORAGE_ROOT=/tmp/image-storage
        --env IMAGE_STORAGE_ATOMIC_WRITE_DIR=/tmp/image-storage/.tmp
        --env IMAGE_API_KEY="$api_key"
    )

    case "$auth_mode" in
        default) ;;
        false) args+=(--env GETMETHOD_AUTH=false --env IMAGE_PRESIGN_TTL_SECONDS=600 --env IMAGE_CACHE_TTL_SECONDS=60) ;;
        true) args+=(--env GETMETHOD_AUTH=true --env IMAGE_PRESIGN_TTL_SECONDS=600 --env IMAGE_CACHE_TTL_SECONDS=60) ;;
        dimensions) args+=(--env IMAGE_OUTPUT_WIDTH=64 --env IMAGE_OUTPUT_HEIGHT=64) ;;
        profiles)
            args+=(
                --env IMAGE_OUTPUT_WIDTH=64 --env IMAGE_OUTPUT_HEIGHT=64
                --env IMAGE_MAX_INPUT_BYTES=1048576
                --env 'IMAGE_PROCESSING_PROFILES={"compact":{"max_input_bytes":4194304,"max_output_bytes":51200},"avatar":{"max_output_bytes":25600,"output_width":32,"output_height":32}}'
                --env 'IMAGE_NAMESPACE_PROFILES={"products":"compact","avatars":"avatar"}'
            )
            ;;
        max-slots) args+=(--env IMAGE_MAX_SLOTS=2) ;;
        input-limit) args+=(--env IMAGE_MAX_INPUT_BYTES=1024) ;;
        output-limit) args+=(--env IMAGE_MAX_OUTPUT_BYTES=1) ;;
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
    if [[ -n "$storage_volume" ]]; then
        docker volume rm "$storage_volume" >/dev/null
        storage_volume=""
    fi
}

test_mode() {
    local auth_mode="$1"
    local max_creatable_slot=5
    [[ "$auth_mode" == "max-slots" ]] && max_creatable_slot=2
    local over_limit_slot=$((max_creatable_slot + 1))
    local upload_file="$image_file"
    if [[ "$auth_mode" == "profiles" ]]; then
        upload_file="$tmp_dir/profile-large.png"
        python3 - "$upload_file" <<'PY'
import random
import struct
import sys
import zlib

def chunk(kind, data):
    checksum = zlib.crc32(kind + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", checksum)

width = height = 1024
rng = random.Random(2026)
pixels = b"".join(bytes((0,)) + rng.randbytes(width * 3) for _ in range(height))
png = bytes((137, 80, 78, 71, 13, 10, 26, 10))
png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(pixels))
png += chunk(b"IEND", b"")
with open(sys.argv[1], "wb") as image_file:
    image_file.write(png)
PY
    fi
    start_service "$auth_mode"
    local resource="smoke-${auth_mode}"
    local route="$base_url/api/images/products/$resource"
    local image_route="$route/1"

    if [[ "$auth_mode" == "input-limit" ]]; then
        expect_status 413 "$tmp_dir/input-too-large" \
            --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$image_route"
        stop_service
        echo "PASS: IMAGE_MAX_INPUT_BYTES rejects oversized uploads"
        return
    fi
    if [[ "$auth_mode" == "output-limit" ]]; then
        expect_status 422 "$tmp_dir/output-too-large" \
            --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$image_route"
        stop_service
        echo "PASS: IMAGE_MAX_OUTPUT_BYTES rejects output that cannot fit"
        return
    fi

    expect_status 204 "$tmp_dir/health" "$base_url/health"
    expect_status 401 "$tmp_dir/unauthorized-upload" \
        --request POST --form "image=@$image_file" "$image_route"
    expect_status 201 "$tmp_dir/upload" \
        --request POST --form "image=@$upload_file" -H "Authorization: Bearer $api_key" "$image_route"
    expect_status 409 "$tmp_dir/conflicting-upload" \
        --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$image_route"
    expect_status 200 "$tmp_dir/put-upload" \
        --request PUT --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$image_route"
    expect_status 201 "$tmp_dir/put-create" \
        --request PUT --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$route/2"
    expect_status 204 "$tmp_dir/put-create-cleanup" \
        --request DELETE -H "Authorization: Bearer $api_key" "$route/2"

    expect_status 400 "$tmp_dir/post-over-slot-limit" \
        --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$route/$over_limit_slot"
    expect_status 400 "$tmp_dir/put-create-over-slot-limit" \
        --request PUT --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$route/$over_limit_slot"

    if [[ "$auth_mode" == "true" ]]; then
        expect_status 401 "$tmp_dir/unauthorized-get" "$image_route"
        expect_status 401 "$tmp_dir/unauthorized-head" --head "$image_route"
        expect_status 401 "$tmp_dir/unauthorized-list" "$route"
        expect_status 401 "$tmp_dir/unauthorized-info" "$image_route/info"
        expect_status 401 "$tmp_dir/unauthorized-presign" --request POST "$image_route/presign"
    fi

    local get_status
    if [[ "$auth_mode" == "true" ]]; then
        get_status="$(request_status_with_headers "$tmp_dir/get" "$tmp_dir/get.headers" -H "Authorization: Bearer $api_key" "$image_route")"
    else
        get_status="$(request_status_with_headers "$tmp_dir/get" "$tmp_dir/get.headers" "$image_route")"
    fi
    [[ "$get_status" == "200" ]] || fail "expected HTTP 200 for image GET, got $get_status"
    [[ -s "$tmp_dir/get" ]] || fail "GET returned an empty image body in $auth_mode mode"
    grep -qi '^ETag: "[[:xdigit:]]\{64\}"' "$tmp_dir/get.headers" || fail "GET did not return a SHA-256 ETag"
    grep -qi '^Last-Modified: ' "$tmp_dir/get.headers" || fail "GET did not return Last-Modified"
    local expected_cache_control='public, max-age=60'
    [[ "$auth_mode" == "true" ]] && expected_cache_control='private, max-age=60'
    grep -qi "^Cache-Control: $expected_cache_control" "$tmp_dir/get.headers" || fail "unexpected default Cache-Control policy"

    local etag last_modified
    etag="$(awk 'tolower($1) == "etag:" {sub(/^[^:]*:[[:space:]]*/, ""); gsub(/\r/, ""); print; exit}' "$tmp_dir/get.headers")"
    last_modified="$(awk 'tolower($1) == "last-modified:" {sub(/^[^:]*:[[:space:]]*/, ""); gsub(/\r/, ""); print; exit}' "$tmp_dir/get.headers")"
    expect_read_status 304 "$tmp_dir/etag-not-modified" -H "If-None-Match: $etag" "$image_route"
    [[ ! -s "$tmp_dir/etag-not-modified" ]] || fail "304 ETag response included a body"
    expect_read_status 304 "$tmp_dir/date-not-modified" -H "If-Modified-Since: $last_modified" "$image_route"
    [[ ! -s "$tmp_dir/date-not-modified" ]] || fail "304 date response included a body"
    expect_read_status 304 "$tmp_dir/head-not-modified" --head -H "If-Modified-Since: $last_modified" "$image_route"

    expect_read_status 200 "$tmp_dir/head" --head "$image_route"
    grep -qi '^Content-Type: image/webp' "$tmp_dir/head" || fail "HEAD did not return image/webp"
    grep -qi '^Content-Length: [1-9][0-9]*' "$tmp_dir/head" || fail "HEAD did not return a positive Content-Length"
    grep -qi "^Cache-Control: $expected_cache_control" "$tmp_dir/head" || fail "HEAD omitted default Cache-Control policy"

    expect_read_status 200 "$tmp_dir/info" "$image_route/info"
    local max_dimension=1024
    local expected_profile="default"
    local expected_output_limit=102400
    [[ "$auth_mode" == "dimensions" || "$auth_mode" == "profiles" ]] && max_dimension=64
    if [[ "$auth_mode" == "profiles" ]]; then
        expected_profile="compact"
        expected_output_limit=51200
    fi
    python3 - "$tmp_dir/info" "$max_dimension" "$expected_profile" "$expected_output_limit" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["namespace"] == "products", payload
assert payload["slot"] == 1, payload
assert payload["format"] == "webp", payload
assert payload["profile"] == sys.argv[3], payload
assert payload["size_bytes"] > 0, payload
assert payload["width"] > 0 and payload["height"] > 0, payload
max_dimension = int(sys.argv[2])
assert payload["width"] <= max_dimension and payload["height"] <= max_dimension, payload
if max_dimension == 64:
    assert payload["width"] == 64 and payload["height"] == 64, payload
assert payload["size_bytes"] <= 102400, payload
assert payload["size_bytes"] <= int(sys.argv[4]), payload
assert payload["etag"].startswith('"') and payload["etag"].endswith('"'), payload
assert payload["last_modified"], payload
PY

    expect_read_status 200 "$tmp_dir/list" "$route"
    python3 - "$tmp_dir/list" "$max_creatable_slot" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["namespace"] == "products", payload
assert payload["max_creatable_slot"] == int(sys.argv[2]), payload
assert [image["slot"] for image in payload["images"]] == [1], payload
assert payload["images"][0]["size_bytes"] > 0, payload
if "etag" in payload["images"][0]:
    assert payload["images"][0]["etag"].startswith('"'), payload
PY

    if [[ "$auth_mode" == "max-slots" ]]; then
        local slot_filename
        printf -v slot_filename '%02d.webp' "$over_limit_slot"
        mkdir -p "$tmp_dir/seed/products/$resource"
        cp "$tmp_dir/get" "$tmp_dir/seed/products/$resource/$slot_filename"
        docker run --rm \
            --mount "type=volume,source=$storage_volume,target=/storage" \
            --mount "type=bind,source=$tmp_dir/seed,target=/seed,readonly" \
            rust:1.88-bookworm \
            sh -c "mkdir -p /storage/products/$resource && cp /seed/products/$resource/$slot_filename /storage/products/$resource/$slot_filename" \
            || fail "could not seed a pre-existing slot above IMAGE_MAX_SLOTS"
        local high_slot_route="$route/$over_limit_slot"
        expect_read_status 200 "$tmp_dir/get-over-limit-existing" "$high_slot_route"
        expect_read_status 200 "$tmp_dir/head-over-limit-existing" --head "$high_slot_route"
        expect_read_status 200 "$tmp_dir/info-over-limit-existing" "$high_slot_route/info"

        expect_read_status 200 "$tmp_dir/list-with-over-limit-existing" "$route"
        python3 - "$tmp_dir/list-with-over-limit-existing" "$max_creatable_slot" "$over_limit_slot" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["max_creatable_slot"] == int(sys.argv[2]), payload
assert [image["slot"] for image in payload["images"]] == [1, int(sys.argv[3])], payload
PY

        expect_status 200 "$tmp_dir/put-replace-over-limit-existing" \
            --request PUT --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$high_slot_route"
        expect_status 204 "$tmp_dir/delete-over-limit-existing" \
            --request DELETE -H "Authorization: Bearer $api_key" "$high_slot_route"
        expect_read_status 404 "$tmp_dir/get-deleted-over-limit-existing" "$high_slot_route"
        expect_read_status 200 "$tmp_dir/list-after-high-delete" "$route"
        python3 - "$tmp_dir/list-after-high-delete" "$max_creatable_slot" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["max_creatable_slot"] == int(sys.argv[2]), payload
assert [image["slot"] for image in payload["images"]] == [1], payload
PY
    fi

    expect_read_status 501 "$tmp_dir/presign-unsupported" --request POST "$image_route/presign"
    python3 - "$tmp_dir/presign-unsupported" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["error"] == "Presigned reads are not supported by this storage backend", payload
PY

    expect_status 401 "$tmp_dir/unauthorized-delete" --request DELETE "$image_route"
    expect_status 204 "$tmp_dir/delete" --request DELETE -H "Authorization: Bearer $api_key" "$image_route"
    expect_status 204 "$tmp_dir/delete-again" --request DELETE -H "Authorization: Bearer $api_key" "$image_route"
    expect_read_status 404 "$tmp_dir/get-after-delete" "$image_route"
    expect_read_status 200 "$tmp_dir/list-after-delete" "$route"
    python3 - "$tmp_dir/list-after-delete" "$max_creatable_slot" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["max_creatable_slot"] == int(sys.argv[2]), payload
assert payload["images"] == [], payload
PY

    local resource_delete_route="$base_url/api/images/products/resource-delete-${auth_mode}"
    local sibling_route="$base_url/api/images/products/resource-delete-${auth_mode}-sibling"
    for slot in 1 2; do
        if (( slot <= max_creatable_slot )); then
            expect_status 201 "$tmp_dir/resource-delete-upload-$slot" \
                --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$resource_delete_route/$slot"
        fi
    done
    expect_status 201 "$tmp_dir/resource-sibling-upload" \
        --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$sibling_route/1"
    expect_status 401 "$tmp_dir/unauthorized-resource-delete" --request DELETE "$resource_delete_route"
    expect_status 204 "$tmp_dir/resource-delete" --request DELETE -H "Authorization: Bearer $api_key" "$resource_delete_route"
    expect_status 204 "$tmp_dir/resource-delete-again" --request DELETE -H "Authorization: Bearer $api_key" "$resource_delete_route"
    expect_read_status 404 "$tmp_dir/resource-delete-get" "$resource_delete_route/1"
    expect_read_status 200 "$tmp_dir/resource-sibling-preserved" "$sibling_route/1"
    expect_read_status 200 "$tmp_dir/resource-delete-empty-list" "$resource_delete_route"
    python3 - "$tmp_dir/resource-delete-empty-list" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["images"] == [], payload
PY

    if [[ "$auth_mode" == "profiles" ]]; then
        python3 - "$tmp_dir/tiny.png" <<'PY'
import struct
import sys
import zlib

def chunk(kind, data):
    checksum = zlib.crc32(kind + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", checksum)

width = height = 16
pixels = b"".join(bytes((0,)) + bytes((255, 0, 0)) * width for _ in range(height))
png = bytes((137, 80, 78, 71, 13, 10, 26, 10))
png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(pixels))
png += chunk(b"IEND", b"")
with open(sys.argv[1], "wb") as image_file:
    image_file.write(png)
PY
        local avatar_route="$base_url/api/images/avatars/profile-smoke/1"
        expect_status 201 "$tmp_dir/avatar-profile-upload" \
            --request POST --form "image=@$tmp_dir/tiny.png" -H "Authorization: Bearer $api_key" "$avatar_route"
        expect_status 200 "$tmp_dir/avatar-profile-info" "$avatar_route/info"
        python3 - "$tmp_dir/avatar-profile-info" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["profile"] == "avatar", payload
assert payload["width"] == 32 and payload["height"] == 32, payload
assert payload["size_bytes"] <= 25600, payload
PY
        expect_status 413 "$tmp_dir/avatar-input-inherits-global-limit" \
            --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$base_url/api/images/avatars/profile-smoke/2"

        local fallback_route="$base_url/api/images/misc/profile-smoke/1"
        expect_status 201 "$tmp_dir/default-profile-upload" \
            --request POST --form "image=@$tmp_dir/tiny.png" -H "Authorization: Bearer $api_key" "$fallback_route"
        expect_status 200 "$tmp_dir/default-profile-info" "$fallback_route/info"
        python3 - "$tmp_dir/default-profile-info" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["profile"] == "default", payload
assert payload["width"] == 64 and payload["height"] == 64, payload
assert payload["size_bytes"] <= 102400, payload
PY
        expect_status 413 "$tmp_dir/unassigned-namespace-input-global-limit" \
            --request POST --form "image=@$image_file" -H "Authorization: Bearer $api_key" "$base_url/api/images/misc/profile-smoke/2"
    fi

    stop_service
    echo "PASS: endpoint and auth smoke tests ($auth_mode GETMETHOD_AUTH)"
}

if docker run --rm --read-only --env GETMETHOD_AUTH=true "$image_name" >"$tmp_dir/invalid-config.log" 2>&1; then
    fail "GETMETHOD_AUTH=true without IMAGE_API_KEY unexpectedly started"
fi
grep -q 'GETMETHOD_AUTH=true requires IMAGE_API_KEY' "$tmp_dir/invalid-config.log" \
    || fail "invalid auth configuration did not report the expected startup error"
echo "PASS: invalid auth configuration is rejected"

if docker run --rm --read-only --env IMAGE_PRESIGN_TTL_SECONDS=0 "$image_name" >"$tmp_dir/invalid-ttl.log" 2>&1; then
    fail "IMAGE_PRESIGN_TTL_SECONDS=0 unexpectedly started"
fi
grep -q 'IMAGE_PRESIGN_TTL_SECONDS must be between 1 and 604800' "$tmp_dir/invalid-ttl.log" \
    || fail "invalid presign TTL did not report the expected startup error"
echo "PASS: invalid presigned URL TTL is rejected"

for setting in IMAGE_MAX_INPUT_BYTES IMAGE_MAX_OUTPUT_BYTES IMAGE_OUTPUT_WIDTH IMAGE_OUTPUT_HEIGHT IMAGE_MAX_SLOTS; do
    if docker run --rm --read-only --env "$setting=0" "$image_name" >"$tmp_dir/invalid-$setting.log" 2>&1; then
        fail "$setting=0 unexpectedly started"
    fi
    grep -q "$setting must be between 1" "$tmp_dir/invalid-$setting.log" \
        || fail "invalid $setting did not report the expected startup error"
done
echo "PASS: image limit configuration rejects zero values"

if docker run --rm --read-only \
    --env 'IMAGE_PROCESSING_PROFILES={"avatar":{"output_width":512}}' \
    --env 'IMAGE_NAMESPACE_PROFILES={"avatars":"missing"}' \
    "$image_name" >"$tmp_dir/invalid-profile.log" 2>&1; then
    fail "invalid processing profile configuration unexpectedly started"
fi
grep -Eq 'must set output_width and output_height together|references undefined image processing profile' "$tmp_dir/invalid-profile.log" \
    || fail "invalid processing profile did not report a profile configuration error"
echo "PASS: invalid processing profile configuration is rejected"

if docker run --rm --read-only \
    --env 'IMAGE_PROCESSING_PROFILES={"avatar":{"max_input_bytes":0}}' \
    --env 'IMAGE_NAMESPACE_PROFILES={"avatars":"avatar"}' \
    "$image_name" >"$tmp_dir/invalid-profile-input-limit.log" 2>&1; then
    fail "zero profile input limit unexpectedly started"
fi
grep -q 'Profile avatar max_input_bytes must be greater than zero' "$tmp_dir/invalid-profile-input-limit.log" \
    || fail "zero profile input limit did not report the expected startup error"
echo "PASS: processing profile input limits reject zero"

test_mode default
test_mode false
test_mode true
test_mode dimensions
test_mode profiles
test_mode max-slots
test_mode input-limit
test_mode output-limit

echo "All Image Service smoke tests passed. Containers used volatile tmpfs storage only."
