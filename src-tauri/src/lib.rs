#[cfg(target_os = "windows")]
mod diagnostics;
mod operations;

use operations::{OperationReport, OperationView, Operations};

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[cfg(target_os = "windows")]
use diagnostics::{diagnostic, init_diagnostics};
#[cfg(target_os = "windows")]
use nandunx_core::{
    authorize_windows_in_place, authorize_windows_restore,
    preflight_windows_in_place_user_expansion, preflight_windows_nand_restore, WindowsDiskSnapshot,
};
use nandunx_core::{
    create_in_place_expansion_plan, create_operation_plan, ArtifactKind, ArtifactReceipt,
    InPlaceExpansionPlan, InPlaceExpansionPreflight, OperationMode, OperationPlan, PreflightReport,
    MAX_ARTIFACT_BYTES,
};
#[cfg(not(target_os = "windows"))]
use nandunx_core::{
    list_block_devices, parse_bis_keyset_file, preflight_in_place_user_expansion,
    preflight_nand_restore,
};
use serde::Serialize;
use tauri::{Manager, State};
use uuid::Uuid;

struct DesktopArtifact {
    path: PathBuf,
    receipt: ArtifactReceipt,
}

#[derive(Clone, Default)]
struct DesktopState {
    artifacts: Arc<Mutex<HashMap<Uuid, DesktopArtifact>>>,
    operations: Arc<Operations>,
    #[cfg(target_os = "windows")]
    windows_disks: Arc<Mutex<HashMap<String, WindowsDiskSnapshot>>>,
    #[cfg(target_os = "windows")]
    mica_enabled: Arc<AtomicBool>,
}

#[derive(Serialize)]
struct Status {
    app_version: &'static str,
    edition: &'static str,
    engine_version: &'static str,
    write_operations_enabled: bool,
    mica_enabled: bool,
}

#[tauri::command]
fn status(state: State<DesktopState>) -> Status {
    Status {
        app_version: env!("CARGO_PKG_VERSION"),
        edition: if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        },
        engine_version: nandunx_core::engine_version(),
        write_operations_enabled: cfg!(any(target_os = "linux", target_os = "windows")),
        mica_enabled: {
            #[cfg(target_os = "windows")]
            {
                state.mica_enabled.load(Ordering::Relaxed)
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = state;
                false
            }
        },
    }
}

#[tauri::command]
fn list_desktop_block_devices(
    state: State<DesktopState>,
) -> Result<Vec<nandunx_core::BlockDevice>, String> {
    #[cfg(target_os = "windows")]
    {
        diagnostic("listing Windows disks");
        let disks = nandunx_core::list_windows_disks().map_err(|error| {
            diagnostic(&format!("Windows disk inventory failed: {error}"));
            format!("Nie udało się odczytać listy urządzeń blokowych: {error}")
        })?;
        diagnostic(&format!(
            "disk inventory returned {} candidates",
            disks.len()
        ));
        let devices = disks.iter().map(|disk| disk.device.clone()).collect();
        *state
            .windows_disks
            .lock()
            .map_err(|_| "Nie można zapisać listy urządzeń.".to_owned())? = disks
            .into_iter()
            .map(|disk| (disk.device.path.clone(), disk))
            .collect();
        return Ok(devices);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = state;
        list_block_devices()
            .map_err(|_| "Nie udało się odczytać listy urządzeń blokowych.".to_owned())
    }
}

/// Registers a file chosen through the desktop file picker. It is referenced in
/// place, not copied to a project directory, and remains inaccessible to the UI.
#[tauri::command]
fn register_desktop_artifact(
    path: String,
    kind: ArtifactKind,
    state: State<DesktopState>,
) -> Result<ArtifactReceipt, String> {
    let metadata =
        fs::metadata(&path).map_err(|_| "Nie można odczytać wybranego pliku.".to_owned())?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("Wybierz niepusty, zwykły plik.".to_owned());
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err("Plik przekracza limit 1 TiB.".to_owned());
    }

    let id = Uuid::new_v4();
    let receipt = ArtifactReceipt {
        id: id.to_string(),
        kind,
        byte_len: metadata.len(),
    };
    let artifact = DesktopArtifact {
        path: PathBuf::from(path),
        receipt: receipt.clone(),
    };
    state
        .artifacts
        .lock()
        .map_err(|_| "Nie można zapisać wyboru pliku.".to_owned())?
        .insert(id, artifact);
    Ok(receipt)
}

#[tauri::command]
fn plan_desktop_operation(
    mode: OperationMode,
    backup_id: String,
    keyset_id: Option<String>,
    boot0_id: Option<String>,
    boot1_id: Option<String>,
    target_path: String,
    state: State<DesktopState>,
) -> Result<OperationPlan, String> {
    resolve_desktop_plan(
        mode,
        backup_id,
        keyset_id,
        boot0_id,
        boot1_id,
        target_path,
        &state,
    )
    .map(|(plan, _, _, _, _)| plan)
}

#[tauri::command]
async fn preflight_desktop_operation(
    mode: OperationMode,
    backup_id: String,
    keyset_id: Option<String>,
    boot0_id: Option<String>,
    boot1_id: Option<String>,
    target_path: String,
    state: State<'_, DesktopState>,
) -> Result<PreflightReport, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (plan, backup_path, keyset_path, boot0_path, boot1_path) = resolve_desktop_plan(
            mode,
            backup_id,
            keyset_id,
            boot0_id,
            boot1_id,
            target_path,
            &state,
        )?;
        #[cfg(target_os = "windows")]
        {
            let selected = selected_windows_disk(&state, &plan.target.path)?;
            diagnostic(&format!(
                "restore preflight started: target={}; mode={mode:?}",
                selected.device.path
            ));
            return preflight_windows_nand_restore(
                &selected,
                plan,
                &backup_path,
                keyset_path.as_deref(),
                boot0_path.as_deref(),
                boot1_path.as_deref(),
            )
            .map(|result| {
                diagnostic("restore preflight completed");
                result.backup
            })
            .map_err(|error| {
                diagnostic(&format!("restore preflight failed: {error:?}"));
                "Preflight Windows odrzucił urządzenie lub pliki źródłowe. Szczegóły w logu."
                    .to_owned()
            });
        }
        #[cfg(not(target_os = "windows"))]
        preflight_nand_restore(
            plan,
            backup_path,
            keyset_path.as_deref(),
            boot0_path.as_deref(),
            boot1_path.as_deref(),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "Preflight został przerwany.".to_owned())?
}

#[tauri::command]
fn plan_desktop_in_place_operation(
    keyset_id: String,
    target_path: String,
    state: State<DesktopState>,
) -> Result<InPlaceExpansionPlan, String> {
    resolve_desktop_in_place_plan(keyset_id, target_path, &state).map(|(plan, _)| plan)
}

#[tauri::command]
async fn preflight_desktop_in_place_operation(
    keyset_id: String,
    target_path: String,
    state: State<'_, DesktopState>,
) -> Result<InPlaceExpansionPreflight, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (plan, keyset_path) = resolve_desktop_in_place_plan(keyset_id, target_path, &state)?;
        #[cfg(target_os = "windows")]
        {
            let selected = selected_windows_disk(&state, &plan.target.path)?;
            diagnostic(&format!(
                "in-place preflight started: target={}",
                selected.device.path
            ));
            let key = nandunx_core::parse_bis_keyset_file(&keyset_path)
                .map_err(|_| "Keyset nie przeszedł walidacji.".to_owned())?
                .user_key()
                .cloned()
                .ok_or("Keyset nie zawiera BIS Key 2 wymaganego dla USER.".to_owned())?;
            return preflight_windows_in_place_user_expansion(
                &selected,
                plan,
                &key,
                &[keyset_path],
            )
            .map(|report| {
                diagnostic("in-place preflight completed");
                report
            })
            .map_err(|error| {
                diagnostic(&format!("in-place preflight failed: {error}"));
                if let nandunx_core::InPlaceExpansionError::PlanningDetail(detail) = &error {
                    diagnostic(&format!("in-place planning details: {detail:?}"));
                }
                error.to_string()
            });
        }
        #[cfg(not(target_os = "windows"))]
        {
            let key = parse_bis_keyset_file(&keyset_path)
                .map_err(|_| "Keyset nie przeszedł walidacji.".to_owned())?
                .user_key()
                .cloned()
                .ok_or("Keyset nie zawiera BIS Key 2 wymaganego dla USER.".to_owned())?;
            preflight_in_place_user_expansion(plan, &key).map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|_| "Preflight został przerwany.".to_owned())?
}

#[tauri::command]
async fn start_desktop_restore(
    mode: OperationMode,
    backup_id: String,
    keyset_id: Option<String>,
    boot0_id: Option<String>,
    boot1_id: Option<String>,
    target_path: String,
    typed_confirmation: String,
    state: State<'_, DesktopState>,
) -> Result<OperationView, String> {
    let state = state.inner().clone();
    let reservation = state.operations.reserve()?;
    #[cfg(target_os = "windows")]
    {
        let selected = selected_windows_disk(&state, &target_path)?;
        return Arc::clone(&state.operations).start(
            reservation,
            nandunx_core::RestoreDevicePhase::CopyingPayload,
            move |observer| {
                let (plan, backup_path, keyset_path, _boot0_path, _boot1_path) =
                    resolve_desktop_plan(
                        mode,
                        backup_id,
                        keyset_id,
                        boot0_id,
                        boot1_id,
                        target_path,
                        &state,
                    )?;
                if plan.target != selected.device || plan.boot0.is_some() || plan.boot1.is_some() {
                    return Err(
                        "Plan zmienił się lub BOOT0/BOOT1 nie są obsługiwane przez writer.".into(),
                    );
                }
                diagnostic(&format!(
                    "restore authorization started: target={}; mode={mode:?}",
                    selected.device.path
                ));
                let session = authorize_windows_restore(
                    &selected,
                    mode,
                    &backup_path,
                    keyset_path.as_deref(),
                    &[],
                    &typed_confirmation,
                )
                .map_err(|error| {
                    diagnostic(&format!("restore authorization failed: {error}"));
                    if let nandunx_core::WindowsOperationError::Device(device) = &error {
                        diagnostic(&format!("restore device protection details: {device:?}"));
                    }
                    error.to_string()
                })?;
                diagnostic("restore authorization completed; writer starting");
                session
                    .execute(observer)
                    .map(OperationReport::Restore)
                    .map_err(|error| {
                        diagnostic(&format!("restore writer failed: {error}"));
                        if let nandunx_core::WindowsOperationError::PlanningDetail(detail) = &error
                        {
                            diagnostic(&format!("restore resize planning details: {detail:?}"));
                        }
                        format!("{error} Zweryfikuj nośnik przed ponowną próbą.")
                    })
            },
        );
    }
    #[cfg(not(target_os = "windows"))]
    tauri::async_runtime::spawn_blocking(move || {
        let (plan, backup_path, keyset_path, boot0_path, boot1_path) = resolve_desktop_plan(
            mode,
            backup_id,
            keyset_id,
            boot0_id,
            boot1_id,
            target_path,
            &state,
        )?;
        if plan.boot0.is_some() || plan.boot1.is_some() {
            return Err(
                "Writer nie przywraca jeszcze BOOT0/BOOT1; przywróć je osobno przez Hekate."
                    .to_owned(),
            );
        }
        preflight_nand_restore(
            plan.clone(),
            &backup_path,
            keyset_path.as_deref(),
            boot0_path.as_deref(),
            boot1_path.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let user_key = if mode == OperationMode::RestoreAndExpandUser {
            Some(desktop_user_key(keyset_path.as_deref().ok_or(
                "Rozszerzenie USER wymaga keysetu w bieżącej sesji.",
            )?)?)
        } else {
            None
        };
        let authorized =
            nandunx_core::authorize_restore_target(&plan, &backup_path, &typed_confirmation)
                .map_err(|error| error.to_string())?;
        state.operations.start(
            reservation,
            nandunx_core::RestoreDevicePhase::CopyingPayload,
            move |observer| {
                let report = match user_key.as_ref() {
                    Some(key) => nandunx_core::restore_and_expand_user_to_authorized_device(
                        &plan,
                        &authorized,
                        backup_path,
                        key,
                        observer,
                    ),
                    None => nandunx_core::restore_raw_nand_to_authorized_device(
                        &plan,
                        &authorized,
                        backup_path,
                        observer,
                    ),
                }
                .map_err(|error| error.to_string())?;
                Ok(OperationReport::Restore(report))
            },
        )
    })
    .await
    .map_err(|_| "Przygotowanie operacji zostało przerwane.".to_owned())?
}

#[tauri::command]
async fn start_desktop_in_place_expansion(
    keyset_id: String,
    target_path: String,
    typed_confirmation: String,
    state: State<'_, DesktopState>,
) -> Result<OperationView, String> {
    let state = state.inner().clone();
    let reservation = state.operations.reserve()?;
    #[cfg(target_os = "windows")]
    {
        let selected = selected_windows_disk(&state, &target_path)?;
        return Arc::clone(&state.operations).start(
            reservation,
            nandunx_core::RestoreDevicePhase::RelocatingUser,
            move |observer| {
                let (plan, keyset_path) =
                    resolve_desktop_in_place_plan(keyset_id, target_path, &state)?;
                if plan.target != selected.device {
                    return Err("Plan celu zmienił się od wyboru urządzenia.".into());
                }
                diagnostic(&format!(
                    "in-place authorization started: target={}",
                    selected.device.path
                ));
                let session =
                    authorize_windows_in_place(&selected, &keyset_path, &[], &typed_confirmation)
                        .map_err(|error| {
                        diagnostic(&format!("in-place authorization failed: {error}"));
                        if let nandunx_core::WindowsOperationError::Device(device) = &error {
                            diagnostic(&format!("in-place device protection details: {device:?}"));
                        }
                        error.to_string()
                    })?;
                diagnostic("in-place authorization completed; writer starting");
                session
                    .execute(observer)
                    .map(OperationReport::InPlace)
                    .map_err(|error| {
                        diagnostic(&format!("in-place writer failed: {error}"));
                        format!("{error} Zweryfikuj nośnik przed ponowną próbą.")
                    })
            },
        );
    }
    #[cfg(not(target_os = "windows"))]
    tauri::async_runtime::spawn_blocking(move || {
        let (plan, keyset_path) = resolve_desktop_in_place_plan(keyset_id, target_path, &state)?;
        let key = desktop_user_key(&keyset_path)?;
        preflight_in_place_user_expansion(plan.clone(), &key).map_err(|error| error.to_string())?;
        let authorized =
            nandunx_core::authorize_in_place_expansion_target(&plan, &typed_confirmation)
                .map_err(|error| error.to_string())?;
        state.operations.start(
            reservation,
            nandunx_core::RestoreDevicePhase::RelocatingUser,
            move |observer| {
                nandunx_core::expand_user_in_place_on_authorized_device(
                    &plan,
                    &authorized,
                    &key,
                    observer,
                )
                .map(OperationReport::InPlace)
                .map_err(|error| error.to_string())
            },
        )
    })
    .await
    .map_err(|_| "Przygotowanie operacji zostało przerwane.".to_owned())?
}

#[tauri::command]
fn desktop_operation_status(
    id: String,
    state: State<DesktopState>,
) -> Result<OperationView, String> {
    state.operations.view(&id, false)
}

#[tauri::command]
fn cancel_desktop_operation(
    id: String,
    state: State<DesktopState>,
) -> Result<OperationView, String> {
    state.operations.view(&id, true)
}

#[cfg(target_os = "windows")]
fn selected_windows_disk(state: &DesktopState, path: &str) -> Result<WindowsDiskSnapshot, String> {
    state
        .windows_disks
        .lock()
        .map_err(|_| "Nie można odczytać wyboru urządzenia.".to_owned())?
        .get(path)
        .cloned()
        .ok_or("Odśwież listę urządzeń i wybierz cel ponownie.".to_owned())
}

#[cfg(not(target_os = "windows"))]
fn desktop_user_key(path: &std::path::Path) -> Result<nandunx_core::BisKey, String> {
    parse_bis_keyset_file(path)
        .map_err(|_| "Keyset nie przeszedł ponownej walidacji.".to_owned())?
        .user_key()
        .cloned()
        .ok_or("Keyset nie zawiera BIS Key 2 wymaganego dla USER.".to_owned())
}

fn resolve_desktop_in_place_plan(
    keyset_id: String,
    target_path: String,
    state: &DesktopState,
) -> Result<(InPlaceExpansionPlan, PathBuf), String> {
    let keyset_id =
        Uuid::parse_str(&keyset_id).map_err(|_| "Nieprawidłowy wybór keysetu.".to_owned())?;
    let keyset_path = {
        let artifacts = state
            .artifacts
            .lock()
            .map_err(|_| "Nie można odczytać wyboru plików.".to_owned())?;
        let keyset = artifacts
            .get(&keyset_id)
            .ok_or("Keyset nie jest dostępny w tej sesji.".to_owned())?;
        if keyset.receipt.kind != ArtifactKind::Keyset || !keyset.path.is_file() {
            return Err("Keyset nie jest dostępny w tej sesji.".to_owned());
        }
        keyset.path.clone()
    };
    let target = resolve_desktop_target(state, &target_path)?;
    let plan = create_in_place_expansion_plan(target).map_err(|error| error.to_string())?;
    Ok((plan, keyset_path))
}

fn resolve_desktop_plan(
    mode: OperationMode,
    backup_id: String,
    keyset_id: Option<String>,
    boot0_id: Option<String>,
    boot1_id: Option<String>,
    target_path: String,
    state: &DesktopState,
) -> Result<
    (
        OperationPlan,
        PathBuf,
        Option<PathBuf>,
        Option<PathBuf>,
        Option<PathBuf>,
    ),
    String,
> {
    let backup_id =
        Uuid::parse_str(&backup_id).map_err(|_| "Nieprawidłowy wybór backupu.".to_owned())?;
    let keyset_id = keyset_id
        .map(|id| Uuid::parse_str(&id).map_err(|_| "Nieprawidłowy wybór keysetu.".to_owned()))
        .transpose()?;
    let boot0_id = boot0_id
        .map(|id| Uuid::parse_str(&id).map_err(|_| "Nieprawidłowy wybór BOOT0.".to_owned()))
        .transpose()?;
    let boot1_id = boot1_id
        .map(|id| Uuid::parse_str(&id).map_err(|_| "Nieprawidłowy wybór BOOT1.".to_owned()))
        .transpose()?;
    let (backup, keyset, backup_path, keyset_path, boot0, boot1, boot0_path, boot1_path) = {
        let artifacts = state
            .artifacts
            .lock()
            .map_err(|_| "Nie można odczytać wyboru plików.".to_owned())?;
        let backup = artifacts
            .get(&backup_id)
            .ok_or("Backup nie jest dostępny w tej sesji.")?;
        if !backup.path.is_file() {
            return Err("Wybrany plik nie jest już dostępny.".to_owned());
        }
        let keyset = match keyset_id {
            Some(keyset_id) => {
                let artifact = artifacts
                    .get(&keyset_id)
                    .ok_or("Keyset nie jest dostępny w tej sesji.")?;
                if !artifact.path.is_file() {
                    return Err("Wybrany plik nie jest już dostępny.".to_owned());
                }
                Some((artifact.receipt.clone(), artifact.path.clone()))
            }
            None => None,
        };
        let (keyset, keyset_path) = match keyset {
            Some((receipt, path)) => (Some(receipt), Some(path)),
            None => (None, None),
        };
        let resolve_boot =
            |id: Option<Uuid>, label: &str| -> Result<Option<(ArtifactReceipt, PathBuf)>, String> {
                id.map(|id| {
                    let artifact = artifacts
                        .get(&id)
                        .ok_or_else(|| format!("{label} nie jest dostępny w tej sesji."))?;
                    if !artifact.path.is_file() {
                        return Err(format!("Wybrany plik {label} nie jest już dostępny."));
                    }
                    Ok((artifact.receipt.clone(), artifact.path.clone()))
                })
                .transpose()
            };
        let boot0 = resolve_boot(boot0_id, "BOOT0")?;
        let boot1 = resolve_boot(boot1_id, "BOOT1")?;
        let (boot0, boot0_path) =
            boot0.map_or((None, None), |(receipt, path)| (Some(receipt), Some(path)));
        let (boot1, boot1_path) =
            boot1.map_or((None, None), |(receipt, path)| (Some(receipt), Some(path)));
        (
            backup.receipt.clone(),
            keyset,
            backup.path.clone(),
            keyset_path,
            boot0,
            boot1,
            boot0_path,
            boot1_path,
        )
    };
    let target = resolve_desktop_target(state, &target_path)?;

    let plan = create_operation_plan(mode, backup, keyset, boot0, boot1, target)
        .map_err(|_| "Plan nie przeszedł walidacji bezpieczeństwa.".to_owned())?;
    Ok((plan, backup_path, keyset_path, boot0_path, boot1_path))
}

fn resolve_desktop_target(
    state: &DesktopState,
    path: &str,
) -> Result<nandunx_core::BlockDevice, String> {
    #[cfg(target_os = "windows")]
    return selected_windows_disk(state, path).map(|snapshot| snapshot.device);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = state;
        list_block_devices()
            .map_err(|_| "Nie udało się odczytać listy urządzeń blokowych.".to_owned())?
            .into_iter()
            .find(|device| device.path == path)
            .ok_or("Wybrane urządzenie nie jest dostępne.".to_owned())
    }
}

pub fn run() {
    #[cfg(target_os = "windows")]
    {
        match init_diagnostics() {
            Ok(path) => diagnostic(&format!("desktop diagnostic file: {}", path.display())),
            Err(error) => eprintln!("NANDuNX: cannot open diagnostic log: {error}"),
        }
        std::panic::set_hook(Box::new(|info| {
            // Panic payloads can contain application data. Record the call
            // site and stack without printing that payload.
            diagnostic(&format!(
                "panic at {:?}; stack:\n{}",
                info.location(),
                std::backtrace::Backtrace::force_capture()
            ));
        }));
    }
    tauri::Builder::default()
        .manage(DesktopState::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            #[cfg(target_os = "windows")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    match window_vibrancy::apply_mica(&window, Some(true)) {
                        Ok(()) => {
                            app.state::<DesktopState>()
                                .mica_enabled
                                .store(true, Ordering::Relaxed);
                            diagnostic("Mica applied to main window; code=none");
                        }
                        Err(error) => {
                            diagnostic(&format!(
                                "Mica unavailable; stage=window setup; {error}; code={}",
                                match &error {
                                    window_vibrancy::Error::Win32Error { result, .. } =>
                                        format!("0x{result:x}"),
                                    _ => "none".to_owned(),
                                }
                            ));
                        }
                    }
                } else {
                    diagnostic(
                        "Mica unavailable; stage=window setup; main window missing; code=none",
                    );
                }
            }
            #[cfg(not(target_os = "windows"))]
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            list_desktop_block_devices,
            register_desktop_artifact,
            plan_desktop_operation,
            preflight_desktop_operation,
            plan_desktop_in_place_operation,
            preflight_desktop_in_place_operation,
            start_desktop_restore,
            start_desktop_in_place_expansion,
            desktop_operation_status,
            cancel_desktop_operation
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.state::<DesktopState>().operations.is_active() {
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running NANDuNX desktop application");
}

#[cfg(all(test, feature = "custom-protocol"))]
mod bundle_tests {
    #[test]
    fn standalone_build_embeds_the_frontend() {
        assert!(!tauri::is_dev());
        let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let index = tauri::utils::assets::AssetKey::from("index.html");
        assert!(context.assets().get(&index).is_some());
    }
}
