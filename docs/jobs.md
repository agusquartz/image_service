# Background Jobs

This guide describes the durable, internal job scheduler currently used by
Image Service. It is intended for contributors and operators who need to know
what work can run in the background, what starts it, and how it behaves during
traffic, errors, and shutdown.

## Current scope

The scheduler is a generic crate (`crates/job-scheduler`) with a SQLite/WAL
catalog and an in-process dispatcher. The catalog is the source of truth; the
dispatcher claims only a bounded number of rows and stores state transitions,
progress, results, errors, and events in SQLite. It is a single-Core-instance
design: do not share the SQLite database between service replicas or put its
WAL files on NFS/SMB.

The scheduler currently runs **internal system jobs only**. It has no public
generic job API, no endpoint for inspecting or cancelling arbitrary jobs, and
no remote worker execution. The `worker-protocol` feature contains DTOs only.
The storage usage API is the only current public way to request a background
job, and it exposes a narrow operation rather than the scheduler itself.

## Registered work

| Job kind | Queue | How it is created | What it does | Current state |
| --- | --- | --- | --- | --- |
| `storage.inventory.reconcile` | `storage.inventory` | Periodic schedule when `IMAGE_STORAGE_USAGE_ENABLED=true`; also by `POST /api/storage/reconcile` | Streams the storage listing, indexes canonical image-slot WebP metadata in batches of 250, then promotes a complete catalog snapshot | Implemented |

The inventory job reads object keys and metadata. It does not read or decode
image bodies and it does not repair or rewrite objects. A failed or interrupted
scan leaves the last successfully completed snapshot available. See the
[storage usage API](api.md#storage-usage-reporting) for its query and response
semantics.

Periodic schedule details:

- Schedule ID: `storage-inventory-periodic`.
- On a new installation, the first run is due immediately after registration.
  Its execution still waits for the dispatcher's readiness, quiet-period, and
  optional UTC-window gates. On later starts, the persisted next-run time is
  retained rather than reset to immediate.
- Later runs are due at `IMAGE_STORAGE_INVENTORY_INTERVAL_SECONDS` intervals
  (default `3600`, valid range `60..=2678400`).
- Scheduled jobs have priority `-10` and allow up to five attempts.
- If a run from this schedule is pending, running, or waiting to retry when
  the next interval becomes due, the scheduler does not create a duplicate.
  The next due time is advanced; it does not build an unbounded backlog of
  missed intervals.

Manual requests use the same handler, with priority `20` and an active-job
deduplication key. Repeated manual requests while the job is pending, running,
or retrying return the existing job ID instead of queuing duplicate scans.

## When a job can start

The dispatcher checks these gates before claiming every job:

1. The service has completed startup and is ready.
2. There are no active API requests, and the configured quiet period has
   elapsed (`IMAGE_BACKGROUND_IDLE_SECONDS`, default `30`).
3. If `IMAGE_BACKGROUND_WINDOW_UTC` is set, the current time is inside that
   UTC window.
4. A registered handler exists for the job kind, the job is due, its
   dependencies have succeeded, and a worker slot is available.

The dispatcher polls at a one-second interval. `IMAGE_BACKGROUND_JOB_CONCURRENCY`
sets the maximum number of simultaneously claimed jobs (default `1`, range
`1..=64`). This is a bound on concurrent jobs, not on the number of persisted
jobs.

Gates apply before a job is claimed; they do not preempt a running job. The
inventory handler also checkpoints every 250 indexed objects and waits for the
quiet-period condition again before processing another batch. A UTC window
closing does not stop an already running handler. Other handlers must make
their work units bounded and honor checkpoints/cancellation if they need
similar yielding behavior.

`IMAGE_BACKGROUND_IDLE_SECONDS=0` removes the delay since last activity, but
the dispatcher still requires zero active API requests. An optional UTC
window is an additional condition, not a replacement for idleness.

## Adding another system job

Contributors should register a stable handler kind and, if recurring, a stable
schedule ID before starting the dispatcher. Treat both identifiers and the
persisted payload shape as durable data: when a payload's meaning changes in
an incompatible way, introduce a versioned kind or provide an explicit data
migration. A handler should validate its payload, make external side effects
idempotent, process bounded units, checkpoint useful progress, and honor
cancellation at safe boundaries. Set a retry policy only for errors that can
reasonably succeed later, and avoid storing secrets or large blobs in SQLite.

Register handlers only when their required feature and dependencies are
available. A persisted job whose handler is not registered is not claimed; it
remains in SQLite until a compatible handler is registered again. For example,
the image inventory handler is registered only when image support and
`IMAGE_STORAGE_USAGE_ENABLED=true` are active.

## Job lifecycle and recovery

Jobs are persisted as `pending`, `running`, `retry_wait`, `succeeded`,
`failed`, or `cancelled`. Claiming a job increments its attempt count and
assigns a lease. The default lease is 60 seconds and is refreshed while the
handler runs. If the process exits or loses the lease, the job becomes
claimable after the lease expires. A handler that cannot checkpoint after
losing its lease must discard its late result.

Handlers return success or failure, and failures may be marked retryable.
Retryable failures are scheduled with exponential backoff, beginning at 1
second and capped at 1 hour, until `max_attempts` is reached. The inventory
handler allows five attempts. Exhausted or non-retryable failures become
terminal `failed` jobs. A dependent job is not claimable until all parents
succeed; it becomes cancelled if a dependency fails or is cancelled.

The scheduler API supports cancellation internally, but Image Service does
not currently expose a public cancellation route. Pending/retrying work can be
cancelled immediately by an internal caller. For running work, cancellation
is signalled to its handler; the handler must check it and stop at a safe
boundary. The inventory handler checks between listing entries and batches.

On SIGINT/SIGTERM, the service stops claiming jobs and drains active handlers
for up to `IMAGE_SHUTDOWN_TIMEOUT_SECONDS`. If that deadline expires, the
dispatcher is aborted; the active job's lease remains in SQLite and permits
recovery after expiry on the next startup. This is at-least-once execution:
handlers must tolerate a retry after a crash and should make side effects
idempotent or checkpoint their progress safely.

## Retention and persistence

Terminal job records and event history become eligible for pruning after
`IMAGE_JOB_RETENTION_DAYS` (default `30`, valid range `1..=3650`). The
dispatcher checks for pruning at most once per hour, while it is allowed to
claim work (ready, idle/window gates open, and at least one handler
registered). Jobs still referenced by dependent jobs are retained. Pruning is
scheduler housekeeping, not a separately queued job. Queued and active jobs
are not removed by this retention policy.

`IMAGE_JOB_DB_PATH` selects the durable job database. Keep its database,
`-wal`, and `-shm` files on persistent local storage. Storage inventory has a
separate catalog database selected by `IMAGE_STORAGE_CATALOG_DB_PATH`; both
paths should be covered by the same persistent container volume when using
the default Docker layout. A temporary filesystem intentionally loses queued
work and inventory state when the container is removed.

## Current limits and future work

- Jobs are dispatched in-process by one service instance; there is no external
  worker transport or cross-instance coordination.
- Persisted jobs whose handler is not registered remain unclaimed; disabling a
  feature does not delete their rows.
- There is no public job listing, job detail, or cancellation API.
- Storage policy audits, repair/recompression, inventory of other processor
  types, and configurable per-queue scheduling policies are not implemented.
- The scheduler persists jobs and periodic schedule definitions, but currently
  only the storage inventory handler is registered by the application.

These boundaries are intentional for the current release. New system jobs
should document their trigger, queue, idempotency, checkpoints, retry policy,
load impact, and shutdown behavior before registration.

## Code map

- `crates/job-scheduler/src/lib.rs`: SQLite schema, job and schedule APIs,
  leases, retries, dispatcher gates, cancellation, and runtime shutdown.
- `crates/file-core/src/lib.rs`: scheduler initialization, handler
  registration, startup, and graceful drain.
- `crates/file-core/src/state.rs`: scheduler configuration and shared
  activity/readiness state.
- `crates/file-core/src/storage_usage.rs`: inventory schedule, manual trigger,
  batching, quiet-period yielding, and snapshot promotion.
- [Configuration](configuration.md#durable-internal-job-scheduler): runtime
  environment variables and defaults.
