use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use futures_util::StreamExt;
use nandunx_core::{
    authorize_in_place_expansion_target, authorize_restore_target, create_in_place_expansion_plan,
    create_operation_plan, expand_user_in_place_on_authorized_device, list_block_devices,
    parse_bis_keyset_file, preflight_in_place_user_expansion, preflight_nand_restore,
    restore_and_expand_user_to_authorized_device, restore_raw_nand_to_authorized_device,
    ArtifactKind, ArtifactReceipt, InPlaceExpansionError, InPlaceExpansionPlan,
    InPlaceExpansionPreflight, InPlaceExpansionReport, OperationMode, RestoreDeviceError,
    RestoreDeviceObserver, RestoreDeviceProgress, RestoreDeviceReport, MAX_ARTIFACT_BYTES,
    MAX_KEYSET_BYTES,
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tower_http::services::ServeDir;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    web_root: PathBuf,
    artifacts: Arc<Mutex<ArtifactStore>>,
    uploads: Arc<Mutex<HashMap<Uuid, WebUpload>>>,
    operations: Arc<Mutex<HashMap<Uuid, WebRestoreOperation>>>,
    logger: Arc<Mutex<RunLogger>>,
}

struct ArtifactStore {
    _directory: TempDir,
    entries: HashMap<Uuid, StoredArtifact>,
}

/// The headless service deliberately retains only diagnostics for its current
/// process lifetime.  It contains operational metadata only: never names or
/// paths of uploads, device identifiers, key material, or backup contents.
struct RunLogger {
    file: Option<fs::File>,
}

impl RunLogger {
    fn for_current_run() -> Self {
        let file = env::var_os("NANDUNX_LAST_RUN_LOG").and_then(|path| {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .ok()
        });
        if env::var_os("NANDUNX_LAST_RUN_LOG").is_some() && file.is_none() {
            eprintln!("NANDuNX: detailed last-run log is unavailable.");
        }
        let mut logger = Self { file };
        logger.log("service process started; previous run log was replaced");
        logger
    }

    fn log(&mut self, event: &str) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if writeln!(file, "{timestamp} {event}").is_err() {
            self.file = None;
            eprintln!("NANDuNX: detailed last-run log became unavailable.");
        }
    }
}

impl AppState {
    fn log(&self, event: impl AsRef<str>) {
        if let Ok(mut logger) = self.logger.lock() {
            logger.log(event.as_ref());
        }
    }
}

#[derive(Clone)]
struct StoredArtifact {
    path: PathBuf,
    receipt: ArtifactReceipt,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartUploadRequest {
    kind: ArtifactKind,
    part_count: usize,
    total_bytes: u64,
}

struct WebUpload {
    kind: ArtifactKind,
    part_count: usize,
    total_bytes: u64,
    part_width: usize,
    received_bytes: u64,
    received_parts: HashSet<usize>,
    parts_in_progress: HashSet<usize>,
    state: WebUploadState,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WebUploadState {
    Receiving,
    Ready,
    Failed,
}

#[derive(Clone, Serialize)]
struct WebUploadView {
    id: Uuid,
    kind: ArtifactKind,
    state: WebUploadState,
    part_count: usize,
    received_parts: usize,
    total_bytes: u64,
    received_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebPlanRequest {
    mode: OperationMode,
    backup_id: Uuid,
    keyset_id: Option<Uuid>,
    boot0_id: Option<Uuid>,
    boot1_id: Option<Uuid>,
    target_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebRestoreRequest {
    mode: OperationMode,
    backup_id: Uuid,
    keyset_id: Option<Uuid>,
    boot0_id: Option<Uuid>,
    boot1_id: Option<Uuid>,
    target_path: String,
    typed_confirmation: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebInPlaceRequest {
    keyset_id: Uuid,
    target_path: String,
    typed_confirmation: Option<String>,
}

struct WebRestoreOperation {
    cancel_requested: bool,
    status: WebRestoreOperationStatus,
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum WebRestoreOperationStatus {
    Running { progress: RestoreDeviceProgress },
    Finished { report: WebOperationReport },
    Failed { error: String },
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
enum WebOperationReport {
    Restore(RestoreDeviceReport),
    InPlace(InPlaceExpansionReport),
}

#[derive(Serialize)]
struct WebRestoreOperationView {
    id: Uuid,
    cancel_requested: bool,
    status: WebRestoreOperationStatus,
}

#[derive(Serialize)]
struct ApiErrorBody {
    error: String,
}

enum ApiError {
    BadRequest(&'static str),
    RestoreRejected(String),
    PayloadTooLarge,
    UploadStorageUnavailable,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, error) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message.to_owned()),
            Self::RestoreRejected(message) => (StatusCode::BAD_REQUEST, message),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "Plik przekracza limit 1 TiB.".to_owned(),
            ),
            Self::UploadStorageUnavailable => (
                StatusCode::INSUFFICIENT_STORAGE,
                "Brak dostępnego miejsca na sesyjny upload backupu.".to_owned(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Nie udało się przetworzyć żądania.".to_owned(),
            ),
        };
        (
            status,
            Json(ApiErrorBody {
                error: error.to_owned(),
            }),
        )
            .into_response()
    }
}

#[tokio::main]
async fn main() {
    let bind: SocketAddr = env::var("NANDUNX_BIND")
        .unwrap_or_else(|_| "127.0.0.1:4321".to_owned())
        .parse()
        .expect("NANDUNX_BIND must be a valid socket address");
    let web_root = env::var_os("NANDUNX_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("dist"));

    if !bind.ip().is_loopback() {
        panic!("Refusing a non-loopback bind. Remote access is not supported by the MVP.");
    }

    let logger = Arc::new(Mutex::new(RunLogger::for_current_run()));
    let artifacts = ArtifactStore {
        _directory: create_private_artifact_directory(),
        entries: HashMap::new(),
    };
    let app = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/uploads", post(start_upload))
        .route("/api/v1/uploads/:id", get(upload_status))
        .route("/api/v1/uploads/:id/parts/:part", post(upload_part))
        .route("/api/v1/uploads/:id/complete", post(complete_upload))
        .route("/api/v1/operations/plan", post(plan_operation))
        .route("/api/v1/operations/preflight", post(preflight_operation))
        .route("/api/v1/operations/restore", post(start_restore))
        .route(
            "/api/v1/operations/in-place/plan",
            post(plan_in_place_operation),
        )
        .route(
            "/api/v1/operations/in-place/preflight",
            post(preflight_in_place_operation),
        )
        .route(
            "/api/v1/operations/in-place",
            post(start_in_place_expansion),
        )
        .route("/api/v1/operations/:id", get(restore_status))
        .route("/api/v1/operations/:id/cancel", post(cancel_restore))
        .fallback_service(ServeDir::new(&web_root))
        .with_state(AppState {
            web_root: web_root.clone(),
            artifacts: Arc::new(Mutex::new(artifacts)),
            uploads: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(HashMap::new())),
            logger: Arc::clone(&logger),
        });

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .expect("cannot bind HTTP server");
    eprintln!(
        "NANDuNX web UI: http://{bind} (serving {})",
        web_root.display()
    );
    if let Ok(mut logger) = logger.lock() {
        logger.log("HTTP listener is ready on loopback");
    }
    axum::serve(listener, app)
        .await
        .expect("HTTP server failed");
}

fn create_private_artifact_directory() -> TempDir {
    match env::var_os("NANDUNX_ARTIFACT_DIR") {
        Some(base) => {
            fs::create_dir_all(&base).expect("cannot create artifact directory");
            clean_previous_artifact_directories(&base)
                .expect("cannot clean previous private artifact directories");
            tempfile::Builder::new()
                .prefix("session-")
                .tempdir_in(base)
                .expect("cannot create private artifact directory")
        }
        None => tempfile::tempdir().expect("cannot create private temporary directory"),
    }
}

/// A SIGTERM can stop the service before `TempDir` gets to remove itself.
/// Sessions never survive a process restart, so remove only our own old,
/// directory entries before creating the new private session.
fn clean_previous_artifact_directories(base: &std::ffi::OsStr) -> std::io::Result<()> {
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("session-") && entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "nandunx-web",
        "app_version": env!("CARGO_PKG_VERSION"),
        "edition": if env::var("NANDUNX_EDITION").ok().as_deref() == Some("docker") { "docker" } else { "web" },
        "engine_version": nandunx_core::engine_version(),
        "web_root": state.web_root,
        "write_operations_enabled": true
    }))
}

async fn devices() -> Result<Json<Vec<nandunx_core::BlockDevice>>, ApiError> {
    list_block_devices()
        .map(Json)
        .map_err(|_| ApiError::Internal)
}

async fn start_upload(
    State(state): State<AppState>,
    Json(request): Json<StartUploadRequest>,
) -> Result<(StatusCode, Json<WebUploadView>), ApiError> {
    let max_bytes = upload_limit(request.kind);
    if request.part_count == 0 || request.total_bytes == 0 {
        return Err(ApiError::BadRequest(
            "Upload musi zawierać co najmniej jeden niepusty plik.",
        ));
    }
    if request.total_bytes > max_bytes {
        return Err(if request.kind == ArtifactKind::Keyset {
            ApiError::BadRequest("Keyset przekracza limit 1 MiB.")
        } else {
            ApiError::PayloadTooLarge
        });
    }
    if request.kind != ArtifactKind::Backup && request.part_count != 1 {
        return Err(ApiError::BadRequest(
            "Ten typ artefaktu musi być pojedynczym plikiem.",
        ));
    }

    let id = Uuid::new_v4();
    let upload = WebUpload {
        kind: request.kind,
        part_count: request.part_count,
        total_bytes: request.total_bytes,
        part_width: split_part_width(request.part_count),
        received_bytes: 0,
        received_parts: HashSet::new(),
        parts_in_progress: HashSet::new(),
        state: WebUploadState::Receiving,
    };
    let view = upload_view(id, &upload);
    state
        .uploads
        .lock()
        .map_err(|_| ApiError::Internal)?
        .insert(id, upload);
    state.log(format!(
        "upload session {id} started: kind={}, parts={}, bytes={}",
        artifact_kind_name(request.kind),
        request.part_count,
        request.total_bytes
    ));
    Ok((StatusCode::CREATED, Json(view)))
}

async fn upload_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WebUploadView>, ApiError> {
    let uploads = state.uploads.lock().map_err(|_| ApiError::Internal)?;
    let upload = uploads
        .get(&id)
        .ok_or(ApiError::BadRequest("Sesja uploadu nie jest dostępna."))?;
    Ok(Json(upload_view(id, upload)))
}

async fn upload_part(
    State(state): State<AppState>,
    Path((id, part)): Path<(Uuid, usize)>,
    body: Body,
) -> Result<Json<WebUploadView>, ApiError> {
    let (path, total_bytes) = reserve_upload_part(&state, id, part)?;
    let mut output = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(_) => {
            fail_upload(&state, id, "upload storage could not create a part");
            return Err(ApiError::UploadStorageUnavailable);
        }
    };

    let mut part_byte_len = 0_u64;
    let mut chunks = body.into_data_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                fail_upload(&state, id, "upload connection was interrupted");
                return Err(ApiError::BadRequest("Przesyłanie pliku zostało przerwane."));
            }
        };
        let next_part_len = match part_byte_len.checked_add(chunk.len() as u64) {
            Some(value) => value,
            None => {
                fail_upload(&state, id, "upload part size overflowed");
                return Err(ApiError::PayloadTooLarge);
            }
        };
        let received_bytes = match current_upload_bytes(&state, id) {
            Ok(value) => value,
            Err(error) => return Err(error),
        };
        if received_bytes.saturating_add(chunk.len() as u64) > total_bytes {
            fail_upload(&state, id, "upload exceeded its declared size");
            return Err(ApiError::PayloadTooLarge);
        }
        if output.write_all(&chunk).await.is_err() {
            fail_upload(&state, id, "upload storage could not write a part");
            return Err(ApiError::UploadStorageUnavailable);
        }
        part_byte_len = next_part_len;
        if add_upload_bytes(&state, id, chunk.len() as u64).is_err() {
            fail_upload(&state, id, "upload session disappeared during transfer");
            return Err(ApiError::BadRequest("Sesja uploadu nie jest dostępna."));
        }
    }
    if output.flush().await.is_err() {
        fail_upload(&state, id, "upload storage could not flush a part");
        return Err(ApiError::UploadStorageUnavailable);
    }
    if part_byte_len == 0 {
        fail_upload(&state, id, "an empty upload part was rejected");
        return Err(ApiError::BadRequest("Wybrany plik jest pusty."));
    }

    let view = finish_upload_part(&state, id, part)?;
    state.log(format!(
        "upload session {id}: part {}/{} stored, bytes={}/{}",
        part + 1,
        view.part_count,
        view.received_bytes,
        view.total_bytes
    ));
    Ok(Json(view))
}

async fn complete_upload(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ArtifactReceipt>, ApiError> {
    let (kind, total_bytes, part_width) = {
        let mut uploads = state.uploads.lock().map_err(|_| ApiError::Internal)?;
        let upload = uploads
            .get_mut(&id)
            .ok_or(ApiError::BadRequest("Sesja uploadu nie jest dostępna."))?;
        if upload.state != WebUploadState::Receiving
            || !upload.parts_in_progress.is_empty()
            || upload.received_parts.len() != upload.part_count
            || upload.received_bytes != upload.total_bytes
        {
            return Err(ApiError::BadRequest(
                "Upload nie został jeszcze w całości zapisany.",
            ));
        }
        upload.state = WebUploadState::Ready;
        (upload.kind, upload.total_bytes, upload.part_width)
    };
    let path = upload_part_path(&state, id, 0, kind, part_width)?;
    let receipt = ArtifactReceipt {
        id: id.to_string(),
        kind,
        byte_len: total_bytes,
    };
    state
        .artifacts
        .lock()
        .map_err(|_| ApiError::Internal)?
        .entries
        .insert(
            id,
            StoredArtifact {
                path,
                receipt: receipt.clone(),
            },
        );
    state.log(format!(
        "upload session {id} completed: kind={}, bytes={}",
        artifact_kind_name(kind),
        total_bytes
    ));
    Ok(Json(receipt))
}

fn upload_limit(kind: ArtifactKind) -> u64 {
    if kind == ArtifactKind::Keyset {
        MAX_KEYSET_BYTES
    } else {
        MAX_ARTIFACT_BYTES
    }
}

fn split_part_width(part_count: usize) -> usize {
    part_count.saturating_sub(1).to_string().len().max(2)
}

fn upload_view(id: Uuid, upload: &WebUpload) -> WebUploadView {
    WebUploadView {
        id,
        kind: upload.kind,
        state: upload.state.clone(),
        part_count: upload.part_count,
        received_parts: upload.received_parts.len(),
        total_bytes: upload.total_bytes,
        received_bytes: upload.received_bytes,
    }
}

fn upload_part_path(
    state: &AppState,
    id: Uuid,
    part: usize,
    kind: ArtifactKind,
    part_width: usize,
) -> Result<PathBuf, ApiError> {
    let directory = state
        .artifacts
        .lock()
        .map_err(|_| ApiError::Internal)?
        ._directory
        .path()
        .to_path_buf();
    let filename = match kind {
        ArtifactKind::Backup => format!("{id}.{part:0part_width$}"),
        ArtifactKind::Keyset => format!("{id}.keyset"),
        ArtifactKind::Boot0 => format!("{id}.boot0"),
        ArtifactKind::Boot1 => format!("{id}.boot1"),
    };
    Ok(directory.join(filename))
}

fn reserve_upload_part(
    state: &AppState,
    id: Uuid,
    part: usize,
) -> Result<(PathBuf, u64), ApiError> {
    let (kind, part_width, total_bytes) = {
        let mut uploads = state.uploads.lock().map_err(|_| ApiError::Internal)?;
        let upload = uploads
            .get_mut(&id)
            .ok_or(ApiError::BadRequest("Sesja uploadu nie jest dostępna."))?;
        if upload.state != WebUploadState::Receiving {
            return Err(ApiError::BadRequest(
                "Sesja uploadu nie przyjmuje już części.",
            ));
        }
        if part >= upload.part_count {
            return Err(ApiError::BadRequest(
                "Numer części uploadu jest nieprawidłowy.",
            ));
        }
        if upload.received_parts.contains(&part) || !upload.parts_in_progress.insert(part) {
            return Err(ApiError::BadRequest(
                "Ta część uploadu została już wysłana.",
            ));
        }
        (upload.kind, upload.part_width, upload.total_bytes)
    };
    Ok((
        upload_part_path(state, id, part, kind, part_width)?,
        total_bytes,
    ))
}

fn current_upload_bytes(state: &AppState, id: Uuid) -> Result<u64, ApiError> {
    state
        .uploads
        .lock()
        .map_err(|_| ApiError::Internal)?
        .get(&id)
        .map(|upload| upload.received_bytes)
        .ok_or(ApiError::BadRequest("Sesja uploadu nie jest dostępna."))
}

fn add_upload_bytes(state: &AppState, id: Uuid, byte_len: u64) -> Result<(), ApiError> {
    let mut uploads = state.uploads.lock().map_err(|_| ApiError::Internal)?;
    let upload = uploads
        .get_mut(&id)
        .ok_or(ApiError::BadRequest("Sesja uploadu nie jest dostępna."))?;
    if upload.state != WebUploadState::Receiving {
        return Err(ApiError::BadRequest(
            "Sesja uploadu nie przyjmuje już części.",
        ));
    }
    upload.received_bytes = upload
        .received_bytes
        .checked_add(byte_len)
        .ok_or(ApiError::PayloadTooLarge)?;
    Ok(())
}

fn finish_upload_part(state: &AppState, id: Uuid, part: usize) -> Result<WebUploadView, ApiError> {
    let mut uploads = state.uploads.lock().map_err(|_| ApiError::Internal)?;
    let upload = uploads
        .get_mut(&id)
        .ok_or(ApiError::BadRequest("Sesja uploadu nie jest dostępna."))?;
    if upload.state != WebUploadState::Receiving || !upload.parts_in_progress.remove(&part) {
        return Err(ApiError::BadRequest("Ta część uploadu nie jest aktywna."));
    }
    upload.received_parts.insert(part);
    Ok(upload_view(id, upload))
}

fn fail_upload(state: &AppState, id: Uuid, event: &str) {
    if let Ok(mut uploads) = state.uploads.lock() {
        if let Some(upload) = uploads.get_mut(&id) {
            upload.parts_in_progress.clear();
            upload.state = WebUploadState::Failed;
        }
    }
    state.log(format!("upload session {id} failed: {event}"));
}

async fn plan_operation(
    State(state): State<AppState>,
    Json(request): Json<WebPlanRequest>,
) -> Result<Json<nandunx_core::OperationPlan>, ApiError> {
    state.log(format!(
        "plan requested: mode={}",
        operation_mode_name(request.mode)
    ));
    let (plan, _, _, _, _) = resolve_web_plan(&state, &request)?;
    state.log("plan completed");
    Ok(Json(plan))
}

async fn preflight_operation(
    State(state): State<AppState>,
    Json(request): Json<WebPlanRequest>,
) -> Result<Json<nandunx_core::PreflightReport>, ApiError> {
    state.log(format!(
        "preflight requested: mode={}",
        operation_mode_name(request.mode)
    ));
    let (plan, backup_path, keyset_path, boot0_path, boot1_path) =
        resolve_web_plan(&state, &request)?;
    preflight_nand_restore(
        plan,
        backup_path,
        keyset_path.as_deref(),
        boot0_path.as_deref(),
        boot1_path.as_deref(),
    )
    .map(|report| {
        state.log("preflight completed");
        Json(report)
    })
    .map_err(|_| {
        state.log("preflight rejected by validation");
        ApiError::BadRequest("Preflight backupu lub keysetu nie przeszedł walidacji.")
    })
}

async fn plan_in_place_operation(
    State(state): State<AppState>,
    Json(request): Json<WebInPlaceRequest>,
) -> Result<Json<InPlaceExpansionPlan>, ApiError> {
    state.log("in-place plan requested");
    let (plan, _) = resolve_in_place_plan(&state, &request)?;
    state.log("in-place plan completed");
    Ok(Json(plan))
}

async fn preflight_in_place_operation(
    State(state): State<AppState>,
    Json(request): Json<WebInPlaceRequest>,
) -> Result<Json<InPlaceExpansionPreflight>, ApiError> {
    state.log("in-place preflight requested");
    let (plan, keyset_path) = resolve_in_place_plan(&state, &request)?;
    let key = in_place_user_key(&keyset_path)?;
    preflight_in_place_user_expansion(plan, &key)
        .map(|report| {
            state.log("in-place preflight completed");
            Json(report)
        })
        .map_err(|_| {
            state.log("in-place preflight rejected by validation");
            ApiError::BadRequest("Preflight urządzenia lub keysetu nie przeszedł walidacji.")
        })
}

async fn start_in_place_expansion(
    State(state): State<AppState>,
    Json(request): Json<WebInPlaceRequest>,
) -> Result<(StatusCode, Json<WebRestoreOperationView>), ApiError> {
    state.log("in-place expansion request received");
    let (plan, keyset_path) = resolve_in_place_plan(&state, &request)?;
    let key = in_place_user_key(&keyset_path)?;
    // Rebuild the complete read-only plan immediately before the target lock.
    preflight_in_place_user_expansion(plan.clone(), &key)
        .map_err(|error| ApiError::RestoreRejected(error.to_string()))?;
    let typed_confirmation = request
        .typed_confirmation
        .as_deref()
        .ok_or(ApiError::BadRequest(
            "Wpisz pełną ścieżkę urządzenia do potwierdzenia.",
        ))?;
    let authorized = authorize_in_place_expansion_target(&plan, typed_confirmation)
        .map_err(|error| ApiError::RestoreRejected(error.to_string()))?;

    let id = Uuid::new_v4();
    let initial_progress = RestoreDeviceProgress {
        phase: nandunx_core::RestoreDevicePhase::RelocatingUser,
        processed_bytes: 0,
        total_bytes: 0,
    };
    let view = WebRestoreOperationView {
        id,
        cancel_requested: false,
        status: WebRestoreOperationStatus::Running {
            progress: initial_progress,
        },
    };
    state
        .operations
        .lock()
        .map_err(|_| ApiError::Internal)?
        .insert(
            id,
            WebRestoreOperation {
                cancel_requested: false,
                status: view.status.clone(),
            },
        );
    state.log(format!("in-place operation {id}: authorized and queued"));

    let operations = Arc::clone(&state.operations);
    let logger = Arc::clone(&state.logger);
    tokio::task::spawn_blocking(move || {
        let mut observer = WebRestoreObserver {
            id,
            operations: Arc::clone(&operations),
            logger: Arc::clone(&logger),
            last_phase: None,
            last_logged_bytes: 0,
        };
        log_with(&logger, format!("in-place operation {id}: worker started"));
        let result =
            expand_user_in_place_on_authorized_device(&plan, &authorized, &key, &mut observer);
        match &result {
            Ok(report) => log_with(
                &logger,
                format!(
                    "in-place operation {id}: finished with status {:?}",
                    report.status
                ),
            ),
            Err(_) => log_with(&logger, format!("in-place operation {id}: failed")),
        }
        if let Ok(mut operations) = operations.lock() {
            let Some(operation) = operations.get_mut(&id) else {
                return;
            };
            operation.status = match result {
                Ok(report) => WebRestoreOperationStatus::Finished {
                    report: WebOperationReport::InPlace(report),
                },
                Err(error) => WebRestoreOperationStatus::Failed {
                    error: in_place_error_message(error),
                },
            };
        }
    });
    Ok((StatusCode::ACCEPTED, Json(view)))
}

async fn start_restore(
    State(state): State<AppState>,
    Json(request): Json<WebRestoreRequest>,
) -> Result<(StatusCode, Json<WebRestoreOperationView>), ApiError> {
    state.log(format!(
        "restore request received: mode={}",
        operation_mode_name(request.mode)
    ));
    let plan_request = WebPlanRequest {
        mode: request.mode,
        backup_id: request.backup_id,
        keyset_id: request.keyset_id,
        boot0_id: request.boot0_id,
        boot1_id: request.boot1_id,
        target_path: request.target_path,
    };
    let (plan, backup_path, keyset_path, boot0_path, boot1_path) =
        resolve_web_plan(&state, &plan_request)?;
    if plan.boot0.is_some() || plan.boot1.is_some() {
        return Err(ApiError::BadRequest(
            "Writer nie przywraca jeszcze BOOT0/BOOT1; przywróć je osobno przez Hekate.",
        ));
    }

    // Re-run all source checks immediately before authorising the target.
    // The write call repeats target identification and mount checks again
    // after opening its read/write descriptor.
    preflight_nand_restore(
        plan.clone(),
        &backup_path,
        keyset_path.as_deref(),
        boot0_path.as_deref(),
        boot1_path.as_deref(),
    )
    .map_err(|error| ApiError::RestoreRejected(error.to_string()))?;
    let user_key = if request.mode == OperationMode::RestoreAndExpandUser {
        let keyset_path = keyset_path.as_deref().ok_or(ApiError::BadRequest(
            "Rozszerzenie USER wymaga keysetu w bieżącej sesji.",
        ))?;
        parse_bis_keyset_file(keyset_path)
            .map_err(|_| {
                ApiError::RestoreRejected("Keyset nie przeszedł ponownej walidacji.".into())
            })?
            .user_key()
            .cloned()
            .ok_or(ApiError::BadRequest(
                "Keyset nie zawiera BIS Key 2 wymaganego dla USER.",
            ))?
            .into()
    } else {
        None
    };
    let authorized = authorize_restore_target(&plan, &backup_path, &request.typed_confirmation)
        .map_err(|error| ApiError::RestoreRejected(error.to_string()))?;

    let initial_progress = RestoreDeviceProgress {
        phase: nandunx_core::RestoreDevicePhase::CopyingPayload,
        processed_bytes: 0,
        total_bytes: 0,
    };
    let id = Uuid::new_v4();
    state.log(format!("restore operation {id}: authorized and queued"));
    let view = WebRestoreOperationView {
        id,
        cancel_requested: false,
        status: WebRestoreOperationStatus::Running {
            progress: initial_progress,
        },
    };
    state
        .operations
        .lock()
        .map_err(|_| ApiError::Internal)?
        .insert(
            id,
            WebRestoreOperation {
                cancel_requested: false,
                status: view.status.clone(),
            },
        );

    let operations = Arc::clone(&state.operations);
    let logger = Arc::clone(&state.logger);
    tokio::task::spawn_blocking(move || {
        let mut observer = WebRestoreObserver {
            id,
            operations: Arc::clone(&operations),
            logger: Arc::clone(&logger),
            last_phase: None,
            last_logged_bytes: 0,
        };
        log_with(&logger, format!("restore operation {id}: worker started"));
        let result = match user_key.as_ref() {
            Some(user_key) => restore_and_expand_user_to_authorized_device(
                &plan,
                &authorized,
                backup_path,
                user_key,
                &mut observer,
            ),
            None => restore_raw_nand_to_authorized_device(
                &plan,
                &authorized,
                backup_path,
                &mut observer,
            ),
        };
        match &result {
            Ok(report) => log_with(
                &logger,
                format!(
                    "restore operation {id}: finished with status {:?}",
                    report.status
                ),
            ),
            Err(_) => log_with(&logger, format!("restore operation {id}: failed")),
        }
        if let Ok(mut operations) = operations.lock() {
            let Some(operation) = operations.get_mut(&id) else {
                return;
            };
            operation.status = match result {
                Ok(report) => WebRestoreOperationStatus::Finished {
                    report: WebOperationReport::Restore(report),
                },
                Err(error) => WebRestoreOperationStatus::Failed {
                    error: device_error_message(error),
                },
            };
        }
    });

    Ok((StatusCode::ACCEPTED, Json(view)))
}

async fn restore_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WebRestoreOperationView>, ApiError> {
    let operations = state.operations.lock().map_err(|_| ApiError::Internal)?;
    let operation = operations
        .get(&id)
        .ok_or(ApiError::BadRequest("Operacja nie jest dostępna."))?;
    Ok(Json(WebRestoreOperationView {
        id,
        cancel_requested: operation.cancel_requested,
        status: operation.status.clone(),
    }))
}

async fn cancel_restore(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WebRestoreOperationView>, ApiError> {
    let mut operations = state.operations.lock().map_err(|_| ApiError::Internal)?;
    let operation = operations
        .get_mut(&id)
        .ok_or(ApiError::BadRequest("Operacja nie jest dostępna."))?;
    if matches!(operation.status, WebRestoreOperationStatus::Running { .. }) {
        operation.cancel_requested = true;
        state.log(format!("restore operation {id}: cancellation requested"));
    }
    Ok(Json(WebRestoreOperationView {
        id,
        cancel_requested: operation.cancel_requested,
        status: operation.status.clone(),
    }))
}

struct WebRestoreObserver {
    id: Uuid,
    operations: Arc<Mutex<HashMap<Uuid, WebRestoreOperation>>>,
    logger: Arc<Mutex<RunLogger>>,
    last_phase: Option<nandunx_core::RestoreDevicePhase>,
    last_logged_bytes: u64,
}

impl RestoreDeviceObserver for WebRestoreObserver {
    fn on_progress(&mut self, progress: RestoreDeviceProgress) -> bool {
        const PROGRESS_LOG_INTERVAL_BYTES: u64 = 64 * 1024 * 1024;
        let phase_changed = self.last_phase != Some(progress.phase);
        let completed =
            progress.total_bytes != 0 && progress.processed_bytes == progress.total_bytes;
        if phase_changed
            || completed
            || progress
                .processed_bytes
                .saturating_sub(self.last_logged_bytes)
                >= PROGRESS_LOG_INTERVAL_BYTES
        {
            log_with(
                &self.logger,
                format!(
                    "restore operation {}: phase={:?}, bytes={}/{}",
                    self.id, progress.phase, progress.processed_bytes, progress.total_bytes
                ),
            );
            self.last_phase = Some(progress.phase);
            self.last_logged_bytes = progress.processed_bytes;
        }
        let Ok(mut operations) = self.operations.lock() else {
            return false;
        };
        let Some(operation) = operations.get_mut(&self.id) else {
            return false;
        };
        if let WebRestoreOperationStatus::Running {
            progress: current_progress,
        } = &mut operation.status
        {
            *current_progress = progress;
        }
        !operation.cancel_requested
    }
}

fn log_with(logger: &Arc<Mutex<RunLogger>>, event: String) {
    if let Ok(mut logger) = logger.lock() {
        logger.log(&event);
    }
}

fn artifact_kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Backup => "backup",
        ArtifactKind::Keyset => "keyset",
        ArtifactKind::Boot0 => "boot0",
        ArtifactKind::Boot1 => "boot1",
    }
}

fn operation_mode_name(mode: OperationMode) -> &'static str {
    match mode {
        OperationMode::Restore => "restore",
        OperationMode::RestoreAndExpandUser => "restore_and_expand_user",
    }
}

fn device_error_message(error: RestoreDeviceError) -> String {
    error.to_string()
}

fn in_place_error_message(error: InPlaceExpansionError) -> String {
    error.to_string()
}

fn in_place_user_key(path: &std::path::Path) -> Result<nandunx_core::BisKey, ApiError> {
    parse_bis_keyset_file(path)
        .map_err(|_| ApiError::RestoreRejected("Keyset nie przeszedł ponownej walidacji.".into()))?
        .user_key()
        .cloned()
        .ok_or(ApiError::BadRequest(
            "Keyset nie zawiera BIS Key 2 wymaganego dla USER.",
        ))
}

fn resolve_in_place_plan(
    state: &AppState,
    request: &WebInPlaceRequest,
) -> Result<(InPlaceExpansionPlan, PathBuf), ApiError> {
    let keyset_path = {
        let store = state.artifacts.lock().map_err(|_| ApiError::Internal)?;
        let keyset = store
            .entries
            .get(&request.keyset_id)
            .ok_or(ApiError::BadRequest(
                "Keyset nie jest dostępny w tej sesji.",
            ))?;
        if keyset.receipt.kind != ArtifactKind::Keyset || !keyset.path.is_file() {
            return Err(ApiError::BadRequest(
                "Keyset nie jest dostępny w tej sesji.",
            ));
        }
        keyset.path.clone()
    };
    let target = list_block_devices()
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .find(|device| device.path == request.target_path)
        .ok_or(ApiError::BadRequest(
            "Wybrane urządzenie nie jest dostępne.",
        ))?;
    let plan = create_in_place_expansion_plan(target)
        .map_err(|error| ApiError::RestoreRejected(error.to_string()))?;
    Ok((plan, keyset_path))
}

fn resolve_web_plan(
    state: &AppState,
    request: &WebPlanRequest,
) -> Result<
    (
        nandunx_core::OperationPlan,
        PathBuf,
        Option<PathBuf>,
        Option<PathBuf>,
        Option<PathBuf>,
    ),
    ApiError,
> {
    let (backup, keyset, backup_path, keyset_path, boot0, boot1, boot0_path, boot1_path) = {
        let store = state.artifacts.lock().map_err(|_| ApiError::Internal)?;
        let backup = store
            .entries
            .get(&request.backup_id)
            .ok_or(ApiError::BadRequest(
                "Backup nie jest dostępny w tej sesji.",
            ))?;
        if !backup.path.is_file() {
            return Err(ApiError::BadRequest(
                "Plik nie jest już dostępny w tej sesji.",
            ));
        }
        let backup_path = backup.path.clone();
        let keyset = match request.keyset_id {
            Some(keyset_id) => {
                let artifact = store.entries.get(&keyset_id).ok_or(ApiError::BadRequest(
                    "Keyset nie jest dostępny w tej sesji.",
                ))?;
                if !artifact.path.is_file() {
                    return Err(ApiError::BadRequest(
                        "Plik nie jest już dostępny w tej sesji.",
                    ));
                }
                Some((artifact.receipt.clone(), artifact.path.clone()))
            }
            None => None,
        };
        let (keyset, keyset_path) = match keyset {
            Some((receipt, path)) => (Some(receipt), Some(path)),
            None => (None, None),
        };
        let resolve_boot = |id: Option<Uuid>,
                            name: &'static str|
         -> Result<Option<(ArtifactReceipt, PathBuf)>, ApiError> {
            id.map(|id| {
                let artifact = store.entries.get(&id).ok_or(ApiError::BadRequest(name))?;
                if !artifact.path.is_file() {
                    return Err(ApiError::BadRequest(name));
                }
                Ok((artifact.receipt.clone(), artifact.path.clone()))
            })
            .transpose()
        };
        let boot0 = resolve_boot(request.boot0_id, "BOOT0 nie jest dostępny w tej sesji.")?;
        let boot1 = resolve_boot(request.boot1_id, "BOOT1 nie jest dostępny w tej sesji.")?;
        let (boot0, boot0_path) =
            boot0.map_or((None, None), |(receipt, path)| (Some(receipt), Some(path)));
        let (boot1, boot1_path) =
            boot1.map_or((None, None), |(receipt, path)| (Some(receipt), Some(path)));
        (
            backup.receipt.clone(),
            keyset,
            backup_path,
            keyset_path,
            boot0,
            boot1,
            boot0_path,
            boot1_path,
        )
    };
    let target = list_block_devices()
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .find(|device| device.path == request.target_path)
        .ok_or(ApiError::BadRequest(
            "Wybrane urządzenie nie jest dostępne.",
        ))?;
    let plan = create_operation_plan(request.mode, backup, keyset, boot0, boot1, target)
        .map_err(|_| ApiError::BadRequest("Plan nie przeszedł walidacji bezpieczeństwa."))?;
    Ok((plan, backup_path, keyset_path, boot0_path, boot1_path))
}
