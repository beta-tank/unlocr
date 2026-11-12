// No browser/npm dependencies. Run with Node >=22: node --test gui/tests/*.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";

class Control {
  constructor(id, value = "") {
    Object.assign(this, { id, value, dataset: {}, checked: false, hidden: false, listeners: new Map() });
  }
  addEventListener(type, fn) {
    this.listeners.set(type, [...(this.listeners.get(type) || []), fn]);
  }
  async fire(type = "change") {
    await Promise.all((this.listeners.get(type) || []).map((fn) => fn({ currentTarget: this, target: this })));
  }
  closest() { return null; }
  showModal() {}
}
const controls = new Map();
globalThis.window = {};
globalThis.document = {
  getElementById: (id) => controls.get(id) || null,
  documentElement: { style: {} },
  addEventListener() {},
};
globalThis.localStorage = { getItem: () => null, setItem() {} };

const options = await import("../src/options.js");
const settings = await import("../src/settings.js");
const { wireQuickSettingsPopup } = await import("../src/quick_settings.js");
const { runOcrOnPath } = await import("../src/run_ocr.js");
const { wireModelBar } = await import("../src/model.js");
const add = (id, value) => { const el = new Control(id, value); controls.set(id, el); return el; };
const el = (id) => controls.get(id);
const settle = () => new Promise((resolve) => setImmediate(resolve));

function setup() {
  controls.clear();
  for (const [id, value] of Object.entries({
    enginePreset: "llamacpp", optQuant: "Q8_0", optDpi: "144", optMaxTokens: "8000",
    optPrompt: "local prompt", optVisionMode: "base", optImageMaxTokens: "",
    optRepetitionProfile: "manual", optRepeatPenalty: "1.1", optDryMultiplier: "0",
    optDryBase: "", optDryAllowedLength: "", optDryPenaltyLastN: "",
    optChatTemplate: "", optTemperature: "0.2", optResultFormat: "markdown",
    optPagesMode: "all", optOutputMode: "single", remoteUrl: "https://remote.example/v1",
    remoteModel: "served-alias", remoteKey: "••••••••", setLlamaBin: "",
    setMode: "local", setQuant: "Q8_0", setDpi: "144", setMaxTokens: "8000", setPrompt: "local prompt",
    setRemoteUrl: "https://remote.example/v1", setRemoteModel: "served-alias", setRemoteKey: "••••••••",
    setIdleMinutes: "15", modelFilePath: "", mmprojFilePath: "",
  })) add(id, value);
  ["optKeepImages", "optAntiLoop", "settingsSave", "effectiveVals", "pendingModelChanges"].forEach((id) => add(id));
  let saved = settings.mergeSettingsPatch({ mode: "local", defaultQuant: "Q8_0", defaultDpi: 144,
    defaultMaxTokens: 8000, defaultPrompt: "local prompt", temperature: 0.2, visionMode: "base",
    repetitionProfile: "manual", repeatPenalty: 1.1, dryMultiplier: 0, remoteApiKey: "••••••••",
    remoteBaseUrl: "https://remote.example/v1", remoteModel: "served-alias",
    remoteConfig: { defaultMaxTokens: 2000, defaultPrompt: "remote prompt", temperature: null },
  }, {});
  const calls = [];
  const bridge = { core: { async invoke(command, payload) {
    calls.push([command, payload]);
    if (command === "get_settings") return structuredClone(saved);
    if (command === "save_settings") { saved = structuredClone(payload.newSettings); return; }
    if (command === "run_ocr") return { paths: ["/tmp/result.md"], combined: "unused" };
    if (command === "read_text_file") return "\n<PAGE><BLOCK>text<FORK>raw & text\n";
    if (command === "add_notification") return;
    throw new Error(`unexpected command ${command}`);
  } } };
  window.__TAURI__ = bridge;
  options.acceptModelStatus({ loaded: false });
  return { bridge, calls, saved: () => saved };
}

test("Workspace switches preserve profiles; narrow edits cannot overwrite other defaults", async () => {
  const ctx = setup();
  settings.wireAutoSaveEngineOptions();
  el("optMaxTokens").value = "9000";
  await el("optMaxTokens").fire();
  assert.equal(ctx.saved().localConfig.defaultMaxTokens, 9000);
  assert.equal(el("setMaxTokens").value, 9000);
  el("enginePreset").value = "custom";
  await el("enginePreset").fire();
  assert.equal(el("optMaxTokens").value, 2000);
  assert.equal(el("optPrompt").value, "remote prompt");
  assert.equal(el("optTemperature").value, "", "null must clear the previous profile's temperature");
  el("optPrompt").value = "edited remote";
  await el("optPrompt").fire();
  el("optMaxTokens").value = "stale uncommitted value";
  el("optDpi").value = "300";
  await el("optDpi").fire();
  assert.equal(ctx.saved().remoteConfig.defaultMaxTokens, 2000);
  assert.equal(ctx.saved().remoteConfig.defaultPrompt, "edited remote");
  el("enginePreset").value = "llamacpp";
  await el("enginePreset").fire();
  assert.equal(el("optMaxTokens").value, 9000);
  assert.equal(el("optPrompt").value, "local prompt");
  assert.equal(el("optTemperature").value, 0.2);
  assert.equal(el("optRepeatPenalty").value, 1.1);
  assert.equal(ctx.saved().defaultDpi, 300);
});

test("queued Workspace saves capture the changed value before a connection switch", async () => {
  const ctx = setup();
  settings.wireAutoSaveEngineOptions();
  el("optMaxTokens").value = "9100";
  const save = el("optMaxTokens").fire();
  el("optMaxTokens").value = "not the edited value";
  el("enginePreset").value = "custom";
  const switchMode = el("enginePreset").fire();
  await Promise.all([save, switchMode]);
  assert.equal(ctx.saved().localConfig.defaultMaxTokens, 9100);
  assert.equal(ctx.saved().remoteConfig.defaultMaxTokens, 2000);
  assert.equal(ctx.saved().mode, "remote");
});

test("Settings mode/save restores the target profile and Workspace mirrors stay current", async () => {
  const ctx = setup();
  await settings.wireSettings();
  settings.wireAutoSaveEngineOptions();
  el("optMaxTokens").value = "9500";
  await el("optMaxTokens").fire();
  el("setMode").value = "remote";
  await el("setMode").fire();
  // Save may be clicked while the asynchronous mode preview is still in flight.
  await el("settingsSave").fire("click");
  assert.equal(ctx.saved().localConfig.defaultMaxTokens, 9500);
  assert.equal(ctx.saved().remoteConfig.defaultMaxTokens, 2000);
  assert.equal(el("optTemperature").value, "");
  assert.equal(el("optMaxTokens").value, 2000);
  assert.equal(el("enginePreset").value, "custom");
  el("setMode").value = "local";
  await el("setMode").fire();
  await el("settingsSave").fire("click");
  assert.equal(el("optMaxTokens").value, 9500);
  assert.equal(el("optTemperature").value, 0.2);
});

test("Quick Settings saves the selected profile and keeps local/remote defaults separate", async () => {
  const ctx = setup();
  settings.wireAutoSaveEngineOptions();
  el("enginePreset").value = "custom";
  await el("enginePreset").fire();
  ["settingsGearBtn", "quickSettingsDialog", "qsSave", "qsQuant", "qsDpi", "qsMaxTokens", "qsPrompt"].forEach((id) => add(id));
  wireQuickSettingsPopup();
  await el("settingsGearBtn").fire("click");
  assert.equal(el("qsMaxTokens").value, 2000);
  el("qsMaxTokens").value = "3000";
  el("qsDpi").value = "200";
  el("qsPrompt").value = "";
  await el("qsSave").fire("click");
  assert.equal(ctx.saved().remoteConfig.defaultMaxTokens, 3000);
  assert.equal(ctx.saved().localConfig.defaultMaxTokens, 8000);
  assert.equal(ctx.saved().remoteConfig.defaultPrompt, "");
  assert.equal(el("setMaxTokens").value, 3000);
  assert.equal(el("optMaxTokens").value, 3000);
  await settle();
});

test("pending status detects cleared projector/binary overrides even without a load response", () => {
  setup();
  const applied = { mode: "local", quant: "Q8_0", visionMode: "base", imageMaxTokens: 273,
    chatTemplate: null, mmprojFile: "/custom/projector.gguf", mmprojOverride: "/custom/projector.gguf",
    llamaBinOverride: "/custom/llama-server" };
  assert.equal(options.hasPendingLoadChanges(options.readLoadOptions(), applied), true);
  el("mmprojFilePath").dataset.path = applied.mmprojOverride;
  el("setLlamaBin").value = applied.llamaBinOverride;
  assert.equal(options.hasPendingLoadChanges(options.readLoadOptions(), applied), false);
  el("mmprojFilePath").dataset.path = "";
  options.acceptModelStatus({ loaded: true, mode: "local", effectiveConfig: applied });
  assert.equal(el("pendingModelChanges").hidden, false);
  const remote = { mode: "remote", model: "alias", baseUrl: "https://server/v1" };
  assert.equal(options.hasPendingLoadChanges({ ...remote, apiKey: "new" }, remote, { ...remote, apiKey: "old" }), true);
});

test("run options use the loaded backend; result format never changes the prompt", async () => {
  const ctx = setup();
  options.acceptModelStatus({ loaded: true, mode: "remote", effectiveConfig: { mode: "remote" } });
  let opts = options.readRunOptions();
  assert.equal(opts.repetitionProfile, "default");
  assert.equal(opts.repeatPenalty, null);
  assert.equal(opts.dryMultiplier, null);
  el("enginePreset").value = "custom";
  options.acceptModelStatus({ loaded: true, mode: "local", effectiveConfig: { mode: "local", quant: "Q8_0" } });
  opts = options.readRunOptions();
  assert.equal(opts.repetitionProfile, "manual");
  assert.equal(opts.dryMultiplier, 0);
  for (const format of ["raw", "text"]) {
    el("optResultFormat").value = format;
    const rendered = [];
    const ok = await runOcrOnPath("/tmp/input.png", null, { clear() {}, render: (...args) => rendered.push(args) }, { value: [] });
    assert.equal(ok, true);
    const payload = ctx.calls.filter(([cmd]) => cmd === "run_ocr").at(-1)[1];
    assert.equal(payload.resultFormat, format);
    assert.equal(payload.prompt, "local prompt");
    assert.equal(payload.dryMultiplier, 0);
    assert.equal(rendered[0][0], "\n<PAGE><BLOCK>text<FORK>raw & text\n");
  }
});

test("successful load/unload results remain authoritative when the status poll fails", async () => {
  const ctx = setup();
  add("loadModelBtn");
  add("unloadModelBtn");
  add("modelStatusText");
  const invoke = ctx.bridge.core.invoke;
  ctx.bridge.core.invoke = async (command, payload) => {
    if (command === "model_status") throw new Error("poll unavailable");
    if (command === "list_local_models") return [];
    if (command === "load_model") return { loaded: true, mode: "remote", label: "remote",
      effectiveConfig: { mode: "remote", baseUrl: "https://remote.example/v1", model: "served-alias" } };
    if (command === "unload_model") return { loaded: false, mode: "", label: "", effectiveConfig: null };
    return invoke(command, payload);
  };
  const statuses = [];
  wireModelBar({ applyModelStatus: (s) => statuses.push(s.loaded) });
  const logError = console.error;
  console.error = () => {};
  try {
    await el("loadModelBtn").fire("click");
    assert.equal(el("unloadModelBtn").disabled, false);
    assert.equal(options.readRunOptions().repetitionProfile, "default");
    await el("unloadModelBtn").fire("click");
    assert.deepEqual(statuses, [true, false]);
    assert.equal(el("unloadModelBtn").disabled, true);
    assert.equal(el("pendingModelChanges").hidden, true);
    assert.equal(options.readRunOptions().repetitionProfile, "manual");
  } finally { console.error = logError; }
});
