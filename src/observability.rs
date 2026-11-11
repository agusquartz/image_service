use crate::{
    auth::require_auth, config::AuthRequirement, error::ApiError, rate_limit::RateClass,
    state::AppState,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::Digest;
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
use uuid::Uuid;

const METHODS: [&str; 6] = ["GET", "HEAD", "POST", "PUT", "DELETE", "OTHER"];
const STATUS_CLASSES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];
const HISTOGRAM_BUCKETS_NS: [u64; 10] = [
    5_000_000,
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
    2_500_000_000,
    10_000_000_000,
];
const HISTOGRAM_BUCKETS: [&str; 11] = [
    "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "2.5", "10", "+Inf",
];

pub struct HttpMetrics {
    requests: [AtomicU64; METHODS.len() * STATUS_CLASSES.len()],
    request_duration_count: AtomicU64,
    request_duration_nanoseconds: AtomicU64,
    request_duration_buckets: [AtomicU64; HISTOGRAM_BUCKETS.len()],
    rate_limited: AtomicU64,
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self {
            requests: std::array::from_fn(|_| AtomicU64::new(0)),
            request_duration_count: AtomicU64::new(0),
            request_duration_nanoseconds: AtomicU64::new(0),
            request_duration_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            rate_limited: AtomicU64::new(0),
        }
    }
}

impl HttpMetrics {
    fn record(&self, method: &Method, status: StatusCode, elapsed_nanoseconds: u64) {
        let method_index = METHODS
            .iter()
            .position(|known| method.as_str() == *known)
            .unwrap_or(METHODS.len() - 1);
        let class_index = match status.as_u16() {
            100..=199 => 0,
            200..=299 => 1,
            300..=399 => 2,
            400..=499 => 3,
            _ => 4,
        };
        self.requests[method_index * STATUS_CLASSES.len() + class_index]
            .fetch_add(1, Ordering::Relaxed);
        self.request_duration_count.fetch_add(1, Ordering::Relaxed);
        self.request_duration_nanoseconds
            .fetch_add(elapsed_nanoseconds, Ordering::Relaxed);
        for (index, upper_bound) in HISTOGRAM_BUCKETS_NS.iter().enumerate() {
            if elapsed_nanoseconds <= *upper_bound {
                self.request_duration_buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.request_duration_buckets[HISTOGRAM_BUCKETS.len() - 1].fetch_add(1, Ordering::Relaxed);
        if status == StatusCode::TOO_MANY_REQUESTS {
            self.rate_limited.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn render_prometheus(&self) -> String {
        let mut output = String::from(
            "# HELP image_service_http_requests_total HTTP requests by method and status class.\n# TYPE image_service_http_requests_total counter\n",
        );
        for (method_index, method) in METHODS.iter().enumerate() {
            for (class_index, class) in STATUS_CLASSES.iter().enumerate() {
                let value = self.requests[method_index * STATUS_CLASSES.len() + class_index]
                    .load(Ordering::Relaxed);
                output.push_str(&format!(
                    "image_service_http_requests_total{{method=\"{method}\",status_class=\"{class}\"}} {value}\n"
                ));
            }
        }
        output.push_str(
            "# HELP image_service_http_request_duration_seconds HTTP request duration in seconds.\n# TYPE image_service_http_request_duration_seconds histogram\n",
        );
        for (index, bucket) in HISTOGRAM_BUCKETS.iter().enumerate() {
            let value = self.request_duration_buckets[index].load(Ordering::Relaxed);
            output.push_str(&format!(
                "image_service_http_request_duration_seconds_bucket{{le=\"{bucket}\"}} {value}\n"
            ));
        }
        let duration =
            self.request_duration_nanoseconds.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
        output.push_str(&format!(
            "image_service_http_request_duration_seconds_sum {duration:.9}\n"
        ));
        output.push_str(&format!(
            "image_service_http_request_duration_seconds_count {}\n",
            self.request_duration_count.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP image_service_rate_limited_requests_total Requests rejected by rate limits.\n# TYPE image_service_rate_limited_requests_total counter\n",
        );
        output.push_str(&format!(
            "image_service_rate_limited_requests_total {}\n",
            self.rate_limited.load(Ordering::Relaxed)
        ));
        output
    }
}

/// Adds a server-generated request ID, applies per-client read/write limits,
/// records low-cardinality metrics, and emits one structured completion log.
pub async fn observe_request(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let request_id_header = HeaderName::from_static("x-request-id");
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let route = route_label(&method, &path);
    let started = Instant::now();
    let rate_class = classify_request(&method, &path);

    let response = if path == "/health" {
        next.run(request).await
    } else {
        let identity = client_identity(&request, state.api_key.as_deref());
        match state.rate_limiter.check(&identity, rate_class) {
            Ok(()) => next.run(request).await,
            Err(retry_after) => ApiError::RateLimited(retry_after).into_response(),
        }
    };

    let elapsed = started.elapsed();
    let status = response.status();
    state.metrics.record(
        &method,
        status,
        elapsed.as_nanos().min(u64::MAX as u128) as u64,
    );
    tracing::info!(
        request_id = %request_id,
        method = %method,
        route,
        status = status.as_u16(),
        duration_ms = elapsed.as_secs_f64() * 1_000.0,
        "http request completed"
    );

    let mut response = response;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(request_id_header, value);
    }
    response
}

fn classify_request(method: &Method, path: &str) -> RateClass {
    if method == Method::GET || method == Method::HEAD || path.ends_with("/presign") {
        RateClass::Read
    } else {
        RateClass::Write
    }
}

fn route_label(method: &Method, path: &str) -> &'static str {
    match path {
        "/health" => return "health",
        "/metrics" => return "metrics",
        _ => {}
    }
    if path.ends_with("/presign") {
        return "image_presign";
    }
    if path.ends_with("/info") {
        return "image_info";
    }

    let Some(image_path) = path.strip_prefix("/api/images/") else {
        return "other";
    };
    let segment_count = image_path.split('/').count();
    if segment_count == 2 {
        return match method.as_str() {
            "GET" => "resource_list",
            "DELETE" => "resource_delete",
            _ => "other",
        };
    }

    match method.as_str() {
        "GET" => "image_get",
        "HEAD" => "image_head",
        "POST" => "image_create",
        "PUT" => "image_replace",
        "DELETE" => "image_delete",
        _ => "other",
    }
}

fn client_identity(request: &Request, api_key: Option<&str>) -> String {
    if let Some(ConnectInfo(address)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        return format!("ip:{}", address.ip());
    }

    if let (Some(api_key), Some(authorization)) = (
        api_key,
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
    ) && authorization == format!("Bearer {api_key}")
    {
        let digest = sha2::Sha256::digest(api_key.as_bytes());
        let encoded = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return format!("api-key:{encoded}");
    }

    "unknown-client".to_string()
}

pub async fn metrics(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if state.metrics_auth {
        require_auth(
            &headers,
            AuthRequirement::Required,
            state.api_key.as_deref(),
        )?;
    }
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics.render_prometheus(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_presign_requests_through_the_read_budget() {
        assert!(matches!(
            classify_request(&Method::POST, "/api/images/a/b/1/presign"),
            RateClass::Read
        ));
        assert_eq!(
            route_label(&Method::GET, "/api/images/products/customer-secret/1"),
            "image_get"
        );
        assert!(matches!(
            classify_request(&Method::POST, "/api/images/a/b/1"),
            RateClass::Write
        ));
    }

    #[test]
    fn prometheus_metrics_use_bounded_method_and_status_labels() {
        let metrics = HttpMetrics::default();
        metrics.record(&Method::GET, StatusCode::OK, 250_000_000);
        metrics.record(&Method::POST, StatusCode::TOO_MANY_REQUESTS, 50_000_000);
        let rendered = metrics.render_prometheus();

        assert!(
            rendered.contains(
                "image_service_http_requests_total{method=\"GET\",status_class=\"2xx\"} 1"
            )
        );
        assert!(
            rendered.contains(
                "image_service_http_requests_total{method=\"POST\",status_class=\"4xx\"} 1"
            )
        );
        assert!(rendered.contains("image_service_rate_limited_requests_total 1"));
        assert!(rendered.contains("image_service_http_request_duration_seconds_sum 0.300000000"));
        assert!(
            rendered.contains("image_service_http_request_duration_seconds_bucket{le=\"0.5\"} 2")
        );
    }
}
