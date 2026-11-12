// Run controls and the applied load-time snapshot are deliberately separate:
// editing a connection never changes the backend used by the next run.

// Task preset -> the user prompt actually sent. Unlimited-OCR uses NO system prompt;
// the backend wraps the image as `<PAGE>` + image + this task instruction.
// `<|grounding|>` lives only on the grounding
// preset (markdown + layout coordinates); other presets use backend cleanup of
// page/layout markers, since OCR 2 can emit them regardless of the instruction.
// `Free OCR.` is a legacy task instruction, not a documented OCR 2 no-tags guarantee.
// The Prompt box overrides this when non-empty. Keep in sync
// with the CLI's Task::prompt() (src/cli_args.rs).
export const TASK_PROMPTS = {
  markdown: "Multi page parsing.",
  grounding: "<|grounding|>Convert the document to markdown.",
  free: "Free OCR.",
  figure: "Parse the figure.",
};

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the range "to" page number in
// readPageSelection(). TASK_PROMPTS above are model-instruction payloads and stay
// canonical English on purpose (they are sent to the model, not shown to the user).
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);
const control = (id) => document.getElementById(id);
const value = (id) => (control(id)?.value || "").trim();
let modelStatus = null;
let acceptedLoadOptions = null;

/** Shared load payload: only expert vision uses the numeric override; stock
 *  Unlimited-OCR-2 always uses its embedded template. No connection prefills. */
export function readLoadOptions() {
  const mode = value("enginePreset") === "custom" ? "remote" : "local";
  const path = (id) => control(id)?.dataset?.path || null;
  const modelFile = mode === "local" ? path("modelFilePath") : null;
  const visionMode = mode === "local" ? value("optVisionMode") || "auto" : null;
  return {
    mode,
    quant: mode === "local" ? value("optQuant") || "Q8_0" : null,
    baseUrl: mode === "remote" ? value("remoteUrl") || null : null,
    apiKey: mode === "remote" ? value("remoteKey") || null : null,
    model: mode === "remote" ? value("remoteModel") || null : null,
    llamaBin: mode === "local" ? value("setLlamaBin") || null : null,
    modelFile,
    mmprojFile: mode === "local" ? path("mmprojFilePath") : null,
    visionMode,
    imageMaxTokens: visionMode === "base" ? 273
      : visionMode === "expert" ? numOr(control("optImageMaxTokens"), null) : null,
    chatTemplate: modelFile ? value("optChatTemplate") || null : null,
  };
}

/** Called for successful loads and status refreshes. Unknown status is distinct
 *  from unloaded: a failed poll must not fabricate a successful unload. */
export function acceptModelStatus(status, loadOptions = null) {
  if (!status || typeof status.loaded !== "boolean") return;
  const wasLoaded = modelStatus?.loaded;
  modelStatus = { ...status, effectiveConfig: status.effectiveConfig ? { ...status.effectiveConfig } : null };
  if (!status.loaded) acceptedLoadOptions = null;
  else if (loadOptions) acceptedLoadOptions = { ...loadOptions };
  else if (!wasLoaded) acceptedLoadOptions = null;
  renderEffectiveSummary();
}

/** Canonical display URL, matching Rust's public_endpoint_url. Never show keys. */
export function publicEndpoint(raw) {
  try {
    const url = new URL(raw);
    url.username = "";
    url.password = "";
    url.search = "";
    url.hash = "";
    return url.toString().replace(/\/$/, "");
  } catch { return ""; }
}

/** Compare only load-sensitive fields relevant to the selected backend. */
export function hasPendingLoadChanges(selected, applied, accepted = null) {
  if (!applied) return false;
  if (selected.mode !== applied.mode) return true;
  const same = (a, b) => (a ?? null) === (b ?? null);
  if (selected.mode === "remote") {
    return publicEndpoint(selected.baseUrl) !== publicEndpoint(applied.baseUrl)
      || !same(selected.model, applied.model)
      || !!(accepted && (!same(selected.apiKey, accepted.apiKey) || !same(selected.baseUrl, accepted.baseUrl)));
  }
  const custom = !!selected.modelFile;
  return custom !== !applied.quant
    || (custom ? !same(selected.modelFile, applied.modelFile) : selected.quant !== applied.quant)
    || !same(selected.visionMode, applied.visionMode || "auto")
    || !same(selected.imageMaxTokens, applied.imageMaxTokens)
    || !same(selected.chatTemplate, applied.chatTemplate)
    || (Object.prototype.hasOwnProperty.call(applied, "mmprojOverride")
      ? !same(selected.mmprojFile, applied.mmprojOverride)
      : !!(selected.mmprojFile && !same(selected.mmprojFile, applied.mmprojFile)))
    || (Object.prototype.hasOwnProperty.call(applied, "llamaBinOverride")
      ? !same(selected.llamaBin, applied.llamaBinOverride)
      : !!(accepted && !same(selected.llamaBin, accepted.llamaBin)))
    || !!(accepted && !same(selected.mmprojFile, accepted.mmprojFile));
}

/** Settings restore can call this without generating autosave events. */
export function applyOptionVisibility(skipDpiProfile = false) {
  const selected = readLoadOptions();
  const remote = selected.mode === "remote";
  const hide = (id, hidden) => { if (control(id)) control(id).hidden = hidden; };
  hide("visionField", remote);
  hide("visionExpertField", remote || selected.visionMode !== "expert");
  hide("templateExpertField", remote || !selected.modelFile);
  hide("embeddedTemplate", remote || !!selected.modelFile);
  if (control("optQuant")) control("optQuant").disabled = !!selected.modelFile;
  // Run options follow the *loaded* backend even while another mode is selected.
  const runRemote = modelStatus?.loaded ? modelStatus.mode === "remote" : remote;
  hide("repetitionField", remote || runRemote);
  hide("repetitionExpert", remote || runRemote || value("optRepetitionProfile") !== "manual");
  const dpi = control("optDpi");
  const profile = control("optDpiProfile");
  if (!skipDpiProfile && profile && dpi) profile.value = ["144", "200", "300"].includes(dpi.value) ? dpi.value : "custom";
}

/** PDF rasterization does not apply when every selected input is an image. */
export function applyInputPaths(paths) {
  const imageOnly = paths.length > 0 && paths.every((p) => /\.(png|jpe?g|webp|bmp|tiff?)$/i.test(p));
  ["optDpi", "optDpiProfile"].forEach((id) => { if (control(id)) control(id).disabled = imageOnly; });
  if (control("dpiImageHint")) control("dpiImageHint").hidden = !imageOnly;
}

/** Parse a positive integer from a control, falling back when blank/invalid.
 *  Shared by readRunOptions() and settings.js's auto-save (so both agree on
 *  what counts as a valid override). */
export const numOr = (el, fallback) => {
  const v = parseInt((el && el.value) || "", 10);
  return Number.isFinite(v) && v > 0 ? v : fallback;
};

/** Optional float; blank/invalid -> null so the backend omits it (server default). */
export const floatOrNull = (el) => {
  const v = parseFloat((el && el.value) || "");
  return Number.isFinite(v) && v > 0 ? v : null;
};

/** Same, but 0 is a real value (DRY: an explicit 0 means "off" and must reach
 *  the backend, or the local-path 1.0 default would override it). */
export const floatOrNullMin0 = (el) => {
  const v = parseFloat((el && el.value) || "");
  return Number.isFinite(v) && v >= 0 ? v : null;
};

/** Integer >= -1 (-1 is a client alias for the managed context, 0 = off, >0 = window),
 *  else the fallback. The backend's OcrOptions::validate rejects < -1. */
export const intMinNeg1 = (el, fallback) => {
  const v = parseInt((el && el.value) || "", 10);
  return Number.isFinite(v) && v >= -1 ? v : fallback;
};

/** Assign `document.getElementById(id).value = v`, but only when both the
 *  element exists and `v` isn't null/undefined (preserves a legitimate 0/false
 *  rather than skipping it). Shared by every settings restore/save-feedback
 *  site (settings.js, quick_settings.js) so the guard can't drift between them. */
export const setVal = (id, v) => {
  const el = document.getElementById(id);
  if (el != null && v != null) el.value = v;
};

/** Read the engine/options controls (EH-0005 bites 1 + 2) into the run_ocr invoke
 *  payload. Every control defaults to unlocr::OcrOptions::default() in the markup
 *  (quant=Q8_0, dpi=144, max_tokens=8000, keep_images=false), so a user who touches
 *  nothing sends CLI-parity values. Invalid/empty numbers fall back to the same
 *  defaults so a half-typed field never crashes a run. The Prompt box is an OPTIONAL
 *  override: when empty it falls back to the selected Task preset, so a run never
 *  sends a blank prompt. Keys are camelCase to match the Tauri command's parameter
 *  names. */
export function readRunOptions() {
  const quantEl = document.getElementById("optQuant");
  const dpiEl = document.getElementById("optDpi");
  const maxTokensEl = document.getElementById("optMaxTokens");
  const keepImagesEl = document.getElementById("optKeepImages");
  const promptEl = document.getElementById("optPrompt");
  const taskEl = document.getElementById("optTask");
  const temperatureEl = document.getElementById("optTemperature");
  const repeatPenaltyEl = document.getElementById("optRepeatPenalty");
  const dryMultiplierEl = document.getElementById("optDryMultiplier");
  const dryBaseEl = document.getElementById("optDryBase");
  const antiLoopEl = document.getElementById("optAntiLoop");
  const dryAllowedLengthEl = document.getElementById("optDryAllowedLength");
  const dryPenaltyLastNEl = document.getElementById("optDryPenaltyLastN");

  const DEFAULT_DPI = 144;
  const DEFAULT_MAX_TOKENS = 8000;
  // Empty Prompt box -> the selected Task preset (markdown if the select is missing).
  const taskPrompt = TASK_PROMPTS[taskEl && taskEl.value] || TASK_PROMPTS.markdown;
  const promptOr = (el, fallback) => {
    const v = (el && el.value) || "";
    // trim only newlines/whitespace at the ends; an interior-only edit is real.
    const trimmed = v.replace(/^\s+|\s+$/g, "");
    return trimmed.length > 0 ? trimmed : fallback;
  };

  const pages = readPageSelection();
  const remote = modelStatus?.loaded ? modelStatus.mode === "remote" : value("enginePreset") === "custom";
  const repetitionProfile = remote ? "default" : value("optRepetitionProfile") || "default";
  const manual = repetitionProfile === "manual";

  return {
    quant: (quantEl && quantEl.value) || "Q8_0",
    dpi: numOr(dpiEl, DEFAULT_DPI),
    maxTokens: numOr(maxTokensEl, DEFAULT_MAX_TOKENS),
    keepImages: !!(keepImagesEl && keepImagesEl.checked),
    prompt: promptOr(promptEl, taskPrompt),
    // 0 is the meaningful default (deterministic OCR), so use the DRY-style
    // "0 is real" parse rather than repeatPenalty's "blank/0 -> null".
    temperature: floatOrNullMin0(temperatureEl),
    repetitionProfile,
    resultFormat: value("optResultFormat") || "markdown",
    repeatPenalty: manual ? floatOrNull(repeatPenaltyEl) : null,
    dryMultiplier: manual ? floatOrNullMin0(dryMultiplierEl) : null,
    // No "0 = off" meaning for a DRY base; blank/invalid/0 -> null (server default).
    dryBase: manual ? floatOrNull(dryBaseEl) : null,
    // Anti-loop preset (dense/math pages): an explicit field wins; else the toggle
    // supplies allowed length 2 and the legacy whole-context alias -1 (Rust resolves
    // it to the positive managed context size before HTTP); else server default.
    // These only reach the wire
    // when DRY is active (local GGUF), which the backend guarantees via its 1.0
    // dry_multiplier default; remote endpoints drop them.
    dryAllowedLength: manual ? numOr(dryAllowedLengthEl, antiLoopEl && antiLoopEl.checked ? 2 : null) : null,
    dryPenaltyLastN: manual ? intMinNeg1(dryPenaltyLastNEl, antiLoopEl && antiLoopEl.checked ? -1 : null) : null,
    // 1-based inclusive page range; both null = all pages. The backend validates
    // first>=1 and last>=first (a direct invoke bypasses the form min= clamp).
    firstPage: pages.firstPage,
    lastPage: pages.lastPage,
    // single/pages/both; backend parse_output_mode validates (defaults to single).
    outputMode: (document.getElementById("optOutputMode") || {}).value || "single",
  };
}

/** Read the Pages control (All / Range / Single) into a `{firstPage, lastPage}`
 *  pair. All (or a blank/invalid entry) -> both null (= all pages). Single -> the
 *  same page for both bounds. Range -> the two bounds, leaving a blank bound null
 *  for the backend to default (first->1, last->first). Never throws on a half-typed
 *  field: an unparseable number falls back to all so a run is never blocked. */
export function readPageSelection() {
  const mode = (document.getElementById("optPagesMode") || {}).value || "all";
  const posInt = (id) => {
    const v = parseInt((document.getElementById(id) || {}).value || "", 10);
    return Number.isFinite(v) && v > 0 ? v : null;
  };
  if (mode === "single") {
    const n = posInt("optPageFrom");
    return n === null ? { firstPage: null, lastPage: null } : { firstPage: n, lastPage: n };
  }
  if (mode === "range") {
    const f = posInt("optPageFrom");
    const t = posInt("optPageTo");
    if (f === null && t === null) return { firstPage: null, lastPage: null };
    return { firstPage: f, lastPage: t };
  }
  return { firstPage: null, lastPage: null };
}

/** Show/hide the page-number inputs to match #optPagesMode's current value, and
 *  keep the second input (the range "to") visible only for Range. Exported (not
 *  just an inline wirePageSelection closure) so a caller that needs to re-run
 *  this after restoring settings (settings.js's applySettingsToControls) can
 *  call it directly instead of dispatching a synthetic `change` event on
 *  #optPagesMode -- that event is also observed by wireAutoSaveEngineOptions,
 *  and a synthetic dispatch would spuriously re-trigger an auto-save. */
export function applyPageSelectionVisibility() {
  const modeEl = document.getElementById("optPagesMode");
  const wrap = document.getElementById("optPagesInputs");
  const toEl = document.getElementById("optPageTo");
  const label = document.getElementById("optPagesInputsLabel");
  const fromEl = document.getElementById("optPageFrom");
  if (!modeEl || !wrap) return;
  const mode = modeEl.value;
  wrap.hidden = mode === "all";
  if (toEl) toEl.hidden = mode !== "range";
  if (label) label.textContent = mode === "range" ? tr("opts.pages") : tr("opts.pageSingular");
  if (fromEl) fromEl.placeholder = mode === "range" ? tr("opts.from") : tr("opts.pagePh");
  renderEffectiveSummary();
}

/** Wire the Pages mode select so a user's mode change re-runs the visibility
 *  toggle above. Called once at startup. */
export function wirePageSelection() {
  const modeEl = document.getElementById("optPagesMode");
  if (!modeEl) return;
  // Visibility toggle on mode change; the summary is refreshed by the shared
  // #runOpts input/change listeners wired in DOMContentLoaded.
  modeEl.addEventListener("change", applyPageSelectionVisibility);
  applyPageSelectionVisibility();
  const refresh = () => { applyOptionVisibility(); renderEffectiveSummary(); };
  ["enginePreset", "remoteUrl", "remoteModel", "remoteKey", "setLlamaBin"].forEach((id) => {
    control(id)?.addEventListener("input", refresh);
    control(id)?.addEventListener("change", refresh);
  });
  document.addEventListener("unlocr:gguf-changed", refresh);
  control("optDpiProfile")?.addEventListener("change", () => {
    const selected = value("optDpiProfile");
    if (selected === "custom") return;
    setVal("optDpi", selected);
    // Use the existing autosave contract for the persisted numeric DPI.
    control("optDpi")?.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const syncInputs = () => {
    const queued = Array.from(document.querySelectorAll("#fileList .file-row__path"), (el) => el.textContent);
    applyInputPaths(queued.length ? queued : value("pdfPath") ? [value("pdfPath")] : []);
    renderEffectiveSummary();
  };
  control("pdfPath")?.addEventListener("input", syncInputs);
  control("pdfPath")?.addEventListener("change", syncInputs);
  // Native imports/queue selection assign .value directly; the file rail is
  // their observable selection seam (also covers mixed PDF/image batches).
  if (control("fileList") && typeof MutationObserver !== "undefined") {
    new MutationObserver(syncInputs).observe(control("fileList"), { childList: true, subtree: true });
  }
  document.addEventListener("unlocr:input-selected", (event) => {
    applyInputPaths(event.detail?.paths || []);
    renderEffectiveSummary();
  });
  syncInputs();
}

/** Applied model identity comes only from Rust; the next request's options come
 *  from readRunOptions. Pending load edits never masquerade as applied values. */
export function renderEffectiveSummary(event) {
  // main.js also observes the profile select. Do not reset its new selection
  // before the dedicated handler copies it into the persisted numeric DPI.
  applyOptionVisibility(event?.target?.id === "optDpiProfile");
  const vals = document.getElementById("effectiveVals");
  if (!vals) return;
  const opts = readRunOptions();
  // Pull the human-readable tail of the prompt (strip a leading <|tag|>) and cap
  // it so a long custom prompt does not blow out the one-line summary.
  const promptShort = opts.prompt
    .replace(/^<\|[^|]*\|\>/, "")
    .replace(/^\s+|\s+$/g, "");
  const shown = promptShort.length > 0 ? promptShort.slice(0, 48) : opts.prompt.slice(0, 48);
  const ellipsis = promptShort.length > 48 ? "…" : "";
  // Page span: blank = all. "5" for single, "5-9" for range, "5-end" when only the
  // first bound is set. Mirrors the firstPage/lastPage the run will send.
  let pagesNote = "";
  if (opts.firstPage != null || opts.lastPage != null) {
    const f = opts.firstPage != null ? opts.firstPage : 1;
    if (opts.lastPage != null && opts.lastPage !== f) pagesNote = tr("eff.pagesRange", { from: f, to: opts.lastPage });
    else if (opts.lastPage == null) pagesNote = tr("eff.pagesToEnd", { from: f });
    else pagesNote = tr("eff.pageSingle", { n: f });
  }
  const applied = modelStatus?.loaded ? modelStatus.effectiveConfig : null;
  const selected = readLoadOptions();
  const pending = !!applied && hasPendingLoadChanges(selected, applied, acceptedLoadOptions);
  let loaded = tr(modelStatus?.loaded ? "eff.snapshotUnavailable" : modelStatus ? "eff.notLoaded" : "eff.statusUnknown");
  if (applied) {
    const identity = applied.mode === "remote"
      ? [tr("engine.custom"), applied.model || tr("eff.serverModel"), publicEndpoint(applied.baseUrl)]
      : [tr("engine.llamacpp"), applied.quant || applied.model || tr("eff.customModel"),
        tr("eff.vision", { mode: tr("opts.vision." + (applied.visionMode || "auto")) }),
        applied.imageMaxTokens == null ? "" : tr("eff.visionTokens", { n: applied.imageMaxTokens }),
        applied.chatTemplate || (applied.quant ? tr("opts.embeddedTemplate") : tr("opts.chatModelDefault")),
        applied.mmprojFile ? tr("eff.projector", { name: applied.mmprojFile.split(/[/\\]/).pop() }) : ""];
    loaded = identity.filter(Boolean).join(" · ");
  }
  const run = [tr("eff.perPage"), tr("opts.format." + opts.resultFormat),
    tr("eff.budget", { n: opts.maxTokens }), tr("eff.temperature", { n: opts.temperature ?? 0 }),
    tr("eff.repetition", { profile: tr("opts.repetition." + opts.repetitionProfile) }),
    control("optDpi")?.disabled ? tr("eff.imageInput") : tr("eff.dpi", { n: opts.dpi }),
    tr("eff.keepImages", { state: tr(opts.keepImages ? "job.on" : "job.off") }),
    tr("eff.files", { mode: opts.outputMode }),
    tr("eff.prompt", { prompt: shown + ellipsis })];
  if (opts.repetitionProfile === "manual") {
    run.push(tr("eff.manual", { repeat: opts.repeatPenalty ?? 1.3, dry: opts.dryMultiplier ?? 1,
      base: opts.dryBase ?? 1.75, length: opts.dryAllowedLength ?? 4, window: opts.dryPenaltyLastN ?? "default" }));
  }
  vals.textContent = loaded + "\n" + tr("eff.nextRun") + " " + run.join(" · ") + pagesNote;
  const pendingEl = control("pendingModelChanges");
  if (pendingEl) {
    pendingEl.hidden = !pending;
    pendingEl.textContent = tr(selected.mode === "remote" ? "eff.pendingRemote" : "eff.pendingLocal");
  }
}

// EH-0013: re-render the effective-values summary on a locale switch so its
// composed string (DPI/tok/keep-images/prompt) retranslates instantly.
if (typeof window !== "undefined" && window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
  window.unlocrI18n.onLocaleChange(renderEffectiveSummary);
}
