#!/usr/bin/env bash
set -Eeuo pipefail

# Prepare a native Linux/amd64 host for the local Image Service benchmark.
# Results stay under bench/results, which is ignored by Git.

repo_url="${REPO_URL:-https://github.com/agusquartz/image_service.git}"
branch="${BRANCH:-develop}"
work_dir="${WORK_DIR:-${HOME}/image_service-benchmark}"
fixture_path="${FIXTURE_PATH:-}"
builder_name="${BUILDER_NAME:-image-service-benchmark}"

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

for command in git docker python3 curl; do
    command -v "$command" >/dev/null 2>&1 || die "Required command not found: $command"
done

[[ "$(uname -s)" == "Linux" ]] || die "Run this script on the Linux benchmark host."
[[ "$(uname -m)" == "x86_64" ]] || die "Expected native x86_64/amd64; found $(uname -m)."

if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    case "${ID:-}" in
        ubuntu|debian) ;;
        *) die "This script currently supports Ubuntu/Debian hosts; found ${ID:-unknown}." ;;
    esac
else
    die "Cannot identify the Linux distribution from /etc/os-release."
fi

python_version="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
python_major="${python_version%%.*}"
python_minor="${python_version#*.}"
(( python_major > 3 || (python_major == 3 && python_minor >= 10) )) \
    || die "Python 3.10 or newer is required; found $python_version."

docker info >/dev/null 2>&1 || die "Docker is installed but the daemon is unavailable to this user."
docker buildx version >/dev/null 2>&1 || die "Docker Buildx is required."

if [[ -e "$work_dir" && ! -d "$work_dir/.git" ]]; then
    die "Work directory exists but is not a Git checkout: $work_dir"
fi

if [[ -d "$work_dir/.git" ]]; then
    [[ -z "$(git -C "$work_dir" status --porcelain)" ]] \
        || die "Existing checkout is not clean; preserve it or choose another WORK_DIR."
    git -C "$work_dir" fetch --depth 1 origin "$branch"
    git -C "$work_dir" checkout -B "$branch" FETCH_HEAD
else
    git clone --depth 1 --single-branch --branch "$branch" "$repo_url" "$work_dir"
fi

fixture_target="$work_dir/TEST_IMAGE_ORIGINAL.png"
if [[ -n "$fixture_path" ]]; then
    [[ -f "$fixture_path" ]] || die "Fixture file not found: $fixture_path"
    source_path="$(cd "$(dirname "$fixture_path")" && pwd)/$(basename "$fixture_path")"
    if [[ "$source_path" != "$fixture_target" ]]; then
        install -m 0644 "$fixture_path" "$fixture_target"
    fi
fi
[[ -f "$fixture_target" ]] || die "Missing TEST_IMAGE_ORIGINAL.png. Copy the fixture to $fixture_target or rerun with FIXTURE_PATH=/path/to/image."

if ! docker buildx inspect "$builder_name" >/dev/null 2>&1; then
    docker buildx create --name "$builder_name" --driver docker-container --use >/dev/null
else
    docker buildx use "$builder_name"
fi
docker buildx inspect --bootstrap "$builder_name" >/dev/null

printf 'Pulling benchmark images for native linux/amd64...\n'
docker pull --platform linux/amd64 rust:1.88-bookworm
docker pull --platform linux/amd64 gcr.io/distroless/cc-debian12:nonroot
docker pull --platform linux/amd64 grafana/k6:latest
docker pull --platform linux/amd64 pgsty/minio:latest

printf 'Warming the release build and BuildKit dependency cache...\n'
docker buildx build --platform linux/amd64 --target runtime \
    --tag image-service:bench-prefetch-amd64 --load "$work_dir"

printf '\nPrepared checkout: %s\n' "$work_dir"
printf 'Branch: %s\n' "$(git -C "$work_dir" branch --show-current)"
printf 'Commit: %s\n' "$(git -C "$work_dir" rev-parse --short HEAD)"
printf 'Host: %s / %s\n' "$(uname -m)" "$python_version"
printf 'Fixture: %s (%s bytes)\n' "$fixture_target" "$(wc -c <"$fixture_target" | tr -d ' ')"
printf '\nBenchmark results stay local under bench/results/.\n'
printf 'The benchmark harness is available on the selected branch.\n'
