#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image_name="${IMAGE_SERVICE_IMAGE:-image-service:local}"
minio_image="${MINIO_IMAGE:-pgsty/minio:latest}"
image_file="${1:-$repo_dir/TEST_IMAGE_ORIGINAL.png}"
suffix="$$"
network="image-service-minio-test-$suffix"
minio_name="image-service-minio-$suffix"
service_name="image-service-s3-$suffix"
access_key="minio-test-user"
secret_key="minio-test-password-123"
bucket="image-service-smoke"
tmp_dir="$(mktemp -d)"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

cleanup() {
    docker rm --force "$service_name" "$minio_name" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

[[ -f "$image_file" ]] || fail "test image not found: $image_file"
docker image inspect "$image_name" >/dev/null 2>&1 || fail "service image not found: $image_name (build it first)"
docker image inspect "$minio_image" >/dev/null 2>&1 || fail "MinIO image not found: $minio_image (pull it first)"

docker network create "$network" >/dev/null
docker run --detach --rm \
    --name "$minio_name" \
    --network "$network" \
    --tmpfs /data:rw,nosuid,size=512m,mode=1777 \
    --publish 127.0.0.1::9000 \
    --env "MINIO_ROOT_USER=$access_key" \
    --env "MINIO_ROOT_PASSWORD=$secret_key" \
    "$minio_image" server /data --console-address :9001 >/dev/null

minio_port="$(docker port "$minio_name" 9000/tcp | awk -F: 'NR == 1 {print $NF}')"
minio_url="http://127.0.0.1:$minio_port"
for _ in $(seq 1 100); do
    if curl --fail --silent "$minio_url/minio/health/live" >/dev/null; then
        break
    fi
    sleep 0.2
done
curl --fail --silent "$minio_url/minio/health/live" >/dev/null || fail "MinIO did not become healthy"

# Create a bucket with S3 Signature V4, without installing another client.
curl --fail --silent --show-error \
    --aws-sigv4 "aws:amz:us-east-1:s3" \
    --user "$access_key:$secret_key" \
    --request PUT "$minio_url/$bucket" >/dev/null

docker run --detach --rm \
    --name "$service_name" \
    --network "$network" \
    --publish 127.0.0.1::3000 \
    --env IMAGE_BIND=0.0.0.0:3000 \
    --env IMAGE_READ_AUTH=none \
    --env IMAGE_WRITE_AUTH=none \
    --env IMAGE_STORAGE_SCHEME=s3 \
    --env IMAGE_STORAGE_BUCKET="$bucket" \
    --env IMAGE_STORAGE_REGION=us-east-1 \
    --env IMAGE_STORAGE_ENDPOINT="http://$minio_name:9000" \
    --env IMAGE_STORAGE_ENABLE_VIRTUAL_HOST_STYLE=false \
    --env IMAGE_STORAGE_ACCESS_KEY_ID="$access_key" \
    --env IMAGE_STORAGE_SECRET_ACCESS_KEY="$secret_key" \
    --env IMAGE_MAX_SLOTS=1 \
    --env IMAGE_OUTPUT_WIDTH=1024 \
    --env IMAGE_OUTPUT_HEIGHT=1024 \
    --env 'IMAGE_PROCESSING_PROFILES={"avatar":{"max_output_bytes":25600,"output_width":32,"output_height":32}}' \
    --env 'IMAGE_NAMESPACE_PROFILES={"profile-smoke":"avatar"}' \
    "$image_name" >/dev/null

service_port="$(docker port "$service_name" 3000/tcp | awk -F: 'NR == 1 {print $NF}')"
service_url="http://127.0.0.1:$service_port"
for _ in $(seq 1 100); do
    if curl --fail --silent "$service_url/health" >/dev/null; then
        break
    fi
    sleep 0.2
done
curl --fail --silent "$service_url/health" >/dev/null || fail "Image Service did not become healthy"
curl --fail --silent "$service_url/health/live" >/dev/null || fail "Image Service liveness probe failed"
curl --fail --silent "$service_url/health/ready" >/dev/null || fail "MinIO storage readiness probe failed"

image_route="$service_url/api/images/minio-smoke/1/1"
printf 'not an image' >"$tmp_dir/not-image.bin"
status="$(curl --silent --output "$tmp_dir/upload.json" --write-out '%{http_code}' \
    --form "image=@$image_file" "$image_route")"
[[ "$status" == 201 ]] || fail "MinIO-backed POST returned HTTP $status: $(cat "$tmp_dir/upload.json")"
status="$(curl --silent --output "$tmp_dir/conflict.json" --write-out '%{http_code}' \
    --form "image=@$tmp_dir/not-image.bin" "$image_route")"
[[ "$status" == 409 ]] || fail "MinIO-backed duplicate POST returned HTTP $status"
status="$(curl --silent --output "$tmp_dir/put.json" --write-out '%{http_code}' \
    --request PUT --form "image=@$image_file" "$image_route")"
[[ "$status" == 200 ]] || fail "MinIO-backed PUT returned HTTP $status: $(cat "$tmp_dir/put.json")"

status="$(curl --silent --output "$tmp_dir/image.webp" --write-out '%{http_code}' "$image_route")"
[[ "$status" == 200 ]] || fail "MinIO-backed GET returned HTTP $status"
[[ -s "$tmp_dir/image.webp" ]] || fail "MinIO-backed GET returned an empty object"
status="$(curl --silent --dump-header "$tmp_dir/get.headers" --output /dev/null --write-out '%{http_code}' "$image_route")"
[[ "$status" == 200 ]] || fail "second MinIO-backed GET returned HTTP $status"
etag="$(awk 'tolower($1) == "etag:" {sub(/^[^:]*:[[:space:]]*/, ""); gsub(/\r/, ""); print; exit}' "$tmp_dir/get.headers")"
[[ -n "$etag" ]] || fail "MinIO-backed GET did not return an ETag"
status="$(curl --silent --dump-header "$tmp_dir/conditional.headers" --output "$tmp_dir/conditional.body" \
    --write-out '%{http_code}' --header "If-None-Match: $etag" "$image_route")"
[[ "$status" == 304 && ! -s "$tmp_dir/conditional.body" ]] || fail "MinIO conditional GET did not return an empty 304"
status="$(curl --silent --dump-header "$tmp_dir/head.headers" --output /dev/null --write-out '%{http_code}' \
    --head "$image_route")"
[[ "$status" == 200 ]] || fail "MinIO-backed HEAD returned HTTP $status"
grep -qi '^Content-Length: [1-9][0-9]*' "$tmp_dir/head.headers" || fail "MinIO-backed HEAD omitted Content-Length"

# Race service GET snapshots against writes made directly through S3, which
# bypasses the process-local resource lock and exercises OpenDAL If-Match.
snapshot_route="$service_url/api/images/minio-smoke/snapshot-read/1"
snapshot_object="$minio_url/$bucket/minio-smoke/snapshot-read/01.webp"
printf 'snapshot-version-a' >"$tmp_dir/snapshot-a.bin"
printf 'snapshot-version-b-is-longer' >"$tmp_dir/snapshot-b.bin"
put_snapshot_version() {
    curl --fail --silent --show-error \
        --aws-sigv4 "aws:amz:us-east-1:s3" \
        --user "$access_key:$secret_key" \
        --upload-file "$1" "$snapshot_object" >/dev/null
}
put_snapshot_version "$tmp_dir/snapshot-a.bin"
(
    for attempt in $(seq 1 40); do
        if (( attempt % 2 == 0 )); then
            put_snapshot_version "$tmp_dir/snapshot-b.bin"
        else
            put_snapshot_version "$tmp_dir/snapshot-a.bin"
        fi
    done
) &
writer_pid=$!
for attempt in $(seq 1 40); do
    status="$(curl --silent --dump-header "$tmp_dir/snapshot-$attempt.headers" \
        --output "$tmp_dir/snapshot-$attempt.body" --write-out '%{http_code}' "$snapshot_route")"
    [[ "$status" == 200 ]] || fail "concurrent MinIO snapshot GET returned HTTP $status"
done
wait "$writer_pid"
python3 - "$tmp_dir" <<'PY'
import hashlib
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
allowed = {
    (root / "snapshot-a.bin").read_bytes(),
    (root / "snapshot-b.bin").read_bytes(),
}
for body_path in root.glob("snapshot-[0-9]*.body"):
    body = body_path.read_bytes()
    assert body in allowed, (body_path, body)
    header_path = body_path.with_suffix(".headers")
    headers = header_path.read_text(encoding="latin-1").splitlines()
    etag = next(line.split(":", 1)[1].strip() for line in headers if line.lower().startswith("etag:"))
    assert etag.strip('"') == hashlib.md5(body).hexdigest(), (body_path, etag)
PY

# Once a replacement has completed, a validator from the old object must not
# cause a false 304 for the new representation.
status="$(curl --silent --dump-header "$tmp_dir/snapshot-old.headers" \
    --output "$tmp_dir/snapshot-old.body" --write-out '%{http_code}' "$snapshot_route")"
[[ "$status" == 200 ]] || fail "MinIO snapshot baseline GET returned HTTP $status"
old_etag="$(awk 'tolower($1) == "etag:" {sub(/^[^:]*:[[:space:]]*/, ""); gsub(/\r/, ""); print; exit}' "$tmp_dir/snapshot-old.headers")"
put_snapshot_version "$tmp_dir/snapshot-a.bin"
status="$(curl --silent --dump-header "$tmp_dir/snapshot-after-put.headers" \
    --output "$tmp_dir/snapshot-after-put.body" --write-out '%{http_code}' \
    --header "If-None-Match: $old_etag" "$snapshot_route")"
[[ "$status" == 200 ]] || fail "MinIO GET with a stale ETag returned HTTP $status instead of 200"
cmp -s "$tmp_dir/snapshot-a.bin" "$tmp_dir/snapshot-after-put.body" \
    || fail "MinIO GET with a stale ETag returned bytes from the old version"

status="$(curl --silent --output "$tmp_dir/info.json" --write-out '%{http_code}' "$image_route/info")"
[[ "$status" == 200 ]] || fail "MinIO-backed info returned HTTP $status"
python3 - "$tmp_dir/info.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    metadata = json.load(response_file)
assert metadata["format"] == "webp", metadata
assert metadata["size_bytes"] > 0, metadata
assert metadata["size_bytes"] <= 102400, metadata
assert 0 < metadata["width"] <= 1024 and 0 < metadata["height"] <= 1024, metadata
PY

profile_route="$service_url/api/images/profile-smoke/1/1"
status="$(curl --silent --output "$tmp_dir/profile-upload.json" --write-out '%{http_code}' \
    --form "image=@$image_file" "$profile_route")"
[[ "$status" == 201 ]] || fail "MinIO profile upload returned HTTP $status"
status="$(curl --silent --output "$tmp_dir/profile-info.json" --write-out '%{http_code}' "$profile_route/info")"
[[ "$status" == 200 ]] || fail "MinIO profile info returned HTTP $status"
python3 - "$tmp_dir/profile-info.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["profile"] == "avatar", payload
assert payload["width"] == 32 and payload["height"] == 32, payload
assert payload["size_bytes"] <= 25600, payload
PY

status="$(curl --silent --output "$tmp_dir/list.json" --write-out '%{http_code}' \
    "$service_url/api/images/minio-smoke/1")"
[[ "$status" == 200 ]] || fail "MinIO-backed list returned HTTP $status"
python3 - "$tmp_dir/list.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    listing = json.load(response_file)
assert [item["slot"] for item in listing["images"]] == [1], listing
assert listing["max_creatable_slot"] == 1, listing
assert listing["images"][0]["size_bytes"] > 0, listing
if "etag" in listing["images"][0]:
    assert listing["images"][0]["etag"].startswith('"'), listing
PY

status="$(curl --silent --output "$tmp_dir/presign.json" --write-out '%{http_code}' \
    --request POST "$image_route/presign")"
[[ "$status" == 200 ]] || fail "MinIO presign returned HTTP $status: $(cat "$tmp_dir/presign.json")"
python3 - "$tmp_dir/presign.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    payload = json.load(response_file)
assert payload["method"] == "GET", payload
assert payload["expires_in"] > 0, payload
assert payload["url"].startswith("http://"), payload
PY
presigned_url="$(python3 - "$tmp_dir/presign.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    print(json.load(response_file)["url"])
PY
)"
status="$(curl --silent --show-error \
    --connect-to "$minio_name:9000:127.0.0.1:$minio_port" \
    --output "$tmp_dir/presigned.webp" --write-out '%{http_code}' "$presigned_url")"
[[ "$status" == 200 ]] || fail "direct GET via the MinIO presigned URL returned HTTP $status"
cmp -s "$tmp_dir/image.webp" "$tmp_dir/presigned.webp" || fail "presigned GET returned different image bytes"

# Seed a legacy slot above the configured creation ceiling directly in S3.
curl --fail --silent --show-error \
    --aws-sigv4 "aws:amz:us-east-1:s3" \
    --user "$access_key:$secret_key" \
    --upload-file "$tmp_dir/image.webp" \
    "$minio_url/$bucket/minio-smoke/1/02.webp" >/dev/null
high_slot_route="$service_url/api/images/minio-smoke/1/2"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' "$high_slot_route")"
[[ "$status" == 200 ]] || fail "existing MinIO slot above IMAGE_MAX_SLOTS was not readable (HTTP $status)"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --head "$high_slot_route")"
[[ "$status" == 200 ]] || fail "existing MinIO slot above IMAGE_MAX_SLOTS failed HEAD (HTTP $status)"
status="$(curl --silent --output "$tmp_dir/put-over-limit.json" --write-out '%{http_code}' \
    --request PUT --form "image=@$image_file" "$high_slot_route")"
[[ "$status" == 200 ]] || fail "PUT did not replace the existing MinIO slot above the limit (HTTP $status)"
status="$(curl --silent --output "$tmp_dir/new-over-limit.json" --write-out '%{http_code}' \
    --request PUT --form "image=@$image_file" "$service_url/api/images/minio-smoke/1/3")"
[[ "$status" == 400 ]] || fail "PUT created a new MinIO slot above IMAGE_MAX_SLOTS (HTTP $status)"
status="$(curl --silent --output "$tmp_dir/post-over-limit.json" --write-out '%{http_code}' \
    --form "image=@$image_file" "$service_url/api/images/minio-smoke/1/3")"
[[ "$status" == 400 ]] || fail "POST created a MinIO slot above IMAGE_MAX_SLOTS (HTTP $status)"
status="$(curl --silent --output "$tmp_dir/list-with-legacy-slot.json" --write-out '%{http_code}' \
    "$service_url/api/images/minio-smoke/1")"
[[ "$status" == 200 ]] || fail "MinIO listing with legacy slot returned HTTP $status"
python3 - "$tmp_dir/list-with-legacy-slot.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    listing = json.load(response_file)
assert listing["max_creatable_slot"] == 1, listing
assert [item["slot"] for item in listing["images"]] == [1, 2], listing
PY

status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$high_slot_route")"
[[ "$status" == 204 ]] || fail "MinIO-backed DELETE above IMAGE_MAX_SLOTS returned HTTP $status"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$high_slot_route")"
[[ "$status" == 204 ]] || fail "MinIO-backed high-slot DELETE was not idempotent"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$image_route")"
[[ "$status" == 204 ]] || fail "MinIO-backed DELETE returned HTTP $status"
status="$(curl --silent --output "$tmp_dir/get-after-delete.json" --write-out '%{http_code}' "$image_route")"
[[ "$status" == 404 ]] || fail "MinIO-backed GET after deletion returned HTTP $status"

# Delete all objects under an exact resource prefix, including slots above
# IMAGE_MAX_SLOTS, while preserving a similarly named resource.
resource_route="$service_url/api/images/minio-smoke/resource-delete"
sibling_route="$service_url/api/images/minio-smoke/resource-delete-extra"
status="$(curl --silent --output "$tmp_dir/resource-upload.json" --write-out '%{http_code}' \
    --form "image=@$image_file" "$resource_route/1")"
[[ "$status" == 201 ]] || fail "resource-delete fixture upload returned HTTP $status"
curl --fail --silent --show-error \
    --aws-sigv4 "aws:amz:us-east-1:s3" \
    --user "$access_key:$secret_key" \
    --upload-file "$tmp_dir/image.webp" \
    "$minio_url/$bucket/minio-smoke/resource-delete/02.webp" >/dev/null
status="$(curl --silent --output "$tmp_dir/sibling-upload.json" --write-out '%{http_code}' \
    --form "image=@$image_file" "$sibling_route/1")"
[[ "$status" == 201 ]] || fail "resource-delete sibling fixture upload returned HTTP $status"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$resource_route")"
[[ "$status" == 204 ]] || fail "MinIO resource-level DELETE returned HTTP $status"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$resource_route")"
[[ "$status" == 204 ]] || fail "MinIO resource-level DELETE was not idempotent"
for slot in 1 2; do
    status="$(curl --silent --output /dev/null --write-out '%{http_code}' "$resource_route/$slot")"
    [[ "$status" == 404 ]] || fail "MinIO resource DELETE left slot $slot behind (HTTP $status)"
done
status="$(curl --silent --output /dev/null --write-out '%{http_code}' "$sibling_route/1")"
[[ "$status" == 200 ]] || fail "MinIO resource DELETE removed a similarly named resource (HTTP $status)"
status="$(curl --silent --output "$tmp_dir/resource-delete-list.json" --write-out '%{http_code}' "$resource_route")"
[[ "$status" == 200 ]] || fail "MinIO resource listing after DELETE returned HTTP $status"
python3 - "$tmp_dir/resource-delete-list.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response_file:
    listing = json.load(response_file)
assert listing["images"] == [], listing
PY

# Hold uploads open long enough for DELETE to invalidate their generation.
# The DELETE response must not be followed by a stale upload publishing again.
slot_race_route="$service_url/api/images/minio-smoke/concurrent-slot/1"
curl --silent --output "$tmp_dir/slot-race-upload.json" --write-out '%{http_code}' \
    --limit-rate 512k --form "image=@$image_file" "$slot_race_route" \
    >"$tmp_dir/slot-race-upload.status" &
upload_pid=$!
sleep 0.5
kill -0 "$upload_pid" 2>/dev/null || fail "slot-race upload finished before DELETE"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$slot_race_route")"
[[ "$status" == 204 ]] || fail "MinIO slot DELETE during upload returned HTTP $status"
wait "$upload_pid" || true
status="$(cat "$tmp_dir/slot-race-upload.status")"
[[ "$status" == 409 ]] || fail "pre-delete MinIO upload returned HTTP $status instead of 409"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' "$slot_race_route")"
[[ "$status" == 404 ]] || fail "pre-delete MinIO upload recreated the slot (HTTP $status)"

resource_race_route="$service_url/api/images/minio-smoke/concurrent-resource"
curl --silent --output "$tmp_dir/resource-race-upload.json" --write-out '%{http_code}' \
    --limit-rate 512k --form "image=@$image_file" "$resource_race_route/1" \
    >"$tmp_dir/resource-race-upload.status" &
upload_pid=$!
sleep 0.5
kill -0 "$upload_pid" 2>/dev/null || fail "resource-race upload finished before DELETE"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE "$resource_race_route")"
[[ "$status" == 204 ]] || fail "MinIO resource DELETE during upload returned HTTP $status"
wait "$upload_pid" || true
status="$(cat "$tmp_dir/resource-race-upload.status")"
[[ "$status" == 409 ]] || fail "pre-delete MinIO resource upload returned HTTP $status instead of 409"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' "$resource_race_route/1")"
[[ "$status" == 404 ]] || fail "pre-delete MinIO upload recreated resource content (HTTP $status)"

cancel_route="$service_url/api/images/minio-smoke/cancelled-upload/1"
curl --silent --output "$tmp_dir/cancelled-upload.json" --write-out '%{http_code}' \
    --limit-rate 512k --form "image=@$image_file" "$cancel_route" \
    >"$tmp_dir/cancelled-upload.status" &
upload_pid=$!
sleep 0.5
kill -0 "$upload_pid" 2>/dev/null || fail "cancel-test upload finished before client cancellation"
kill "$upload_pid" 2>/dev/null || true
wait "$upload_pid" 2>/dev/null || true
sleep 0.2
status="$(curl --silent --output /dev/null --write-out '%{http_code}' "$cancel_route")"
[[ "$status" == 404 ]] || fail "cancelled MinIO upload stored an incomplete object (HTTP $status)"

echo "PASS: MinIO uploads, replacement, snapshot-consistent GETs during external writes, stale conditional validators, HEAD, metadata, listing, profiles, presigned GET, DELETE races, cancellation, and resource DELETE"
echo "Both containers used ephemeral storage; all test data was removed during cleanup."
