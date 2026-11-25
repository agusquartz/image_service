//! Optional Tus 1.0 upload sessions.
//!
//! SQLite stores session metadata, checksums, and confirmed offsets. Chunk bytes
//! are durable OpenDAL staging objects; the offset advances only after a chunk
//! writer has closed successfully and its manifest row commits.

use crate::{
    auth::require_auth,
    error::ApiError,
    images::{ImageKey, IncomingImage, service},
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    path::Path as FsPath,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const TUS_VERSION: &str = "1.0.0";
const TUS_VERSION_HEADER: HeaderName = HeaderName::from_static("tus-version");
const TUS_RESUMABLE: HeaderName = HeaderName::from_static("tus-resumable");
const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
const UPLOAD_LENGTH: HeaderName = HeaderName::from_static("upload-length");
const UPLOAD_EXPIRES: HeaderName = HeaderName::from_static("upload-expires");
const UPLOAD_STATUS: HeaderName = HeaderName::from_static("upload-status");
const UPLOAD_CHECKSUM: HeaderName = HeaderName::from_static("upload-checksum");
const TUS_EXTENSION: HeaderName = HeaderName::from_static("tus-extension");
const TUS_CHECKSUM_ALGORITHM: HeaderName = HeaderName::from_static("tus-checksum-algorithm");

#[derive(Clone)]
struct Session {
    id: String,
    namespace: String,
    resource_id: String,
    slot: u8,
    replace_existing: bool,
    length: u64,
    offset: u64,
    expires_at: u64,
    status: String,
}

struct SessionChunk {
    offset: u64,
    length: usize,
    chunk_id: Option<String>,
    checksum: Option<Vec<u8>>,
    legacy_bytes: Option<Vec<u8>>,
}

pub(crate) async fn create_post_session(
    state: State<AppState>,
    path: Path<(String, String, u8)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    create_session(state, path, headers, false).await
}

pub(crate) async fn tus_options(State(state): State<AppState>) -> Response {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = if state.resumable_uploads_enabled {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    };
    if state.resumable_uploads_enabled {
        response.headers_mut().insert(
            TUS_VERSION_HEADER.clone(),
            HeaderValue::from_static(TUS_VERSION),
        );
        response.headers_mut().insert(
            TUS_EXTENSION.clone(),
            HeaderValue::from_static("creation,expiration,checksum"),
        );
        response.headers_mut().insert(
            TUS_CHECKSUM_ALGORITHM.clone(),
            HeaderValue::from_static("sha256"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

pub(crate) async fn create_put_session(
    state: State<AppState>,
    path: Path<(String, String, u8)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    create_session(state, path, headers, true).await
}

async fn create_session(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
    replace_existing: bool,
) -> Result<Response, ApiError> {
    ensure_enabled(&state)?;
    require_tus_version(&headers)?;
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    let key = ImageKey::new(namespace, resource_id, slot)?;
    let policy = state.processing_policy_for(&key.namespace);
    let length = parse_u64_header(&headers, &UPLOAD_LENGTH, "Upload-Length")?;
    if length == 0 {
        return Err(ApiError::BadRequest(
            "Upload-Length must be greater than zero".into(),
        ));
    }
    if length > policy.max_input_bytes as u64 {
        return Err(ApiError::PayloadTooLarge(format!(
            "Upload-Length exceeds the configured input limit of {} bytes",
            policy.max_input_bytes
        )));
    }
    if slot > state.max_slots {
        if !replace_existing {
            return Err(ApiError::BadRequest(format!(
                "cannot create slot {slot}: IMAGE_MAX_SLOTS is {}",
                state.max_slots
            )));
        }
        state.store.stat(&key).await?;
    }
    if replace_existing {
        // PUT sessions may create or replace, matching the synchronous API.
    } else {
        state.store.ensure_absent(&key).await?;
    }

    let id = Uuid::new_v4().to_string();
    cleanup_expired_sessions(&state).await?;
    let expires_at = now_seconds().saturating_add(state.resumable_upload_ttl_seconds);
    let db_path = state.resumable_upload_db_path.clone();
    let session = Session {
        id: id.clone(),
        namespace: key.namespace,
        resource_id: key.resource_id,
        slot,
        replace_existing,
        length,
        offset: 0,
        expires_at,
        status: "uploading".into(),
    };
    blocking_db(move || {
        let connection = open_db(&db_path)?;
        connection.execute(
            "INSERT INTO resumable_upload_sessions
                (id, namespace, resource_id, slot, replace_existing, upload_length, upload_offset, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![session.id, session.namespace, session.resource_id, session.slot, session.replace_existing, session.length, session.expires_at],
        ).map_err(db_error)?;
        Ok(())
    }).await?;

    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::CREATED;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let location = format!("/api/image-uploads/{id}");
    response
        .headers_mut()
        .insert(header::LOCATION, header_value(&location)?);
    set_tus_headers(response.headers_mut(), 0, length, expires_at, "uploading")?;
    Ok(response)
}

pub(crate) async fn head_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    ensure_enabled(&state)?;
    require_tus_version(&headers)?;
    require_auth(&headers, state.read_auth, state.api_key.as_deref())?;
    let session = load_session(&state, id).await?;
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    set_tus_headers(
        response.headers_mut(),
        session.offset,
        session.length,
        session.expires_at,
        &session.status,
    )?;
    Ok(response)
}

pub(crate) async fn patch_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let headers = request.headers().clone();
    ensure_enabled(&state)?;
    require_tus_version(&headers)?;
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if content_type != Some("application/offset+octet-stream") {
        return Err(ApiError::UnsupportedMediaType(
            "Tus PATCH requires Content-Type: application/offset+octet-stream".into(),
        ));
    }
    let requested_offset = parse_u64_header(&headers, &UPLOAD_OFFSET, "Upload-Offset")?;
    let session = load_session(&state, id.clone()).await?;
    if session.status == "completed" {
        if requested_offset == session.offset {
            return patch_response(session.offset, "completed");
        }
        return Err(ApiError::Conflict(
            "Upload session is already completed".into(),
        ));
    }
    if session.status == "finalizing" {
        return Err(ApiError::Conflict(
            "Upload session is already being finalized".into(),
        ));
    }
    if session.offset != requested_offset {
        return Err(ApiError::Conflict(format!(
            "Upload-Offset mismatch; current offset is {}",
            session.offset
        )));
    }
    let policy = state.processing_policy_for(&session.namespace);
    let admission = state.admit_transfer_upload(policy.max_input_bytes)?;
    let patch_limit = policy
        .max_input_bytes
        .min(state.resumable_upload_max_chunk_bytes) as u64;
    let checksum = headers
        .get(&UPLOAD_CHECKSUM)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| ApiError::BadRequest("Upload-Checksum is not valid text".into()))
        })
        .transpose()?;
    let chunk_id = Uuid::new_v4().to_string();
    let cleanup_chunk_id = chunk_id.clone();
    let mut writer = state
        .store
        .open_resumable_chunk_writer(&id, &chunk_id)
        .await?;
    let mut body_stream = request.into_body().into_data_stream();
    let receive_result = tokio::time::timeout(
        std::time::Duration::from_secs(state.upload_read_timeout_seconds),
        async {
            let mut patch_length = 0u64;
            let mut digest = Sha256::new();
            while let Some(frame) = body_stream.next().await {
                let frame = frame.map_err(|error| {
                    ApiError::BadRequest(format!("Could not read upload chunk: {error}"))
                })?;
                let next_size = patch_length.saturating_add(frame.len() as u64);
                if next_size > patch_limit
                    || next_size > session.length.saturating_sub(session.offset)
                {
                    return Err(ApiError::PayloadTooLarge(format!(
                        "PATCH exceeds the allowed chunk size of {patch_limit} bytes or the remaining upload length"
                    )));
                }
                digest.update(&frame);
                writer
                    .write(frame)
                    .await
                    .map_err(|error| ApiError::Storage(error.to_string()))?;
                patch_length = next_size;
            }
            Ok::<_, ApiError>((patch_length, digest.finalize().to_vec()))
        },
    )
    .await;
    let (patch_length, actual_checksum) = match receive_result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let _ = writer.abort().await;
            let _ = state
                .store
                .delete_resumable_chunk(&id, &cleanup_chunk_id)
                .await;
            return Err(error);
        }
        Err(_) => {
            let _ = writer.abort().await;
            let _ = state
                .store
                .delete_resumable_chunk(&id, &cleanup_chunk_id)
                .await;
            return Err(ApiError::RequestTimeout(
                "Tus PATCH body read timed out".into(),
            ));
        }
    };
    if let Some(checksum) = checksum.as_deref()
        && let Err(error) = verify_checksum_digest(checksum, &actual_checksum)
    {
        let _ = writer.abort().await;
        let _ = state
            .store
            .delete_resumable_chunk(&id, &cleanup_chunk_id)
            .await;
        return Err(error);
    }
    if let Err(error) = writer.close().await {
        let _ = state
            .store
            .delete_resumable_chunk(&id, &cleanup_chunk_id)
            .await;
        return Err(ApiError::Storage(error.to_string()));
    }

    let session_id = id.clone();
    let db_path = state.resumable_upload_db_path.clone();
    let transaction_result = blocking_db(move || {
        let mut connection = open_db(&db_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db_error)?;
        let (offset, length, expires_at): (u64, u64, u64) = transaction.query_row(
            "SELECT upload_offset, upload_length, expires_at FROM resumable_upload_sessions WHERE id=?1",
            [&session_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(db_error)?.ok_or_else(|| ApiError::NotFound("Upload session not found".into()))?;
        if expires_at <= now_seconds() {
            transaction.execute("DELETE FROM resumable_upload_sessions WHERE id=?1", [&session_id]).map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
            return Err(ApiError::NotFound("Upload session expired".into()));
        }
        if offset != requested_offset {
            return Err(ApiError::Conflict(format!("Upload-Offset mismatch; current offset is {offset}")));
        }
        let next_offset = offset.checked_add(patch_length)
            .ok_or_else(|| ApiError::PayloadTooLarge("Upload offset overflow".into()))?;
        if next_offset > length {
            return Err(ApiError::PayloadTooLarge("PATCH exceeds the declared Upload-Length".into()));
        }
        if patch_length > 0 {
            transaction.execute(
                "INSERT INTO resumable_upload_staged_chunks
                    (session_id, chunk_offset, chunk_id, chunk_length, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, offset, chunk_id, patch_length, actual_checksum],
            ).map_err(db_error)?;
            transaction.execute(
                "UPDATE resumable_upload_sessions
                 SET upload_offset=?2, state=CASE WHEN ?2=upload_length THEN 'ready' ELSE 'uploading' END
                 WHERE id=?1",
                params![session_id, next_offset],
            ).map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        Ok(next_offset)
    }).await;
    let new_offset = match transaction_result {
        Ok(offset) => offset,
        Err(error) => {
            if matches!(&error, ApiError::NotFound(_)) {
                let _ = state.store.delete_resumable_upload_staging(&id).await;
            } else {
                let _ = state
                    .store
                    .delete_resumable_chunk(&id, &cleanup_chunk_id)
                    .await;
            }
            return Err(error);
        }
    };
    if patch_length == 0 {
        state
            .store
            .delete_resumable_chunk(&id, &cleanup_chunk_id)
            .await?;
    }
    drop(admission);

    let session = load_session(&state, id.clone()).await?;
    if new_offset == session.length {
        finalize_session(&state, &session).await?;
    }
    patch_response(
        new_offset,
        if new_offset == session.length {
            "completed"
        } else {
            "uploading"
        },
    )
}

fn patch_response(offset: u64, status: &str) -> Result<Response, ApiError> {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(TUS_RESUMABLE.clone(), HeaderValue::from_static(TUS_VERSION));
    response
        .headers_mut()
        .insert(UPLOAD_OFFSET.clone(), header_value(&offset.to_string())?);
    response
        .headers_mut()
        .insert(UPLOAD_STATUS.clone(), header_value(status)?);
    Ok(response)
}

pub(crate) async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    ensure_enabled(&state)?;
    require_tus_version(&headers)?;
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    let cleanup_id = id.clone();
    let db_path = state.resumable_upload_db_path.clone();
    blocking_db(move || {
        let connection = open_db(&db_path)?;
        let deleted = connection
            .execute("DELETE FROM resumable_upload_sessions WHERE id=?1", [&id])
            .map_err(db_error)?;
        if deleted == 0 {
            return Err(ApiError::NotFound("Upload session not found".into()));
        }
        Ok(())
    })
    .await?;
    state
        .store
        .delete_resumable_upload_staging(&cleanup_id)
        .await?;
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(TUS_RESUMABLE.clone(), HeaderValue::from_static(TUS_VERSION));
    Ok(response)
}

pub(crate) async fn recover_staging(state: &AppState) -> Result<(), ApiError> {
    cleanup_expired_sessions(state).await?;
    let db_path = state.resumable_upload_db_path.clone();
    let registered = blocking_db(move || {
        let mut connection = open_db(&db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        transaction
            .execute(
                "UPDATE resumable_upload_sessions SET state='ready' WHERE state='finalizing'",
                [],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        let connection = connection;
        let mut statement = connection
            .prepare("SELECT session_id, chunk_id FROM resumable_upload_staged_chunks")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_error)?;
        let mut keys = std::collections::HashSet::new();
        for row in rows {
            let (session_id, chunk_id) = row.map_err(db_error)?;
            keys.insert(format!(
                "__image_transfer/resumable/{session_id}/chunks/{chunk_id}"
            ));
        }
        Ok(keys)
    })
    .await?;
    for key in state.store.list_resumable_staging_keys().await? {
        if !registered.contains(&key) {
            state.store.delete_resumable_staging_key(&key).await?;
        }
    }
    Ok(())
}

async fn cleanup_expired_sessions(state: &AppState) -> Result<(), ApiError> {
    let db_path = state.resumable_upload_db_path.clone();
    let expired = blocking_db(move || {
        let mut connection = open_db(&db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let ids = {
            let mut statement = transaction
                .prepare("SELECT id FROM resumable_upload_sessions WHERE expires_at <= ?1")
                .map_err(db_error)?;
            let rows = statement
                .query_map([now_seconds()], |row| row.get::<_, String>(0))
                .map_err(db_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?
        };
        transaction
            .execute(
                "DELETE FROM resumable_upload_sessions WHERE expires_at <= ?1",
                [now_seconds()],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(ids)
    })
    .await?;
    for id in expired {
        state.store.delete_resumable_upload_staging(&id).await?;
    }
    Ok(())
}

async fn finalize_session(state: &AppState, session: &Session) -> Result<(), ApiError> {
    match session.status.as_str() {
        "completed" => return Ok(()),
        "uploading" => {
            return Err(ApiError::Conflict(
                "Upload session has not received all declared bytes".into(),
            ));
        }
        "finalizing" => {
            return Err(ApiError::Conflict(
                "Upload session is already being finalized".into(),
            ));
        }
        "ready" => {}
        _ => return Err(ApiError::Internal("Unknown upload session state".into())),
    }
    let claim_id = session.id.clone();
    let db_path = state.resumable_upload_db_path.clone();
    blocking_db(move || {
        let connection = open_db(&db_path)?;
        let updated = connection
            .execute(
                "UPDATE resumable_upload_sessions SET state='finalizing'
                 WHERE id=?1 AND state='ready'",
                [&claim_id],
            )
            .map_err(db_error)?;
        if updated != 1 {
            return Err(ApiError::Conflict(
                "Upload session is already being finalized".into(),
            ));
        }
        Ok(())
    })
    .await?;

    let outcome = async {
        let key = ImageKey::new(
            session.namespace.clone(),
            session.resource_id.clone(),
            session.slot,
        )?;
        let policy = state.processing_policy_for(&key.namespace);
        let mut admission = state.admit_transfer_upload(policy.max_input_bytes)?;
        let source_capacity = usize::try_from(session.length).map_err(|_| {
            ApiError::PayloadTooLarge("Upload length is too large for this host".into())
        })?;
        state.reserve_staged_source(&mut admission, source_capacity)?;

        let id = session.id.clone();
        let db_path = state.resumable_upload_db_path.clone();
        let mut chunks = blocking_db(move || {
            let connection = open_db(&db_path)?;
            let mut statement = connection.prepare(
            "SELECT chunk_offset, chunk_bytes FROM resumable_upload_chunks WHERE session_id=?1"
        ).map_err(db_error)?;
            let mut rows = statement.query([id.as_str()]).map_err(db_error)?;
            let mut chunks = Vec::new();
            while let Some(row) = rows.next().map_err(db_error)? {
                let offset = row.get::<_, u64>(0).map_err(db_error)?;
                let chunk = row.get::<_, Vec<u8>>(1).map_err(db_error)?;
                chunks.push(SessionChunk {
                    offset,
                    length: chunk.len(),
                    chunk_id: None,
                    checksum: None,
                    legacy_bytes: Some(chunk),
                });
            }
            let mut statement = connection
                .prepare(
                    "SELECT chunk_offset, chunk_id, chunk_length, sha256
             FROM resumable_upload_staged_chunks WHERE session_id=?1",
                )
                .map_err(db_error)?;
            let mut rows = statement.query([id.as_str()]).map_err(db_error)?;
            while let Some(row) = rows.next().map_err(db_error)? {
                chunks.push(SessionChunk {
                    offset: row.get(0).map_err(db_error)?,
                    chunk_id: Some(row.get(1).map_err(db_error)?),
                    length: usize::try_from(row.get::<_, u64>(2).map_err(db_error)?)
                        .map_err(|_| ApiError::Internal("Stored chunk length overflow".into()))?,
                    checksum: Some(row.get(3).map_err(db_error)?),
                    legacy_bytes: None,
                });
            }
            chunks.sort_by_key(|chunk| chunk.offset);
            Ok(chunks)
        })
        .await?;
        let mut source = Vec::with_capacity(source_capacity);
        for chunk in chunks.drain(..) {
            if chunk.offset != source.len() as u64 {
                return Err(ApiError::ServiceUnavailable(
                    "Upload session has a gap in its confirmed chunks".into(),
                ));
            }
            let chunk_bytes = match (chunk.chunk_id, chunk.legacy_bytes) {
                (Some(chunk_id), None) => {
                    let bytes = state
                        .store
                        .read_resumable_chunk(&session.id, &chunk_id)
                        .await?;
                    let actual_checksum = Sha256::digest(&bytes);
                    if bytes.len() != chunk.length
                        || chunk
                            .checksum
                            .as_deref()
                            .is_none_or(|expected| actual_checksum[..] != expected[..])
                    {
                        return Err(ApiError::ServiceUnavailable(
                            "A confirmed upload chunk failed its length or checksum check".into(),
                        ));
                    }
                    bytes
                }
                (None, Some(bytes)) => Bytes::from(bytes),
                _ => {
                    return Err(ApiError::Internal(
                        "Invalid resumable upload chunk manifest".into(),
                    ));
                }
            };
            source.extend_from_slice(&chunk_bytes);
        }
        if source.len() as u64 != session.length {
            return Err(ApiError::ServiceUnavailable(
                "Upload session is not fully durable".into(),
            ));
        }
        let operation = state.store.begin_operation(&key).await;
        service::upload(
            state,
            &key,
            IncomingImage {
                bytes: Bytes::from(source),
                mime: None,
            },
            session.replace_existing,
            admission,
            operation,
        )
        .await?;
        let id = session.id.clone();
        let db_path = state.resumable_upload_db_path.clone();
        blocking_db(move || {
            let connection = open_db(&db_path)?;
            connection
                .execute(
                    "UPDATE resumable_upload_sessions SET state='completed' WHERE id=?1",
                    [id.as_str()],
                )
                .map_err(db_error)?;
            connection
                .execute(
                    "DELETE FROM resumable_upload_staged_chunks WHERE session_id=?1",
                    [id.as_str()],
                )
                .map_err(db_error)?;
            connection
                .execute(
                    "DELETE FROM resumable_upload_chunks WHERE session_id=?1",
                    [id.as_str()],
                )
                .map_err(db_error)?;
            Ok(())
        })
        .await?;
        state
            .store
            .delete_resumable_upload_staging(&session.id)
            .await
    }
    .await;
    if outcome.is_err() {
        let retry_id = session.id.clone();
        let db_path = state.resumable_upload_db_path.clone();
        let _ = blocking_db(move || {
            let connection = open_db(&db_path)?;
            connection
                .execute(
                    "UPDATE resumable_upload_sessions SET state='ready'
                     WHERE id=?1 AND state='finalizing'",
                    [&retry_id],
                )
                .map_err(db_error)?;
            Ok(())
        })
        .await;
    }
    outcome
}

async fn load_session(state: &AppState, id: String) -> Result<Session, ApiError> {
    let cleanup_id = id.clone();
    let db_path = state.resumable_upload_db_path.clone();
    let result = blocking_db(move || {
        let connection = open_db(&db_path)?;
        let session = connection.query_row(
            "SELECT id, namespace, resource_id, slot, replace_existing, upload_length, upload_offset, expires_at, state FROM resumable_upload_sessions WHERE id=?1",
            [&id], |row| Ok(Session {
                id: row.get(0)?, namespace: row.get(1)?, resource_id: row.get(2)?, slot: row.get(3)?,
                replace_existing: row.get(4)?, length: row.get(5)?, offset: row.get(6)?, expires_at: row.get(7)?, status: row.get(8)?,
            }),
        ).optional().map_err(db_error)?.ok_or_else(|| ApiError::NotFound("Upload session not found".into()))?;
        if session.expires_at <= now_seconds() {
            connection.execute("DELETE FROM resumable_upload_sessions WHERE id=?1", [&session.id]).map_err(db_error)?;
            return Err(ApiError::NotFound("Upload session expired".into()));
        }
        Ok(session)
    })
    .await;
    if matches!(&result, Err(ApiError::NotFound(_))) {
        state
            .store
            .delete_resumable_upload_staging(&cleanup_id)
            .await?;
    }
    result
}

async fn blocking_db<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| ApiError::Internal(format!("SQLite task failed: {error}")))?
}

fn open_db(path: &FsPath) -> Result<Connection, ApiError> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| ApiError::Internal(error.to_string()))?;
    }
    let connection = Connection::open(path).map_err(db_error)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(db_error)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(db_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(db_error)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS resumable_upload_sessions (
            id TEXT PRIMARY KEY, namespace TEXT NOT NULL, resource_id TEXT NOT NULL, slot INTEGER NOT NULL,
            replace_existing INTEGER NOT NULL, upload_length INTEGER NOT NULL, upload_offset INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            state TEXT NOT NULL DEFAULT 'uploading'
         );
         CREATE TABLE IF NOT EXISTS resumable_upload_chunks (
            session_id TEXT NOT NULL REFERENCES resumable_upload_sessions(id) ON DELETE CASCADE,
            chunk_offset INTEGER NOT NULL, chunk_bytes BLOB NOT NULL, PRIMARY KEY(session_id, chunk_offset)
         );
         CREATE TABLE IF NOT EXISTS resumable_upload_staged_chunks (
            session_id TEXT NOT NULL REFERENCES resumable_upload_sessions(id) ON DELETE CASCADE,
            chunk_offset INTEGER NOT NULL,
            chunk_id TEXT NOT NULL,
            chunk_length INTEGER NOT NULL,
            sha256 BLOB NOT NULL,
            PRIMARY KEY(session_id, chunk_offset),
            UNIQUE(session_id, chunk_id)
         );"
    ).map_err(db_error)?;
    let has_state = {
        let mut statement = connection
            .prepare("PRAGMA table_info(resumable_upload_sessions)")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?
            .iter()
            .any(|column| column == "state")
    };
    if !has_state {
        connection
            .execute(
                "ALTER TABLE resumable_upload_sessions ADD COLUMN state TEXT NOT NULL DEFAULT 'uploading'",
                [],
            )
            .map_err(db_error)?;
        connection
            .execute(
                "UPDATE resumable_upload_sessions SET state='ready' WHERE upload_offset=upload_length",
                [],
            )
            .map_err(db_error)?;
    }
    Ok(connection)
}

fn require_tus_version(headers: &HeaderMap) -> Result<(), ApiError> {
    match headers
        .get(&TUS_RESUMABLE)
        .and_then(|value| value.to_str().ok())
    {
        Some(TUS_VERSION) => Ok(()),
        _ => Err(ApiError::PreconditionFailed(
            "Tus-Resumable: 1.0.0 is required".into(),
        )),
    }
}

fn ensure_enabled(state: &AppState) -> Result<(), ApiError> {
    if state.resumable_uploads_enabled {
        Ok(())
    } else {
        Err(ApiError::NotFound("Resumable uploads are disabled".into()))
    }
}

fn parse_u64_header(headers: &HeaderMap, name: &HeaderName, label: &str) -> Result<u64, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "{label} header is required and must be an unsigned integer"
            ))
        })
}

fn verify_checksum_digest(value: &str, actual: &[u8]) -> Result<(), ApiError> {
    let (algorithm, encoded) = value.split_once(' ').ok_or_else(|| {
        ApiError::BadRequest("Upload-Checksum must contain an algorithm and base64 digest".into())
    })?;
    if algorithm != "sha256" {
        return Err(ApiError::BadRequest(
            "Only the Tus sha256 checksum algorithm is supported".into(),
        ));
    }
    let expected = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::BadRequest("Upload-Checksum is not valid base64".into()))?;
    if actual[..] != expected[..] {
        return Err(ApiError::ChecksumMismatch);
    }
    Ok(())
}

fn set_tus_headers(
    headers: &mut HeaderMap,
    offset: u64,
    length: u64,
    expires_at: u64,
    status: &str,
) -> Result<(), ApiError> {
    headers.insert(TUS_RESUMABLE.clone(), HeaderValue::from_static(TUS_VERSION));
    headers.insert(
        TUS_EXTENSION.clone(),
        HeaderValue::from_static("creation,expiration,checksum"),
    );
    headers.insert(
        TUS_CHECKSUM_ALGORITHM.clone(),
        HeaderValue::from_static("sha256"),
    );
    headers.insert(UPLOAD_OFFSET.clone(), header_value(&offset.to_string())?);
    headers.insert(UPLOAD_STATUS.clone(), header_value(status)?);
    headers.insert(UPLOAD_LENGTH.clone(), header_value(&length.to_string())?);
    headers.insert(
        UPLOAD_EXPIRES.clone(),
        header_value(&httpdate::fmt_http_date(
            UNIX_EPOCH + std::time::Duration::from_secs(expires_at),
        ))?,
    );
    Ok(())
}

fn header_value(value: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(value).map_err(|error| ApiError::Internal(error.to_string()))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn db_error(error: rusqlite::Error) -> ApiError {
    tracing::error!(error = %error, "resumable upload database operation failed");
    ApiError::Internal("Resumable upload state is unavailable".into())
}
