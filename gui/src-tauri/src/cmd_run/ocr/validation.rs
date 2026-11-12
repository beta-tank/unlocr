use std::path::PathBuf;
use unlocr::{OcrOptions, OutputMode};

/// Validates the OCR arguments, sets up the default options, duplicate stem checks,
/// and determines the output mode.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_and_prepare_options(
    inputs: &[String],
    out_file: Option<&str>,
    max_tokens: Option<u32>,
    dpi: Option<u32>,
    prompt: Option<String>,
    keep_images: Option<bool>,
    repeat_penalty: Option<f32>,
    dry_multiplier: Option<f32>,
    dry_base: Option<f32>,
    dry_allowed_length: Option<u32>,
    dry_penalty_last_n: Option<i32>,
    temperature: Option<f32>,
    first_page: Option<u32>,
    last_page: Option<u32>,
    output_mode: Option<&str>,
    passwords: Option<Vec<String>>,
    result_format: Option<&str>,
) -> Result<(OcrOptions, OutputMode), String> {
    let mut opts = OcrOptions::default();
    if let Some(t) = max_tokens {
        opts.max_tokens = t;
    }
    if let Some(d) = dpi {
        opts.dpi = d;
    }
    if let Some(p) = prompt {
        opts.prompt = p;
    }
    if let Some(k) = keep_images {
        opts.keep_images = k;
    }
    opts.result_format = unlocr::parse_result_format(result_format.unwrap_or("markdown"))
        .map_err(|e| e.to_string())?;
    opts.temperature = temperature;
    opts.pages = match (first_page, last_page) {
        (None, None) => None,
        (f, l) => Some((f.unwrap_or(1), l)),
    };
    // Candidate PDF passwords (session-only, never persisted). Empty = none, which
    // pdf::select_password treats as "no password". No numeric guard needed.
    opts.passwords = passwords.unwrap_or_default();

    // Single shared numeric/range guard: the same `OcrOptions::validate` the CLI
    // runs in main.rs, so the checks (and their wording) live in one place
    // rather than drifting between the two front ends. `image_max_tokens` is
    // None here (it is a load-time flag, validated in `load_model`); validate()
    // covers dpi / max_tokens / repeat_penalty and the page range.
    opts.validate().map_err(|e| e.to_string())?;
    // Sampling is validated after the loaded backend/profile has resolved it.
    // Stale manual controls must not reject a default-profile or remote run.
    opts.repeat_penalty = repeat_penalty;
    opts.dry_multiplier = dry_multiplier;
    opts.dry_base = dry_base;
    opts.dry_allowed_length = dry_allowed_length;
    opts.dry_penalty_last_n = dry_penalty_last_n;

    if out_file.is_some() && inputs.len() > 1 {
        return Err("out_file names a single file; clear it for multiple inputs".to_string());
    }

    // Resolve the on-disk layout once (single/pages/both); Copy, so the move
    // closure captures it by value. Unknown string -> error before any spawn.
    let mode =
        unlocr::parse_output_mode(output_mode.unwrap_or("single")).map_err(|e| e.to_string())?;

    // Pages/Both name the output folder after the input stem; out_file is ignored
    // there. Warn (parity with the CLI's ocr::run_pdf) so a set filename that has
    // no effect is surfaced rather than silently dropped.
    if out_file.is_some() && matches!(mode, OutputMode::Pages | OutputMode::Both) {
        eprintln!(
            "[run_ocr] warning: out_file is ignored in pages/both mode; the folder uses the input stem"
        );
    }
    // Same-stem inputs collide on the shared out dir: the `{stem}.md` file or the
    // `{stem}/` pages folder. A later input silently overwrites an earlier one, so
    // warn before running.
    for stem in unlocr::duplicate_stems(&inputs.iter().map(PathBuf::from).collect::<Vec<_>>()) {
        eprintln!(
            "[run_ocr] warning: multiple inputs share the stem '{stem}'; their outputs overwrite each other"
        );
    }

    #[cfg(debug_assertions)]
    eprintln!(
        "[run_ocr] effective opts: dpi={} max_tokens={} keep_images={}",
        opts.dpi, opts.max_tokens, opts.keep_images
    );

    Ok((opts, mode))
}

pub(crate) fn apply_repetition_profile(
    opts: &mut OcrOptions,
    local: bool,
    profile: Option<&str>,
) -> Result<String, String> {
    let profile = profile.unwrap_or("default");
    if !matches!(profile, "default" | "enhanced" | "manual") {
        return Err("repetition_profile must be default, enhanced, or manual".into());
    }
    if !local || profile != "manual" {
        opts.repeat_penalty = None;
        opts.dry_multiplier = None;
        opts.dry_base = None;
        opts.dry_allowed_length = None;
        opts.dry_penalty_last_n = None;
    }
    if local {
        opts.repeat_penalty.get_or_insert(1.3);
        opts.dry_multiplier.get_or_insert(1.0);
        // Materialize the actual request-builder fallback in the run snapshot.
        opts.dry_allowed_length.get_or_insert(4);
        if profile == "enhanced" {
            opts.dry_allowed_length = Some(2);
            opts.dry_penalty_last_n = Some(unlocr::server::LOCAL_CONTEXT_SIZE);
        }
        // Old manual profiles use the CLI's -1 alias. Record the same positive
        // window that the HTTP builder sends, including for restored settings.
        if opts.dry_penalty_last_n == Some(-1) {
            opts.dry_penalty_last_n = Some(unlocr::server::LOCAL_CONTEXT_SIZE);
        }
    }
    opts.validate().map_err(|e| e.to_string())?;
    Ok(if local { profile } else { "default" }.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stale_manual() -> OcrOptions {
        OcrOptions {
            repeat_penalty: Some(f32::NAN),
            dry_multiplier: Some(-5.0),
            dry_base: Some(0.0),
            dry_allowed_length: Some(0),
            dry_penalty_last_n: Some(-9),
            ..Default::default()
        }
    }

    #[test]
    fn default_and_enhanced_ignore_stale_manual_values() {
        let mut opts = stale_manual();
        assert_eq!(
            apply_repetition_profile(&mut opts, true, None).unwrap(),
            "default"
        );
        assert_eq!(opts.repeat_penalty, Some(1.3));
        assert_eq!(opts.dry_multiplier, Some(1.0));
        assert_eq!(opts.dry_base, None);
        assert_eq!(opts.dry_allowed_length, Some(4));
        assert_eq!(opts.dry_penalty_last_n, None);
        apply_repetition_profile(&mut opts, true, Some("enhanced")).unwrap();
        assert_eq!(opts.dry_allowed_length, Some(2));
        assert_eq!(
            opts.dry_penalty_last_n,
            Some(unlocr::server::LOCAL_CONTEXT_SIZE)
        );
    }

    #[test]
    fn manual_validates_overrides_and_remote_strips_them() {
        assert!(apply_repetition_profile(&mut stale_manual(), true, Some("manual")).is_err());
        let mut opts = OcrOptions {
            repeat_penalty: Some(1.0),
            dry_multiplier: Some(0.0),
            ..Default::default()
        };
        apply_repetition_profile(&mut opts, true, Some("manual")).unwrap();
        assert_eq!(opts.repeat_penalty, Some(1.0));
        assert_eq!(opts.dry_multiplier, Some(0.0));
        let mut opts = stale_manual();
        assert_eq!(
            apply_repetition_profile(&mut opts, false, Some("enhanced")).unwrap(),
            "default"
        );
        assert_eq!(opts.repeat_penalty, None);
        assert_eq!(opts.dry_multiplier, None);
        assert_eq!(opts.dry_allowed_length, None);
        assert_eq!(opts.dry_base, None);
        assert_eq!(opts.dry_penalty_last_n, None);
    }

    #[test]
    fn legacy_manual_window_is_resolved_for_effective_history() {
        let mut opts = OcrOptions {
            dry_penalty_last_n: Some(-1),
            ..Default::default()
        };
        apply_repetition_profile(&mut opts, true, Some("manual")).unwrap();
        assert_eq!(
            opts.dry_penalty_last_n,
            Some(unlocr::server::LOCAL_CONTEXT_SIZE)
        );
        for window in [0, 128] {
            opts.dry_penalty_last_n = Some(window);
            apply_repetition_profile(&mut opts, true, Some("manual")).unwrap();
            assert_eq!(opts.dry_penalty_last_n, Some(window));
        }
    }
}
