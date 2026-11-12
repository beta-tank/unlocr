use serde::Serialize;

/// Serializable payload for the `ocr://page` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageProgress {
    pub(crate) page: usize,
    pub(crate) total: usize,
}

/// Serializable payload for the `ocr://rasterizing` event. `total` is `None`
/// when the run's page count wasn't known upfront (whole-doc run with no
/// resolvable `pdfinfo`); the frontend shows a running count with no
/// denominator in that case.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RasterizeProgress {
    pub(crate) page: usize,
    pub(crate) total: Option<usize>,
}

/// Payload for the `ocr://partial-text` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialText {
    pub(crate) page: usize,
    pub(crate) chunk: String,
}

/// Payload for the `ocr://page-truncated` event: the page's generation hit
/// `max_tokens` without a natural stop (see `unlocr::Progress::Truncated`).
/// `is_local` lets the frontend show the GGUF-quant repetition-loop framing +
/// remedies only for the local backend; a remote/full-precision endpoint
/// hitting the same signal is more likely just a legitimately dense page.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Truncated {
    pub(crate) page: usize,
    pub(crate) is_local: bool,
}

/// Payload for the terminal `ocr://done` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OcrDone {
    pub(crate) markdown: String,
}

/// Payload for `ocr://images-kept`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImagesKept {
    pub(crate) dir: String,
}

/// Payload for `ocr://status`: a one-line message for a long, event-less phase.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusMsg {
    pub(crate) message: String,
}

/// Return value of `run_ocr`: every written file path (combined file first in
/// single/both; first page file in pages) plus the in-memory combined markdown.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunResult {
    pub(crate) paths: Vec<String>,
    pub(crate) combined: String,
    pub(crate) effective_config: serde_json::Value,
}

/// Flatten the applied load snapshot with the per-run options. This payload can
/// be persisted by the history frontend without re-reading mutable controls.
pub(crate) fn effective_run_config(
    lm: &crate::state::LoadedModel,
    opts: &unlocr::OcrOptions,
    output_mode: unlocr::OutputMode,
    repetition_profile: &str,
    result_format: &str,
) -> Result<serde_json::Value, String> {
    let mut config = serde_json::to_value(&lm.effective_config).map_err(|e| e.to_string())?;
    config["processingMode"] = "pages".into();
    config["maxTokens"] = opts.max_tokens.into();
    config["dpi"] = opts.dpi.into();
    config["prompt"] = opts.prompt.clone().into();
    config["keepImages"] = opts.keep_images.into();
    config["pages"] = serde_json::json!(opts.pages);
    config["temperature"] = serde_json::json!(opts.temperature.unwrap_or(0.0));
    config["repetitionProfile"] = repetition_profile.into();
    config["repeatPenalty"] = serde_json::json!(opts.repeat_penalty);
    config["dryMultiplier"] = serde_json::json!(opts.dry_multiplier);
    config["dryBase"] = serde_json::json!(opts.dry_base);
    config["dryAllowedLength"] = serde_json::json!(opts.dry_allowed_length);
    config["dryPenaltyLastN"] = serde_json::json!(opts.dry_penalty_last_n);
    // Already accepted by parse_result_format before the worker starts. Keep
    // the wire value independent of the core enum's Rust variant names.
    config["resultFormat"] = result_format.trim().to_ascii_lowercase().into();
    config["outputMode"] = match output_mode {
        unlocr::OutputMode::Single => "single",
        unlocr::OutputMode::Pages => "pages",
        unlocr::OutputMode::Both => "both",
    }
    .into();
    Ok(config)
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_snapshot_uses_loaded_connection_and_resolved_sampling() {
        use crate::state::{Backend, EffectiveModelConfig, LoadedModel};
        let lm = LoadedModel {
            backend: Backend::Remote(unlocr::server::RemoteEndpoint {
                base_url: "https://actual.example".into(),
                api_key: Some("never-serialize-this".into()),
                model: Some("served-name".into()),
            }),
            label: "remote".into(),
            mode: "remote".into(),
            effective_config: EffectiveModelConfig {
                mode: "remote".into(),
                base_url: Some("https://actual.example".into()),
                model: Some("served-name".into()),
                ..Default::default()
            },
        };
        let mut opts = unlocr::OcrOptions {
            quant: "stale-form-quant".into(),
            repeat_penalty: Some(2.0),
            dry_multiplier: Some(9.0),
            ..Default::default()
        };
        super::super::validation::apply_repetition_profile(&mut opts, false, Some("manual"))
            .unwrap();
        let config = super::effective_run_config(
            &lm,
            &opts,
            unlocr::OutputMode::Single,
            "default",
            "markdown",
        )
        .unwrap();
        assert_eq!(config["model"], "served-name");
        assert_eq!(config["baseUrl"], "https://actual.example");
        assert!(config["quant"].is_null());
        assert!(config["repeatPenalty"].is_null());
        assert!(config["dryMultiplier"].is_null());
        assert_eq!(config["temperature"], 0.0);
        assert_eq!(config["processingMode"], "pages");
        let json = config.to_string();
        assert!(!json.contains("never-serialize-this"));
        assert!(!json.contains("stale-form-quant"));
    }
}
