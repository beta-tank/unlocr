// System Requirements Tauri command.
// Probes CPU, RAM, GPU, and disk space then rates each against known thresholds.
// The frontend renders the result directly (no decision logic in JS).
//
// Returns the library's serializable `SystemInfo` as-is. Its `Status` enum is
// `#[serde(rename_all = "lowercase")]` ("good"/"marginal"/"insufficient"/
// "unknown") and the struct is camelCase on the wire, which is exactly what
// settings.js reads (metrics[].status, verdict, verdictLabel). No parallel DTO
// or status_str mapping to keep in sync with the lib.

use unlocr::preflight::sysreq::SystemInfo;

#[tauri::command]
pub(crate) async fn system_requirements() -> Result<SystemInfo, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        Ok(unlocr::preflight::sysreq::check_system_requirements(&cache))
    })
    .await
    .map_err(|e| format!("system requirements worker join failed: {e}"))?
}
