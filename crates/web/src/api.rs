//! HTTP routes: the two pages, their static assets, the status stream and operator actions.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use masscap_engine::events::{Event, EventKind};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

use crate::state::{AppState, Outcome, Refusal};

type St = State<Arc<AppState>>;

/// Status updates per second on the stream.
const STREAM_INTERVAL: Duration = Duration::from_millis(100);
/// Newest events included in each status payload.
const EVENTS_SENT: usize = 100;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(|| page(INDEX_HTML)))
        .route("/advanced", get(|| page(ADVANCED_HTML)))
        .route("/static/{file}", get(static_file))
        .route("/api/status", get(status_once))
        .route("/api/status/stream", get(status_stream))
        .route("/api/record/start", post(|State(s): St| async move { reply(s.start()) }))
        .route("/api/record/stop", post(|State(s): St| async move { reply(s.stop()) }))
        .route("/api/marker", post(marker))
        .route("/api/session", post(session))
        .route("/api/destinations", post(add_destination).delete(remove_destination))
        .route("/api/destinations/measure", post(measure))
        .route("/api/use-sim", post(|State(s): St| async move { reply(s.use_sim()) }))
        .with_state(state)
}

// ---- pages and assets -------------------------------------------------------

const INDEX_HTML: &str = include_str!("../static/index.html");
const ADVANCED_HTML: &str = include_str!("../static/advanced.html");

async fn page(html: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], html).into_response()
}

async fn static_file(Path(file): Path<String>) -> Response {
    let (body, ctype) = match file.as_str() {
        "common.css" => (include_str!("../static/common.css"), "text/css; charset=utf-8"),
        "common.js" => (include_str!("../static/common.js"), "text/javascript; charset=utf-8"),
        "advanced.js" => (include_str!("../static/advanced.js"), "text/javascript; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

// ---- status -------------------------------------------------------------------

/// Channel names and file paths change rarely; a stream sends them only when they change.
#[derive(Default)]
struct Sent {
    names: Option<Vec<String>>,
    paths: Option<Vec<String>>,
}

fn round1(v: f32) -> f64 {
    (v as f64 * 10.0).round() / 10.0
}

fn kind_name(k: &EventKind) -> &'static str {
    match k {
        EventKind::TakeStarted { .. } => "TakeStarted",
        EventKind::TakeStopped { .. } => "TakeStopped",
        EventKind::Marker { .. } => "Marker",
        EventKind::Drop { .. } => "Drop",
        EventKind::Clip { .. } => "Clip",
        EventKind::SignalLost { .. } => "SignalLost",
        EventKind::SignalReturned { .. } => "SignalReturned",
        EventKind::DiskFailed { .. } => "DiskFailed",
        EventKind::DiskAssigned { .. } => "DiskAssigned",
        EventKind::CacheFill { .. } => "CacheFill",
        EventKind::Info(_) => "Info",
    }
}

fn event_json(e: &Event) -> Value {
    json!({
        "time": e.time.format("%H:%M:%S").to_string(),
        "elapsed": e.elapsed,
        "severity": e.severity(),
        "kind": kind_name(&e.kind),
        "message": e.message(),
    })
}

/// One status payload. Meters are compact arrays: `[peak, hold, rms, clips, dropped, state]`.
fn payload(state: &AppState, sent: &mut Sent) -> Value {
    let s = state.status();
    let meters: Vec<Value> = s.meters.iter().map(|m| json!([round1(m.peak_dbfs), round1(m.hold_dbfs), round1(m.rms_dbfs), m.clips_total, m.dropped_frames_total, m.state])).collect();
    let skip = s.recent_events.len().saturating_sub(EVENTS_SENT);
    let events: Vec<Value> = s.recent_events.iter().skip(skip).map(event_json).collect();
    let mut v = json!({
        "state": s.state,
        "session": s.session,
        "device_name": s.device_name,
        "sample_rate": s.sample_rate,
        "channels": s.channels,
        "take_name": s.take_name,
        "elapsed_seconds": s.elapsed_seconds,
        "remaining_seconds": s.remaining_seconds,
        "free_bytes_total": s.free_bytes_total,
        "cache_bytes": s.cache_bytes,
        "cache_fill": s.cache_fill,
        "drops_total": s.drops_total,
        "clips_total": s.clips_total,
        "silent_count": s.silent_count,
        "markers": s.markers,
        "meters": meters,
        "channel_bytes": s.channel_bytes,
        "disks": s.disks,
        "last_error": s.last_error,
        "not_ready": s.not_ready,
        "events": events,
        "ui": state.ui_info(),
    });
    if sent.names.as_ref() != Some(&s.channel_names) {
        v["channel_names"] = json!(s.channel_names);
        sent.names = Some(s.channel_names);
    }
    if sent.paths.as_ref() != Some(&s.channel_paths) {
        v["channel_paths"] = json!(s.channel_paths);
        sent.paths = Some(s.channel_paths);
    }
    v
}

async fn status_once(State(s): St) -> Json<Value> {
    Json(payload(&s, &mut Sent::default()))
}

async fn status_stream(State(s): St) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let mut tick = tokio::time::interval(STREAM_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = Sent::default();
    let stream = IntervalStream::new(tick).map(move |_| Ok(SseEvent::default().data(payload(&s, &mut sent).to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---- actions --------------------------------------------------------------------

fn reply(o: Outcome) -> Response {
    match o {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(Refusal::Conflict(m)) => (StatusCode::CONFLICT, m).into_response(),
        Err(Refusal::BadRequest(m)) => (StatusCode::BAD_REQUEST, m).into_response(),
    }
}

/// Parse the JSON body (an empty body means the default, so `curl -X POST` works) and run `f`.
fn with_body<T: DeserializeOwned + Default>(b: &Bytes, f: impl FnOnce(T) -> Outcome) -> Response {
    if b.iter().all(|c| c.is_ascii_whitespace()) {
        return reply(f(T::default()));
    }
    match serde_json::from_slice(b) {
        Ok(v) => reply(f(v)),
        Err(e) => (StatusCode::BAD_REQUEST, format!("bad JSON body: {e}")).into_response(),
    }
}

#[derive(Deserialize, Default)]
struct MarkBody {
    #[serde(default)]
    label: String,
}

async fn marker(State(s): St, b: Bytes) -> Response {
    with_body(&b, |m: MarkBody| s.mark(m.label))
}

#[derive(Deserialize, Default)]
struct SessionBody {
    #[serde(default)]
    name: String,
}

async fn session(State(s): St, b: Bytes) -> Response {
    with_body(&b, |m: SessionBody| s.set_session(&m.name))
}

#[derive(Deserialize, Default)]
struct PathBody {
    #[serde(default)]
    path: String,
    #[serde(default)]
    test_gb: Option<f64>,
}

async fn add_destination(State(s): St, b: Bytes) -> Response {
    with_body(&b, |m: PathBody| s.add_destination(&m.path))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

async fn remove_destination(State(s): St, Query(q): Query<PathQuery>) -> Response {
    reply(s.remove_destination(&q.path))
}

async fn measure(State(s): St, b: Bytes) -> Response {
    with_body(&b, |m: PathBody| s.start_measure(&m.path, m.test_gb))
}
