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
    docker logs "$service_name" >&2 2>/dev/null || true
    exit 1
}

wait_for_image_job() {
    local job_id="$1"
    local output_file="$2"
    local status
    for _ in $(seq 1 100); do
        status="$(curl --silent --show-error --output "$output_file" --write-out '%{http_code}' \
            -H 'Authorization: Bearer storage-usage-smoke-key' "$service_url/api/image-jobs/$job_id")"
        [[ "$status" == 200 ]] || fail "MinIO image job status returned HTTP $status"
        if python3 - "$output_file" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    value = json.load(f)
sys.exit(0 if value["status"] in ("succeeded", "failed", "cancelled", "superseded") else 1)
PY
        then
            return
        fi
        sleep 0.2
    done
    fail "MinIO image job did not reach a terminal state"
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
    --env IMAGE_API_KEY=storage-usage-smoke-key \
    --env IMAGE_STORAGE_USAGE_ENABLED=true \
    --env IMAGE_BACKGROUND_IDLE_SECONDS=0 \
    --env IMAGE_STORAGE_SCHEME=s3 \
    --env IMAGE_STORAGE_BUCKET="$bucket" \
    --env IMAGE_STORAGE_REGION=us-east-1 \
    --env IMAGE_STORAGE_ENDPOINT="http://$minio_name:9000" \
    --env IMAGE_STORAGE_ENABLE_VIRTUAL_HOST_STYLE=false \
    --env IMAGE_STORAGE_ACCESS_KEY_ID="$access_key" \
    --env IMAGE_STORAGE_SECRET_ACCESS_KEY="$secret_key" \
    --env IMAGE_MAX_SLOTS=1 \
    --env "IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES=${IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES:-1}" \
    --env IMAGE_OUTPUT_WIDTH=1024 \
    --env IMAGE_OUTPUT_HEIGHT=1024 \
    --env 'IMAGE_PROCESSING_PROFILES={"avatar":{"max_output_bytes":25600,"output_width":32,"output_height":32},"maintenance":{"output_width":64,"output_height":64}}' \
    --env 'IMAGE_NAMESPACE_PROFILES={"profile-smoke":"avatar","maintenance-smoke":"maintenance"}' \
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

# Exercise durable staged processing on the S3-compatible backend.
async_route="$service_url/api/image-jobs/minio-smoke/async-resource/1"
status="$(curl --silent --output "$tmp_dir/async-created.json" --write-out '%{http_code}' \
    --request POST --form "image=@$image_file" --header 'Authorization: Bearer storage-usage-smoke-key' \
    --header 'Idempotency-Key: minio-async-post-1' "$async_route")"
[[ "$status" == 202 ]] || fail "MinIO async POST returned HTTP $status: $(cat "$tmp_dir/async-created.json")"
async_job_id="$(python3 - "$tmp_dir/async-created.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    print(json.load(f)["job_id"])
PY
)"
wait_for_image_job "$async_job_id" "$tmp_dir/async-status.json"
python3 - "$tmp_dir/async-status.json" <<'PY' || fail "MinIO async POST job did not succeed"
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    value = json.load(f)
assert value["status"] == "succeeded" and value["result"]["size_bytes"] > 0, value
PY
status="$(curl --silent --output "$tmp_dir/async-published.webp" --write-out '%{http_code}' \
    -H 'Authorization: Bearer storage-usage-smoke-key' "$service_url/api/images/minio-smoke/async-resource/1")"
[[ "$status" == 200 ]] || fail "MinIO async POST result returned HTTP $status"
status="$(curl --silent --output "$tmp_dir/async-conflict.json" --write-out '%{http_code}' \
    --request POST --form "image=@$image_file" --header 'Authorization: Bearer storage-usage-smoke-key' "$async_route")"
[[ "$status" == 409 ]] || fail "MinIO async duplicate POST returned HTTP $status"

status="$(curl --silent --output "$tmp_dir/async-put.json" --write-out '%{http_code}' \
    --request PUT --form "image=@$image_file" --header 'Authorization: Bearer storage-usage-smoke-key' \
    --header 'Idempotency-Key: minio-async-put-1' "$async_route")"
[[ "$status" == 202 ]] || fail "MinIO async PUT returned HTTP $status: $(cat "$tmp_dir/async-put.json")"
async_put_job_id="$(python3 - "$tmp_dir/async-put.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    print(json.load(f)["job_id"])
PY
)"
wait_for_image_job "$async_put_job_id" "$tmp_dir/async-put-status.json"
python3 - "$tmp_dir/async-put-status.json" <<'PY' || fail "MinIO async PUT job did not succeed"
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    assert json.load(f)["status"] == "succeeded"
PY
status="$(curl --silent --output "$tmp_dir/async-put-retry.json" --write-out '%{http_code}' \
    --request PUT --form "image=@$image_file" --header 'Authorization: Bearer storage-usage-smoke-key' \
    --header 'Idempotency-Key: minio-async-put-1' "$async_route")"
[[ "$status" == 202 ]] || fail "MinIO async PUT idempotent retry returned HTTP $status"
python3 - "$tmp_dir/async-put-retry.json" "$async_put_job_id" <<'PY' || fail "MinIO idempotent retry did not return the completed job"
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    value = json.load(f)
assert value["job_id"] == sys.argv[2] and value["status"] == "succeeded", value
PY
echo "PASS: MinIO durable async image POST/PUT, status, conflicts, and idempotent retry"

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

# Simulate a legacy/direct-to-storage image that violates the namespace profile.
# Inventory verification should detect and automatically normalize it to WebP.
curl --fail --silent --show-error \
    --aws-sigv4 "aws:amz:us-east-1:s3" \
    --user "$access_key:$secret_key" \
    --upload-file "$image_file" \
    "$minio_url/$bucket/maintenance-smoke/1/01.webp" >/dev/null

usage_route="$service_url/api/storage/usage"
status="$(curl --silent --output "$tmp_dir/usage-unauthorized.json" --write-out '%{http_code}' "$usage_route")"
[[ "$status" == 401 ]] || fail "MinIO storage usage endpoint returned HTTP $status without its independent API key"
status="$(curl --silent --output "$tmp_dir/reconcile.json" --write-out '%{http_code}' \
    --request POST --header 'Authorization: Bearer storage-usage-smoke-key' \
    "$service_url/api/storage/reconcile")"
[[ "$status" == 202 ]] || fail "MinIO storage reconciliation returned HTTP $status"
for _ in $(seq 1 100); do
    status="$(curl --silent --dump-header "$tmp_dir/usage.headers" --output "$tmp_dir/usage.json" --write-out '%{http_code}' \
        --header 'Authorization: Bearer storage-usage-smoke-key' "$usage_route?namespace=minio-smoke&resource_id=resource-delete-extra&slot=1")"
    [[ "$status" == 200 ]] || fail "MinIO storage usage query returned HTTP $status"
    if python3 - "$tmp_dir/usage.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    payload = json.load(f)
sys.exit(0 if payload["scan_status"] == "complete" and payload["object_count"] == 1 and payload["total_bytes"] > 0 else 1)
PY
    then
        break
    fi
    sleep 0.2
done
grep -qi '^Cache-Control: no-store' "$tmp_dir/usage.headers" || fail "storage usage response did not disable caching"
python3 - "$tmp_dir/usage.json" <<'PY' || fail "MinIO inventory did not include the stored slot"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    payload = json.load(f)
assert payload["scope"] == {"namespace": "minio-smoke", "resource_id": "resource-delete-extra", "slot": 1}, payload
assert payload["object_count"] == 1 and payload["total_bytes"] > 0, payload
assert payload["stale"] is False, payload
PY

maintenance_info_route="$service_url/api/images/maintenance-smoke/1/1/info"
maintenance_complete=false
for _ in $(seq 1 150); do
    status="$(curl --silent --output "$tmp_dir/maintenance-info.json" --write-out '%{http_code}' \
        --header 'Authorization: Bearer storage-usage-smoke-key' "$maintenance_info_route")"
    if [[ "$status" == 200 ]] && python3 - "$tmp_dir/maintenance-info.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    value = json.load(f)
sys.exit(0 if value["format"] == "webp" and value["width"] == 64 and value["height"] == 64 else 1)
PY
    then
        maintenance_complete=true
        break
    fi
    sleep 0.2
done
[[ "$maintenance_complete" == true ]] || fail "automatic MinIO maintenance did not reprocess the legacy image under its namespace profile"

# Service writes should adjust an already-completed usage snapshot without
# waiting for the next full reconciliation.
live_route="$service_url/api/images/minio-smoke/resource-delete-extra/1"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' --request DELETE \
    --header 'Authorization: Bearer storage-usage-smoke-key' "$live_route")"
[[ "$status" == 204 ]] || fail "MinIO catalog freshness DELETE returned HTTP $status"
status="$(curl --silent --output "$tmp_dir/usage-after-delete.json" --write-out '%{http_code}' \
    --header 'Authorization: Bearer storage-usage-smoke-key' \
    "$usage_route?namespace=minio-smoke&resource_id=resource-delete-extra")"
[[ "$status" == 200 ]] || fail "MinIO usage query after DELETE returned HTTP $status"
python3 - "$tmp_dir/usage-after-delete.json" <<'PY' || fail "MinIO DELETE did not immediately update usage totals"
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    value = json.load(f)
assert value["object_count"] == 0 and value["total_bytes"] == 0, value
PY
status="$(curl --silent --output "$tmp_dir/live-upload.json" --write-out '%{http_code}' \
    --request POST --header 'Authorization: Bearer storage-usage-smoke-key' \
    --form "image=@$image_file" "$live_route")"
[[ "$status" == 201 ]] || fail "MinIO catalog freshness POST returned HTTP $status"
status="$(curl --silent --output "$tmp_dir/usage-after-post.json" --write-out '%{http_code}' \
    --header 'Authorization: Bearer storage-usage-smoke-key' \
    "$usage_route?namespace=minio-smoke&resource_id=resource-delete-extra")"
[[ "$status" == 200 ]] || fail "MinIO usage query after POST returned HTTP $status"
python3 - "$tmp_dir/usage-after-post.json" <<'PY' || fail "MinIO POST did not immediately update usage totals"
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    value = json.load(f)
assert value["object_count"] == 1 and value["total_bytes"] > 0, value
PY

audits_route="$service_url/api/storage/audits"
status="$(curl --silent --output "$tmp_dir/audit-unauthorized.json" --write-out '%{http_code}' \
    --header 'Content-Type: application/json' --data '{}' "$audits_route")"
[[ "$status" == 401 ]] || fail "MinIO policy audit endpoint returned HTTP $status without its independent API key"
status="$(curl --silent --output "$tmp_dir/audit-created.json" --write-out '%{http_code}' \
    --header 'Authorization: Bearer storage-usage-smoke-key' --header 'Content-Type: application/json' \
    --data '{"namespace":"minio-smoke"}' "$audits_route")"
[[ "$status" == 202 ]] || fail "MinIO policy audit creation returned HTTP $status: $(cat "$tmp_dir/audit-created.json")"
audit_id="$(python3 - "$tmp_dir/audit-created.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    print(json.load(f)["audit_id"])
PY
)"
for _ in $(seq 1 100); do
    status="$(curl --silent --dump-header "$tmp_dir/audit.headers" --output "$tmp_dir/audit.json" --write-out '%{http_code}' \
        --header 'Authorization: Bearer storage-usage-smoke-key' "$service_url/api/storage/audits/$audit_id")"
    [[ "$status" == 200 ]] || fail "MinIO policy audit status returned HTTP $status"
    if python3 - "$tmp_dir/audit.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    sys.exit(0 if json.load(f)["status"] == "complete" else 1)
PY
    then
        break
    fi
    sleep 0.2
done
grep -qi '^Cache-Control: no-store' "$tmp_dir/audit.headers" || fail "policy audit response did not disable caching"
status="$(curl --silent --output "$tmp_dir/audit-findings.json" --write-out '%{http_code}' \
    --header 'Authorization: Bearer storage-usage-smoke-key' \
    "$service_url/api/storage/audits/$audit_id/findings?limit=50")"
[[ "$status" == 200 ]] || fail "MinIO policy audit findings returned HTTP $status"
python3 - "$tmp_dir/audit.json" "$tmp_dir/audit-findings.json" <<'PY' || fail "policy audit did not inspect and report the externally changed invalid fixture"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    summary = json.load(f)
with open(sys.argv[2], encoding="utf-8") as f:
    findings = json.load(f)["findings"]
assert summary["inventory_scan_id"], summary
assert summary["inspected"] >= 1, summary
assert any(item["code"] in ("invalid_or_unsupported_image", "inspection_error") for item in findings), findings
PY

echo "PASS: MinIO uploads, replacement, snapshot-consistent GETs during external writes, stale conditional validators, HEAD, metadata, listing, profiles, presigned GET, DELETE races, cancellation, resource DELETE, journaled live usage updates, automatic policy verification/maintenance, storage reconciliation, and durable policy audit"
echo "Both containers used ephemeral storage; all test data was removed during cleanup."
