// Model lifecycle + environment commands.
// Handles load, unload, stop, status lifecycle events, and delegates cache/preflight to submodules.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use unlocr::server::{free_port, RemoteEndpoint, Server};
use unlocr::{OcrOptions, Progress};

use crate::state::{AppState, Backend, LoadedModel};

mod cache;
pub(crate) use cache::{
    clear_model_cache, get_cache_info, list_available_quants, list_cached_files, list_local_models,
    preflight, remove_cached_file,
};
mod sysreq;
pub(crate) use sysreq::system_requirements;

/// Status payload for `model_status` / `load_model` returns.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelStatus {
    pub loaded: bool,
    /// "local" | "remote" | "" when nothing is loaded.
    pub mode: String,
    /// Display label ("Unlimited-OCR-2 Q8_0" / the remote URL), empty when unloaded.
    pub label: String,
}

/// Payload for the `ocr://progress` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    name: String,
    pct: u8,
    done: u64,
    total: u64,
}

/// Payload for the `ocr://server-ready` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReady {
    port: u16,
}

/// Payload for the `ocr://status` event: a free-form one-line message surfacing
/// what a long, otherwise event-less phase is doing (model load into RAM,
/// rasterization) so the UI does not look frozen.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusMsg {
    message: String,
}

/// Load a model so it stays warm in RAM (litellm-style), gating Run OCR until it
/// succeeds.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn load_model(
    app: AppHandle,
    mode: String,
    quant: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    llama_bin: Option<String>,
    image_max_tokens: Option<u32>,
    chat_template: Option<String>,
    model_file: Option<String>,
    mmproj_file: Option<String>,
) -> Result<ModelStatus, String> {
    let llama_override = llama_bin
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let model_override = model_file
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let mmproj_override = mmproj_file
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    if mode != "remote" && mmproj_override.is_some() && model_override.is_none() {
        return Err("mmproj_file requires model_file".to_string());
    }
    let chat_template = chat_template.filter(|s| !s.trim().is_empty());
    if image_max_tokens == Some(0) {
        return Err("image_max_tokens must be greater than 0".to_string());
    }

    let res = tauri::async_runtime::spawn_blocking(move || -> Result<ModelStatus, String> {
        {
            let state = app.state::<AppState>();
            let mut g = state.model.lock().unwrap_or_else(|p| p.into_inner());
            *g = None;
        }

        let (backend, label, mode_out, pdftoppm) = match mode.as_str() {
            "remote" => {
                let base = base_url.unwrap_or_default().trim().to_string();
                if base.is_empty() {
                    return Err("remote mode requires a base URL".into());
                }
                let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
                eprintln!(
                    "[load_model] remote mode requires Unlimited-OCR-2 support on the server. \
                     GGUF requires the patched llama-server; gateways (litellm/vLLM) need \
                     a model name set."
                );
                let mut real_key = api_key.filter(|k| !k.trim().is_empty());
                if let Some(ref k) = real_key {
                    if k == "••••••••" || k == "__saved__" {
                        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
                            if let Ok(key) = entry.get_password() {
                                real_key = Some(key);
                            } else {
                                real_key = None;
                            }
                        } else {
                            real_key = None;
                        }
                    }
                }
                let ep = RemoteEndpoint {
                    base_url: base.clone(),
                    api_key: real_key,
                    model: model.filter(|m| !m.trim().is_empty()),
                };
                if let Err(e) = ep.probe() {
                    eprintln!("[load_model] remote probe failed (continuing): {e}");
                }
                (Backend::Remote(ep), base, "remote".to_string(), pdftoppm)
            }
            _ => {
                // Build the download-progress sink FIRST: preflight::check now
                // auto-downloads unlocr's managed llama-server and emits
                // Progress::Download for it, so the same sink surfaces both the
                // llama-server and the model download to the model bar.
                let app_dl = app.clone();
                let mut on_progress = |p: Progress| {
                    if let Progress::Download {
                        name,
                        pct,
                        done,
                        total,
                    } = p
                    {
                        let _ = app_dl.emit(
                            "ocr://progress",
                            DownloadProgress {
                                name,
                                pct,
                                done,
                                total,
                            },
                        );
                    }
                };

                let tools = unlocr::preflight::check(llama_override.as_deref(), &mut on_progress)
                    .map_err(|e| e.to_string())?;
                let quant = quant
                    .filter(|q| !q.trim().is_empty())
                    .unwrap_or_else(|| OcrOptions::default().quant);
                unlocr::model::validate_quant(&quant).map_err(|e| e.to_string())?;
                let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;

                let files = unlocr::model::ensure_with_overrides(
                    &cache,
                    &quant,
                    model_override.as_deref(),
                    mmproj_override.as_deref(),
                    &mut on_progress,
                )
                .map_err(|e| e.to_string())?;

                let port = free_port().map_err(|e| e.to_string())?;
                // Server::start blocks in await_health while llama-server loads the
                // multi-GB GGUF into RAM, emitting nothing. Surface a status so the
                // model bar does not sit frozen on "downloading 100%"/"loading…".
                let _ = app.emit(
                    "ocr://status",
                    StatusMsg {
                        message: "loading model into memory (can take a minute)…".to_string(),
                    },
                );
                let srv = Server::start(
                    &tools.llama_server,
                    &files.model,
                    &files.mmproj,
                    port,
                    image_max_tokens,
                    chat_template.as_deref(),
                )
                .map_err(|e| e.to_string())?;

                let _ = app.emit("ocr://server-ready", ServerReady { port });
                let label = match model_override.as_deref() {
                    Some(p) => p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "custom model".to_string()),
                    None => format!("Unlimited-OCR-2 {quant}"),
                };
                (
                    Backend::Local(srv),
                    label,
                    "local".to_string(),
                    tools.pdftoppm,
                )
            }
        };

        let state = app.state::<AppState>();
        if let Ok(mut g) = state.model.lock() {
            *g = Some(LoadedModel {
                backend,
                label: label.clone(),
                mode: mode_out.clone(),
            });
        }
        if let Ok(mut p) = state.pdftoppm.lock() {
            *p = Some(pdftoppm);
        }
        // Stamp last_used INSIDE the worker, right after the model is installed, so
        // the 60s idle-unload watcher can never observe a stale (pre-load)
        // timestamp in the window between install and the stamp and unload a model
        // that was just loaded.
        *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now());

        Ok(ModelStatus {
            loaded: true,
            mode: mode_out,
            label,
        })
    })
    .await;
    res.map_err(|e| format!("load worker join failed: {e}"))?
}

/// Unload the currently-held model.
#[tauri::command]
pub(crate) fn unload_model(state: State<'_, AppState>) -> ModelStatus {
    // Dropping the Server handle IS the kill: `Server::Drop` kills+waits the
    // owned Child (identity-safe). Stop only cancels the active HTTP request;
    // explicitly unloading the model is what terminates the server.
    let mut g = state.model.lock().unwrap_or_else(|p| p.into_inner());
    *g = None;
    // Clear last_used so a later load is not judged against a stale timestamp by
    // the idle-unload watcher (the watcher clears it on its own unload; an
    // explicit unload must too).
    *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) = None;
    ModelStatus {
        loaded: false,
        mode: String::new(),
        label: String::new(),
    }
}

/// Cancel the active HTTP/SSE request without unloading the model. The OCR client
/// polls this flag even while waiting for headers/tokens and drops the response;
/// llama-server treats the disconnect as cancellation of that generation task.
/// No model lock here: the worker holds it throughout OCR.
#[tauri::command]
pub(crate) fn stop_ocr(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// Current load state for the titlebar badge + the Run OCR gate.
#[tauri::command]
pub(crate) fn model_status(state: State<'_, AppState>) -> ModelStatus {
    let g = state.model.lock().unwrap_or_else(|p| p.into_inner());
    match g.as_ref().map(|lm| (lm.mode.clone(), lm.label.clone())) {
        Some((mode, label)) => ModelStatus {
            loaded: true,
            mode,
            label,
        },
        None => ModelStatus {
            loaded: false,
            mode: String::new(),
            label: String::new(),
        },
    }
}
