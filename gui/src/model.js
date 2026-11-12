// Managed local model / editable remote connection, applied model status,
// load progress, and metadata-driven quant download labels.

import { requireTauri } from "./tauri.js";
import { showToast, removeToast } from "./toasts.js";
import { acceptModelStatus, readLoadOptions, renderEffectiveSummary } from "./options.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in every fn.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

// Last-known loaded state, stamped by refreshModelStatus. updateLoadLabel reads
// THIS rather than inferring loaded state from the button's textContent, so a
// live locale switch (which changes tr("model.reload") under the button) cannot
// make it relabel a loaded model as "Load model".
let modelLoaded = false;
let lastModelStatus = null;
let labelRevision = 0;
let statusRevision = 0;

// True only while a load is in flight (set on Load click, cleared in its finally).
// The ocr://progress / status / server-ready listeners are gated on this so a
// late-arriving server-ready event cannot clobber the resting "Loaded: ..." text
// after refreshModelStatus has already set it.
let loadingModel = false;
// Keep startup errors visible across model_status refreshes (including finally).
let modelLoadError = null;

// Load-phase "still alive" feedback. The backend emits ocr://status ("loading
// model into memory…") ONCE, then Server::start blocks in await_health for 30-90s
// (CPU load of the multi-GB GGUF) emitting nothing; meanwhile the download toast
// has already been removed, so both the model bar and the toast stack look frozen.
// This ticks a 1s elapsed-seconds counter on both surfaces until server-ready or
// the load resolves, so the user can see it is working, not hung.
let loadHeartbeat = null;
let loadHeartbeatBase = "";
let loadHeartbeatStart = 0;
const LOAD_TOAST_ID = "load:memory";

/** Stop the load heartbeat and clear its toast. Idempotent. */
function stopLoadFeedback() {
  if (loadHeartbeat) {
    clearInterval(loadHeartbeat);
    loadHeartbeat = null;
  }
  removeToast(LOAD_TOAST_ID);
}

/** Show `baseMsg` on the model bar + a toast, both with a live "(Ns)" elapsed
 *  counter, so a long event-less load phase is visibly progressing. Self-stops if
 *  the load gate (loadingModel) drops. ponytail: "(Ns)" is an elapsed unit, not
 *  prose, so it is not routed through i18n (the base message is backend-supplied
 *  and already English). */
function startLoadFeedback(baseMsg) {
  stopLoadFeedback();
  loadHeartbeatBase = baseMsg || tr("model.loading");
  loadHeartbeatStart = Date.now();
  const statusText = document.getElementById("modelStatusText");
  const paint = () => {
    const secs = Math.floor((Date.now() - loadHeartbeatStart) / 1000);
    if (statusText) statusText.textContent = loadHeartbeatBase + " (" + secs + "s)";
    showToast(LOAD_TOAST_ID, { kind: "download", title: loadHeartbeatBase, meta: secs + "s" });
  };
  paint();
  loadHeartbeat = setInterval(() => {
    if (!loadingModel) {
      stopLoadFeedback();
      return;
    }
    paint();
  }, 1000);
}

/** Paint the titlebar model-light + label from a model_status payload. This is
 *  the single at-a-glance status (the model-bar text below carries detail):
 *  green dot + the loaded model label (e.g. "Unlimited-OCR-2 Q8_0") for a local
 *  model, violet dot + the label for a remote endpoint, gray dot + "No model"
 *  when nothing is loaded. The dot color carries loaded/idle/remote state, so
 *  no "Local"/"Loaded:" prefix is needed. */
export function updateModeBadge(status) {
  const badge = document.getElementById("modeBadge");
  if (!badge) return;
  const loaded = !!(status && status.loaded);
  const mode = status && status.mode;
  const dotClass = !loaded ? "is-off" : mode === "remote" ? "is-remote" : "is-loaded";
  const fallback = mode === "remote" ? tr("model.remote") : tr("model.local");
  const label = !loaded ? tr("status.noModel") : (status && status.label) || fallback;
  const dot = document.createElement("span");
  dot.className = "titlebar__mode-dot " + dotClass;
  badge.replaceChildren(dot, document.createTextNode(label));
}

/** Fetch model_status and fan it out: the Run gate (ui), the titlebar badge, and
 *  the model bar (status text + Load/Unload enablement). Called on startup and
 *  after every load/unload. Fail-soft outside the webview. */
export async function refreshModelStatus(ui) {
  const revision = ++statusRevision;
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let status;
  try {
    status = await t.core.invoke("model_status");
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[model] status failed", err);
    return; // Preserve the last known snapshot and Run gate after a failed poll.
  }
  if (revision !== statusRevision || !status || typeof status.loaded !== "boolean") return;
  lastModelStatus = status;
  acceptModelStatus(status);
  if (ui && typeof ui.applyModelStatus === "function") ui.applyModelStatus(status);
  updateModeBadge(status);
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");
  modelLoaded = !!status.loaded;
  if (unloadBtn) unloadBtn.disabled = !status.loaded;
  if (statusText) {
    statusText.textContent = modelLoadError !== null
      ? tr("model.loadFailed", { error: modelLoadError })
      : status.loaded ? tr(status.mode === "remote" ? "model.connectedLabel" : "model.loadedLabel", { label: status.label }) : tr("model.noModelLoaded");
  }
  // Upgrade the unloaded "Load model" label to "Download & load model" when the
  // selected local quant is not yet cached (best-effort; loaded state is left).
  updateLoadLabel();
  if (status.loaded && status.mode === "local") refreshProjectorInfo(t);
  // Signal load/unload/startup completion so the file-rail pipeline pane can
  // re-run preflight (a download may have added the GGUF). DOM event is the seam
  // (this fn has no `rail`), same pattern as the GGUF picker's unlocr:gguf-changed.
  document.dispatchEvent(new CustomEvent("unlocr:model-changed"));
}

/** When no model is loaded, label the Load button "Download & load model" if the
 *  selected local quant is not cached on disk, else "Load model". Remote backends
 *  and a custom GGUF override never download a quant, so they stay "Load model".
 *  Best-effort: silent outside the webview or if list_local_models fails. */
async function updateLoadLabel() {
  const revision = ++labelRevision;
  const loadBtn = document.getElementById("loadModelBtn");
  if (!loadBtn) return;
  const unloadBtn = document.getElementById("unloadModelBtn");
  if (unloadBtn) unloadBtn.textContent = tr(modelLoaded && lastModelStatus?.mode === "remote" ? "model.disconnect" : "model.unload");
  if (activeEngineMode() !== "local") {
    loadBtn.textContent = tr(modelLoaded && lastModelStatus?.mode === "remote" ? "model.reconnect" : "model.connect");
    return;
  }
  if (modelLoaded) {
    loadBtn.textContent = tr("model.reload");
    return;
  }
  if (pickedGguf("modelFilePath")) {
    loadBtn.textContent = tr("model.load");
    return;
  }
  const quant = document.getElementById("optQuant")?.value;
  let cached = [];
  try {
    cached = await requireTauri().core.invoke("list_local_models");
  } catch (err) {
    if (revision === labelRevision) loadBtn.textContent = tr("model.load");
    return;
  }
  if (revision !== labelRevision) return;
  loadBtn.textContent = (cached || []).includes(quant)
    ? tr("model.load")
    : tr("model.downloadLoad");
}

// Connection choices never mutate the user's URL, key, or served model name.
export const ENGINE_PRESETS = {
  llamacpp: { mode: "local" },
  custom: { mode: "remote" },
};

/** Toggle connection fields and local-only model options. */
export function applyPreset(name) {
  const p = ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp;
  const remoteFields = document.getElementById("remoteFields");
  if (remoteFields) remoteFields.hidden = p.mode !== "remote";
  // Custom-GGUF pickers apply only to the managed-local spawn (they replace the
  // download + quant naming); hide them for any remote backend.
  const localFields = document.getElementById("localFields");
  if (localFields) localFields.hidden = p.mode !== "local";
  const quantEl = document.getElementById("optQuant");
  const quantField = quantEl && quantEl.closest(".opts__field");
  if (quantField) quantField.hidden = p.mode !== "local";
  renderEffectiveSummary();
  updateLoadLabel();
}

/** Wire the connection dropdown. The selected preset's mode is read by the
 *  Load button to pick local vs remote. */
export function wireEnginePreset() {
  const sel = document.getElementById("enginePreset");
  if (!sel) return;
  sel.addEventListener("change", () => {
    applyPreset(sel.value);
  });
  applyPreset(sel.value);
}

/** Open the engine-connection modal from the Modify button. applyPreset() has
 *  already toggled which block (remote vs custom-GGUF) is visible inside it.
 *  Native <dialog>: backdrop, Esc, and the Done/× forms close it for free. */
export function wireEngineDialog() {
  const btn = document.getElementById("engineModifyBtn");
  const dlg = document.getElementById("engineDialog");
  if (!btn || !dlg || typeof dlg.showModal !== "function") return;
  btn.addEventListener("click", () => dlg.showModal());
}

/** Return the active backend's mode ("local" | "remote"). */
export function activeEngineMode() {
  const sel = document.getElementById("enginePreset");
  const name = sel ? sel.value : "llamacpp";
  return (ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp).mode;
}

/** Wire the Load/Unload model buttons. Load reads the active engine mode + the
 *  quant (local) or remote URL/key (remote) and calls load_model, then refreshes
 *  status. Loading is long (download + health wait) so the button shows progress
 *  via the app-lifetime ocr:// listeners attached in attachLoadListeners. */
/** Read a custom-GGUF picker's chosen path, or null if none picked / element
 *  missing. Stored on the span's dataset by wireGgufPicker. */
function pickedGguf(spanId) {
  const el = document.getElementById(spanId);
  const p = el && el.dataset ? el.dataset.path : "";
  return p && p.trim() ? p : null;
}

/** Render a GGUF override, including its Clear button. Used by both the picker
 *  and settings restore; rendering alone must not trigger an auto-save. */
export function setGgufPath(spanId, path) {
  const span = document.getElementById(spanId);
  if (!span) return;
  const selected = typeof path === "string" && path.trim() ? path : "";
  if (selected) span.dataset.path = selected;
  else delete span.dataset.path;
  span.textContent = selected ? selected.split(/[/\\]/).pop() : tr("model.none");
  span.title = selected;
  const clearBtn = document.getElementById(spanId + "Clear");
  if (clearBtn) clearBtn.disabled = !selected;
  renderEffectiveSummary();
}

/** Wire one GGUF file picker: clicking the button opens the native dialog
 *  (tauri-plugin-dialog, same as the PDF importer) filtered to .gguf, and the
 *  chosen path is stored on the paired span (dataset.path + visible basename).
 *  The explicit Clear button resets the override. Clicking the path also clears
 *  it for compatibility with the previous UI. */
function wireGgufPicker(btnId, spanId, label) {
  const btn = document.getElementById(btnId);
  const span = document.getElementById(spanId);
  if (!btn || !span) return;
  const setPath = (p) => {
    setGgufPath(spanId, p);
    // A custom model GGUF skips the download, so the Load label drops the
    // "Download &" prefix; clearing it restores the cached/uncached label.
    updateLoadLabel();
    // Let settings.js's auto-save persist this pick/clear without a direct
    // import (settings.js already imports FROM model.js; the reverse would
    // be circular). A plain DOM event is the decoupling seam.
    span.dispatchEvent(new CustomEvent("unlocr:gguf-changed", { bubbles: true }));
  };
  // Initialize without clearing a restored path or firing a save on startup.
  setGgufPath(spanId, pickedGguf(spanId));
  const clearBtn = document.getElementById(spanId + "Clear");
  if (clearBtn) clearBtn.addEventListener("click", () => setPath(null));
  btn.addEventListener("click", async () => {
    const dialog = window.__TAURI__ && window.__TAURI__.dialog;
    if (!dialog || !dialog.open) return;
    try {
      const selected = await dialog.open({
        multiple: false,
        directory: false,
        filters: [{ name: "GGUF", extensions: ["gguf"] }],
      });
      if (typeof selected === "string" && selected.trim()) setPath(selected);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[model] " + label + " picker failed:", err.message);
    }
  });
  // Click the path span to clear the override.
  span.addEventListener("click", () => setPath(null));
}

/**
 * Wire up event listeners and custom file pickers for the model control bar.
 * @param {Object} ui - The UI controller instance.
 */
export function wireModelBar(ui) {
  const loadBtn = document.getElementById("loadModelBtn");
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");

  // Custom-GGUF pickers (local mode). Each button opens the native dialog and
  // stores the chosen path on the paired span's dataset; the load handler reads
  // it. Clearing is via the span's clear button. No path picked -> null sent.
  wireGgufPicker("pickModelBtn", "modelFilePath", "model GGUF");
  wireGgufPicker("pickMmprojBtn", "mmprojFilePath", "projector GGUF");

  // Quant change can flip cached<->uncached: refresh the download/load label.
  const quantEl = document.getElementById("optQuant");
  if (quantEl) quantEl.addEventListener("change", () => updateLoadLabel());

  if (loadBtn) {
    loadBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        if (statusText) statusText.textContent = tr("model.unavailableOutside");
        return;
      }
      const loadOptions = readLoadOptions();
      ++statusRevision; // Ignore a status poll begun before this load request.
      loadBtn.disabled = true;
      if (unloadBtn) unloadBtn.disabled = true;
      modelLoadError = null;
      removeToast("load:error");
      if (statusText) statusText.textContent = tr(loadOptions.mode === "remote" ? "model.connecting" : "model.loading");
      loadingModel = true;
      try {
        const status = await t.core.invoke("load_model", loadOptions);
        lastModelStatus = status;
        modelLoaded = !!status.loaded;
        if (unloadBtn) unloadBtn.disabled = !status.loaded;
        acceptModelStatus(status, loadOptions);
        if (ui) ui.applyModelStatus(status);
        // Paint the badge + status text DIRECTLY from the known load result
        // instead of waiting on refreshModelStatus's model_status re-fetch (a
        // late ocr://server-ready event can otherwise land after it and pin the
        // text on "server ready on :PORT"). loadingModel is dropped in finally
        // BEFORE that re-fetch, so those listeners cannot clobber this.
        updateModeBadge(status);
        if (statusText) statusText.textContent = tr(status.mode === "remote" ? "model.connectedLabel" : "model.loadedLabel", { label: status.label });
      } catch (err) {
        modelLoadError = String(err);
        const message = tr("model.loadFailed", { error: modelLoadError });
        if (statusText) statusText.textContent = message;
        showToast("load:error", { kind: "error", title: message });
        console.error("[model] load failed", err);
      } finally {
        // Load resolved (success line above / catch). Kill the heartbeat + its
        // toast now so a pending tick cannot overwrite the final "Loaded: ..."
        // label; loadingModel drops just below as a second backstop.
        stopLoadFeedback();
        loadBtn.disabled = false;
        // Clear the gate BEFORE refreshModelStatus's await: otherwise a late
        // ocr://server-ready landing during that re-fetch passes the gate and
        // overwrites the "Loaded: ..." label above with "server ready on :PORT".
        loadingModel = false;
        await refreshModelStatus(ui);
      }
    });
  }

  if (unloadBtn) {
    unloadBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        return;
      }
      // Feedback + badge flip immediately: the backend kill/drop is fast, but
      // without this the model bar sits on "server ready on :PORT" until
      // refreshModelStatus lands "No model loaded". loadingModel is false here,
      // so the gated load listeners cannot overwrite this.
      loadingModel = false;
      ++statusRevision;
      modelLoadError = null;
      removeToast("load:error");
      if (statusText) statusText.textContent = tr("model.stopping");
      updateModeBadge({ loaded: false, mode: "", label: "" });
      try {
        const status = await t.core.invoke("unload_model");
        // The command result is authoritative even if the subsequent poll fails.
        lastModelStatus = status;
        modelLoaded = !!status.loaded;
        acceptModelStatus(status);
        if (ui) ui.applyModelStatus(status);
        updateModeBadge(status);
        unloadBtn.disabled = !status.loaded;
        if (statusText) statusText.textContent = tr("model.noModelLoaded");
        updateLoadLabel();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[model] unload failed", err);
      }
      await refreshModelStatus(ui);
    });
  }
}

/** App-lifetime listeners that surface model LOAD progress in the model bar
 *  (download pct + server-ready). These events now fire during load_model, not
 *  run_ocr, so they belong to the model bar rather than the per-run subscription. */
export function attachLoadListeners() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const statusText = document.getElementById("modelStatusText");
  t.event.listen("ocr://progress", (e) => {
    if (!loadingModel) return;
    const { name, pct } = (e && e.payload) || {};
    if (statusText) statusText.textContent = tr("model.downloadingPct", { name: name || tr("model.model"), pct: pct || 0 });
  });
  t.event.listen("ocr://status", (e) => {
    if (!loadingModel) return;
    const { message } = (e && e.payload) || {};
    // This status marks the long, event-less "loading into memory" phase; drive
    // the elapsed-seconds heartbeat (bar + toast) so it does not look frozen.
    if (message) startLoadFeedback(message);
  });
  t.event.listen("ocr://server-ready", (e) => {
    if (!loadingModel) return;
    stopLoadFeedback();
    const { port } = (e && e.payload) || {};
    if (statusText) statusText.textContent = tr("model.serverReady", { port });
  });
}

/** Metadata sizes are downloads, never estimates of runtime memory usage. */
export function quantTierLabel(name, _tier, sizeBytes) {
  const key = { Q8_0: "recommended", BF16: "full", "AD-Q6_K": "compact", "AD-Q5_K": "compact", "AD-Q4_K": "smallest" }[name];
  const size = Number.isFinite(sizeBytes) ? " · " + (sizeBytes / 1e9).toFixed(2) + " GB (" + sizeBytes.toLocaleString("en-US") + " B)" : "";
  return name + (key ? " · " + tr("quant." + key) : "") + size;
}

/** Rebuild the 3 quant `<select>`s (`#optQuant`, `#setQuant`, `#qsQuant`) from
 *  the backend's full published lineup (list_available_quants: all five quants,
 *  not just the 3 CLI Quality tiers), marking each already-cached option.
 *  Replaces the previously-static `<option>` lists in index.html. Preserves
 *  each select's current selection when it's still an available quant, else
 *  falls back to Q8_0 (OcrOptions::default().quant). Best-effort; never throws. */
export async function populateQuantSelects() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let quants = [];
  try {
    quants = await t.core.invoke("list_available_quants");
  } catch (err) {
    return;
  }
  const names = new Set(quants.map((q) => q.name));
  ["optQuant", "setQuant", "qsQuant"].forEach((id) => {
    const sel = document.getElementById(id);
    if (!sel) return;
    const prevValue = sel.value;
    sel.innerHTML = "";
    quants.forEach((q) => {
      const opt = document.createElement("option");
      opt.value = q.name;
      const label = quantTierLabel(q.name, q.tier, q.sizeBytes);
      opt.textContent = q.cached ? label + tr("model.cached") : label;
      sel.appendChild(opt);
    });
    sel.value = names.has(prevValue) ? prevValue : names.has("Q8_0") ? "Q8_0" : sel.value;
  });
  await refreshProjectorInfo(t);
}

/** Shared projector size from AtomicChat's pinned b2cda9b HF tree metadata:
 *  825,245,856 bytes. Presence is independent of the selected weight quant. */
async function refreshProjectorInfo(t) {
  const el = document.getElementById("projectorInfo");
  if (!el) return;
  el.removeAttribute("data-i18n");
  try {
    // Passive presence probe; list_cached_files would hash every multi-GB model.
    const report = await t.core.invoke("preflight", { quant: document.getElementById("optQuant")?.value || "Q8_0" });
    el.textContent = report.mmprojPresent
      ? tr("quant.projectorCached")
      : tr("quant.projectorDownload");
  } catch {
    el.textContent = tr("quant.projectorUnknown");
  }
}

/** Alias kept for existing call sites (main.js, settings.js): a single
 *  re-fetch now does both jobs (relabeling AND cached-marking), since the
 *  `<select>` options are rebuilt from the backend rather than statically
 *  relabeled in place. */
export async function markCachedQuants() {
  await populateQuantSelects();
}

// EH-0013: re-render the quant tier labels and the model Load-button label on a
// locale switch. Both re-derive via tr() (reading the freshly-updated dict), so
// the quant <option> text and the Load/Download&load button flip language
// instantly. (The model status TEXT is refreshed on the next status event.)
if (typeof window !== "undefined" && window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
  window.unlocrI18n.onLocaleChange(markCachedQuants);
  window.unlocrI18n.onLocaleChange(updateLoadLabel);
}
