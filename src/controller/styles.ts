import { confirmDialog, invoke, listen, open } from "../shared/ipc";
import { logError, logInfo, logWarn } from "../shared/logger";
import { EVENTS, type SetStylePayload } from "../shared/events";
import {
  STYLE_FONTS,
  defaultStyleConfig,
  mediaKindFromPath,
  normalizeStyleConfig,
  resolveStyleMediaPath,
  type BibleCaptionPosition,
  type StyleConfig,
  type StyleRecord,
} from "../shared/style";
import { sendToDisplay } from "./display-bridge";
import { previewSetStyle } from "./preview-frame";

type StyleRow = {
  id: number;
  name: string;
  configJson?: string;
  config_json?: string;
  isActive?: boolean;
  is_active?: boolean;
};

export type StylesHooks = {
  refreshIcons: () => void;
  previewFrames: () => HTMLIFrameElement[];
  previewBackgroundEnabled?: (frame: HTMLIFrameElement) => boolean;
};

let hooks: StylesHooks;
let styles: StyleRecord[] = [];
let selectedId: number | null = null;
let draft: StyleConfig = defaultStyleConfig();
let draftName = "";
let selectedMediaIndex = -1;
let activeConfig: StyleConfig = defaultStyleConfig();
const videoChecks = new Map<string, boolean>();
const videoProgress = new Map<string, number>();
const videoConversions = new Set<string>();
/** Проверки, которые уже выполняются: повторный рендер не должен дублировать вызов. */
const videoChecking = new Set<string>();

function $(sel: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(sel);
  if (!el) {
    throw new Error(`Missing ${sel}`);
  }
  return el;
}

function input(sel: string): HTMLInputElement {
  return $(sel) as HTMLInputElement;
}

function select(sel: string): HTMLSelectElement {
  return $(sel) as HTMLSelectElement;
}

function parseRow(row: StyleRow): StyleRecord {
  const raw = row.configJson ?? row.config_json ?? "{}";
  let parsed: Partial<StyleConfig> = {};
  try {
    parsed = JSON.parse(raw) as Partial<StyleConfig>;
  } catch {
    parsed = {};
  }
  return {
    id: Number(row.id),
    name: String(row.name),
    config: normalizeStyleConfig(parsed),
    isActive: Boolean(row.isActive ?? row.is_active),
  };
}

export function getActiveStyleConfig(): StyleConfig {
  return activeConfig;
}

function previewStyle(frame: HTMLIFrameElement, config: StyleConfig, forceBackground = false) {
  if (!forceBackground && hooks.previewBackgroundEnabled?.(frame) === false) {
    return { ...config, backgroundMode: "color", backgroundColor: "transparent" } satisfies StyleConfig;
  }
  return config;
}

/** Re-push the active style to every preview frame (e.g. after iframe load). */
export function refreshActiveStylePreviews() {
  for (const frame of hooks.previewFrames()) {
    previewSetStyle(frame, previewStyle(frame, activeConfig));
  }
}

async function pushStyleEverywhere(config: StyleConfig, applyBackground: boolean) {
  activeConfig = config;
  const payload = { ...config } satisfies SetStylePayload;
  for (const frame of hooks.previewFrames()) {
    previewSetStyle(frame, previewStyle(frame, payload, applyBackground));
  }
  try {
    await sendToDisplay(EVENTS.setStyle, {
      ...payload,
      // Force a concrete media pick for random when applying live.
      selectedMediaPath: applyBackground
        ? resolveStyleMediaPath(config)
        : config.selectedMediaPath,
      backgroundMode:
        applyBackground && config.backgroundMode === "random"
          ? "media"
          : config.backgroundMode,
    });
  } catch (err) {
    console.warn("[styles] push to display failed", err);
  }
}

export async function applyActiveStyleToOutputs(options?: { background?: boolean }) {
  await pushStyleEverywhere(activeConfig, options?.background !== false);
}

function fillFonts() {
  const font = select("#style-font");
  font.replaceChildren();
  for (const name of STYLE_FONTS) {
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    font.appendChild(opt);
  }
}

function syncBgModeUi() {
  const mode = draft.backgroundMode;
  document.querySelectorAll<HTMLInputElement>('input[name="style-bg-mode"]').forEach((el) => {
    el.checked = el.value === mode;
  });
  const colorWrap = document.getElementById("style-bg-color-wrap");
  const mediaWrap = document.getElementById("style-media-wrap");
  if (colorWrap) {
    colorWrap.hidden = mode !== "color";
  }
  if (mediaWrap) {
    mediaWrap.hidden = mode === "color";
  }
}

function renderMediaList() {
  const list = $("#style-media-list");
  list.replaceChildren();
  if (draft.mediaPaths.length === 0) {
    const li = document.createElement("li");
    li.className = "list-status";
    li.textContent = "Медиафайлы не добавлены";
    list.appendChild(li);
    input("#style-media-remove").disabled = true;
    return;
  }
  draft.mediaPaths.forEach((path, index) => {
    const li = document.createElement("li");
    li.className =
      "style-media-item" + (index === selectedMediaIndex ? " selected" : "");
    const kind = mediaKindFromPath(path);
    const name = path.replace(/\\/g, "/").split("/").pop() || path;
    li.innerHTML = `<span class="meta">${kind === "video" ? "Видео" : "Фото"}</span><span class="path">${name}</span>`;
    li.title = path;
    if (kind === "video") {
      const status = document.createElement("span");
      status.className = "media-optimize-status";
      if (videoChecks.get(path) === false) {
        status.textContent = "Не оптимировано";
        const convert = document.createElement("button");
        convert.type = "button";
        convert.className = "tool-btn media-convert-btn";
        convert.textContent = videoConversions.has(path)
          ? `${Math.round(videoProgress.get(path) || 0)}%`
          : "Конвертировать";
        convert.disabled = videoConversions.has(path);
        convert.addEventListener("click", (event) => {
          event.stopPropagation();
          void convertVideo(path);
        });
        li.appendChild(convert);
      } else if (videoChecks.get(path) === true) {
        status.textContent = "Готово";
      } else {
        status.textContent = "Проверка…";
      }
      li.appendChild(status);
      if (!videoChecks.has(path) && !videoChecking.has(path)) {
        void checkVideo(path);
      }
    }
    li.addEventListener("click", () => {
      selectedMediaIndex = index;
      draft.selectedMediaPath = path;
      renderMediaList();
    });
    list.appendChild(li);
  });
  input("#style-media-remove").disabled = selectedMediaIndex < 0;
}

async function checkVideo(path: string) {
  // Проверка одного и того же файла не должна идти параллельно: при старте
  // список медиа перерисовывается несколько раз подряд, и каждый рендер
  // запускал новый ffprobe.
  if (videoChecks.has(path) || videoChecking.has(path)) {
    return;
  }
  videoChecking.add(path);
  try {
    const optimized = await invoke<boolean>("check_video_optimization", { path });
    videoChecks.set(path, optimized);
  } catch (error) {
    // Ошибка проверки (нет ffprobe / файл не читается) — не помечаем файл
    // как «нужна конвертация», чтобы не показывать ложную кнопку.
    console.warn("Video optimization check failed", error);
    logError("video", `проверка оптимизации не удалась: ${path}`, { error: String(error) });
    videoChecks.set(path, true);
  } finally {
    videoChecking.delete(path);
  }
  renderMediaList();
}

async function convertVideo(path: string) {
  videoConversions.add(path);
  videoProgress.set(path, 0);
  renderMediaList();
  logInfo("video", `запуск конвертации: ${path}`);
  try {
    await invoke("optimize_video", { path });
  } catch (error) {
    videoConversions.delete(path);
    videoChecks.set(path, false);
    logError("video", `конвертация не удалась: ${path}`, { error: String(error) });
    window.alert(`Ошибка конвертации: ${String(error)}`);
    renderMediaList();
  }
}

function fillEditorFromDraft() {
  input("#style-name").value = draftName;
  input("#style-text-color").value = draft.textColor;
  select("#style-font").value = STYLE_FONTS.includes(draft.fontFamily as (typeof STYLE_FONTS)[number])
    ? draft.fontFamily
    : "Segoe UI";
  input("#style-bold").checked = draft.bold;
  select("#style-align").value = draft.align;
  input("#style-stroke-width").value = String(draft.strokeWidth);
  input("#style-stroke-color").value = draft.strokeColor;
  input("#style-stroke-opacity").value = String(Math.round(draft.strokeOpacity * 100));
  select("#style-transition").value = draft.transitionType;
  input("#style-transition-ms").value = String(draft.transitionMs);
  $("#style-transition-ms-label").textContent = `${draft.transitionMs} мс`;
  input("#style-bg-color").value = draft.backgroundColor;
  input("#style-bible-caption-enabled").checked = draft.bibleCaptionEnabled;
  select("#style-bible-caption-position").value = draft.bibleCaptionPosition;
  document
    .querySelector("#style-bible-caption-position")
    ?.closest("label")
    ?.classList.toggle("disabled", !draft.bibleCaptionEnabled);
  selectedMediaIndex = draft.selectedMediaPath
    ? draft.mediaPaths.indexOf(draft.selectedMediaPath)
    : -1;
  syncBgModeUi();
  renderMediaList();
}

function readDraftFromEditor(): StyleConfig {
  const modeEl = document.querySelector<HTMLInputElement>(
    'input[name="style-bg-mode"]:checked',
  );
  return normalizeStyleConfig({
    textColor: input("#style-text-color").value,
    fontFamily: select("#style-font").value,
    bold: input("#style-bold").checked,
    align: select("#style-align").value as StyleConfig["align"],
    strokeWidth: Number(input("#style-stroke-width").value) || 0,
    strokeColor: input("#style-stroke-color").value,
    strokeOpacity: Number(input("#style-stroke-opacity").value) / 100,
    transitionType: select("#style-transition").value as StyleConfig["transitionType"],
    transitionMs: Number(input("#style-transition-ms").value) || 0,
    backgroundMode: (modeEl?.value || "color") as StyleConfig["backgroundMode"],
    backgroundColor: input("#style-bg-color").value,
    mediaPaths: draft.mediaPaths,
    selectedMediaPath: draft.selectedMediaPath,
    bibleCaptionEnabled: input("#style-bible-caption-enabled").checked,
    bibleCaptionPosition: select("#style-bible-caption-position").value as BibleCaptionPosition,
  });
}

function renderStylesList() {
  const list = $("#styles-list");
  list.replaceChildren();
  for (const style of styles) {
    const li = document.createElement("li");
    li.className =
      "styles-list-item" +
      (style.id === selectedId ? " selected" : "") +
      (style.isActive ? " active-style" : "");
    li.innerHTML = `<span class="name">${style.name}</span>${
      style.isActive ? '<span class="badge">активен</span>' : ""
    }`;
    li.addEventListener("click", () => void selectStyle(style.id));
    list.appendChild(li);
  }
}

async function reloadStyles(preferId?: number | null) {
  const rows = await invoke<StyleRow[]>("list_styles").catch(() => []);
  styles = (Array.isArray(rows) ? rows : []).map(parseRow);
  const active = styles.find((s) => s.isActive) || styles[0] || null;
  activeConfig = active?.config ?? defaultStyleConfig();
  const nextId =
    preferId != null && styles.some((s) => s.id === preferId)
      ? preferId
      : selectedId != null && styles.some((s) => s.id === selectedId)
        ? selectedId
        : active?.id ?? null;
  if (nextId != null) {
    await selectStyle(nextId);
  } else {
    selectedId = null;
    $("#style-editor").hidden = true;
    $("#style-empty").hidden = false;
    renderStylesList();
  }
}

async function selectStyle(id: number) {
  const style = styles.find((s) => s.id === id);
  if (!style) {
    return;
  }
  logInfo("style", `выбран стиль «${style.name}» (#${id})`);
  selectedId = id;
  draftName = style.name;
  draft = { ...style.config, mediaPaths: [...style.config.mediaPaths] };
  $("#style-editor").hidden = false;
  $("#style-empty").hidden = true;
  fillEditorFromDraft();
  renderStylesList();
}

async function createStyle() {
  const config = defaultStyleConfig();
  const saved = await invoke<StyleRow>("save_style", {
    id: null,
    name: "Новый стиль",
    configJson: JSON.stringify(config),
  });
  const record = parseRow(saved);
  await reloadStyles(record.id);
  logInfo("style", `создан стиль (#${record.id})`);
}

async function saveCurrentStyle() {
  if (selectedId == null) {
    return;
  }
  draftName = input("#style-name").value.trim() || "Без названия";
  draft = readDraftFromEditor();
  const saved = await invoke<StyleRow>("save_style", {
    id: selectedId,
    name: draftName,
    configJson: JSON.stringify(draft),
  });
  const record = parseRow(saved);
  await reloadStyles(record.id);
  if (record.isActive || styles.find((s) => s.id === record.id)?.isActive) {
    await pushStyleEverywhere(record.config, true);
  }
  logInfo("style", `сохранён стиль «${draftName}» (#${record.id})`);
}

async function deleteCurrentStyle() {
  if (selectedId == null) {
    return;
  }
  if (!(await confirmDialog("Удалить этот стиль?"))) {
    return;
  }
  try {
    logInfo("style", `удаление стиля #${selectedId}`);
    await invoke("delete_style", { id: selectedId });
    selectedId = null;
    await reloadStyles();
    await applyActiveStyleToOutputs({ background: true });
  } catch (err) {
    logError("style", "не удалось удалить стиль", { error: String(err) });
    window.alert(String(err));
  }
}

async function activateCurrentStyle() {
  if (selectedId == null) {
    return;
  }
  draft = readDraftFromEditor();
  draftName = input("#style-name").value.trim() || draftName;
  await invoke("save_style", {
    id: selectedId,
    name: draftName,
    configJson: JSON.stringify(draft),
  });
  const active = await invoke<StyleRow | null>("set_active_style", { id: selectedId });
  if (active) {
    const record = parseRow(active);
    activeConfig = record.config;
    await reloadStyles(record.id);
    await pushStyleEverywhere(record.config, true);
    logInfo("style", `активирован стиль «${draftName}» (#${record.id})`);
  }
}

async function addMediaFiles() {
  const selected = await open({
    multiple: true,
    filters: [
      {
        name: "Медиа",
        extensions: ["jpg", "jpeg", "png", "webp", "gif", "mp4", "mkv", "avi", "mov", "webm"],
      },
    ],
  });
  if (!selected) {
    return;
  }
  const paths = Array.isArray(selected) ? selected : [selected];
  for (const path of paths) {
    if (!draft.mediaPaths.includes(path)) {
      draft.mediaPaths.push(path);
    }
  }
  if (!draft.selectedMediaPath && draft.mediaPaths[0]) {
    draft.selectedMediaPath = draft.mediaPaths[0];
    selectedMediaIndex = 0;
  }
  renderMediaList();
}

function removeSelectedMedia() {
  if (selectedMediaIndex < 0) {
    return;
  }
  const removed = draft.mediaPaths.splice(selectedMediaIndex, 1)[0];
  if (draft.selectedMediaPath === removed) {
    draft.selectedMediaPath = draft.mediaPaths[0] || null;
  }
  selectedMediaIndex = draft.selectedMediaPath
    ? draft.mediaPaths.indexOf(draft.selectedMediaPath)
    : -1;
  renderMediaList();
}

function bindSettingsTabs() {
  document.querySelectorAll("[data-settings-tab]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const name = (btn as HTMLElement).dataset.settingsTab;
      if (!name) {
        return;
      }
      document.querySelectorAll("[data-settings-tab]").forEach((node) => {
        node.classList.toggle("active", (node as HTMLElement).dataset.settingsTab === name);
      });
      document.querySelectorAll("[data-settings-panel]").forEach((panel) => {
        const el = panel as HTMLElement;
        const match = el.dataset.settingsPanel === name;
        el.hidden = !match;
        el.classList.toggle("active", match);
      });
      if (name === "styles") {
        void reloadStyles();
      }
    });
  });
}

export function bindStylesUi(h: StylesHooks) {
  hooks = h;
  void listen<{ path: string; percent: number; completed: boolean; error?: string }>("ffmpeg-progress", (event) => {
    const progress = event.payload;
    videoProgress.set(progress.path, progress.percent);
    if (progress.error) {
      videoConversions.delete(progress.path);
      logWarn("video", `FFmpeg: ${progress.error}`, { path: progress.path });
      window.alert(`Ошибка FFmpeg: ${progress.error}`);
    } else if (progress.completed) {
      videoConversions.delete(progress.path);
      videoChecks.set(progress.path, true);
      videoProgress.set(progress.path, 100);
      logInfo("video", `конвертация завершена: ${progress.path}`);
    }
    renderMediaList();
  });
  fillFonts();
  bindSettingsTabs();

  $("#style-add").addEventListener("click", () => void createStyle());
  $("#style-save").addEventListener("click", () => void saveCurrentStyle());
  $("#style-delete").addEventListener("click", () => void deleteCurrentStyle());
  $("#style-activate").addEventListener("click", () => void activateCurrentStyle());
  $("#style-media-add").addEventListener("click", () => void addMediaFiles());
  $("#style-media-remove").addEventListener("click", () => removeSelectedMedia());

  input("#style-transition-ms").addEventListener("input", () => {
    $("#style-transition-ms-label").textContent = `${input("#style-transition-ms").value} мс`;
  });

  document.querySelectorAll('input[name="style-bg-mode"]').forEach((el) => {
    el.addEventListener("change", () => {
      draft.backgroundMode = (el as HTMLInputElement).value as StyleConfig["backgroundMode"];
      syncBgModeUi();
    });
  });

  const captionEnabled = input("#style-bible-caption-enabled");
  captionEnabled.addEventListener("change", () => {
    const disabled = !captionEnabled.checked;
    select("#style-bible-caption-position").disabled = disabled;
    document
      .querySelector("#style-bible-caption-position")
      ?.closest("label")
      ?.classList.toggle("disabled", disabled);
  });
}

export async function bootStyles() {
  const active = await invoke<StyleRow | null>("get_active_style").catch(() => null);
  if (active) {
    const record = parseRow(active);
    activeConfig = record.config;
    selectedId = record.id;
  } else {
    activeConfig = defaultStyleConfig();
  }
  await reloadStyles(selectedId);
  // Preview frames may still be loading — apply again shortly.
  window.setTimeout(() => {
    for (const frame of hooks.previewFrames()) {
      previewSetStyle(frame, previewStyle(frame, activeConfig));
    }
  }, 400);
}
