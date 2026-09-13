/**
 * Вывод слов в OBS.
 *
 * OBS забирает текст источником «Браузер»: приложение отдаёт страницу-оверлей с
 * прозрачным фоном (см. `src-tauri/src/obs.rs`), а этот модуль показывает настройки
 * вывода во вкладке «Трансляция» и зеркалирует в OBS команды окна вывода (текст
 * слайда, оформление, очистку) — поэтому слова в трансляции не отстают от эфира.
 * В трансляцию уходят только песни и стихи Библии: объявления показывают в зале,
 * в OBS они не попадают (см. `obsAcceptsTextMode`).
 */
import { invoke } from "../shared/ipc";
import { logError, logInfo } from "../shared/logger";
import { EVENTS, type SetTextPayload, type TextMode } from "../shared/events";
import { cleanSongLines, type BibleCaptionPosition, type StyleConfig } from "../shared/style";

/** Ответ команды `obs_settings`. */
export type ObsSettings = {
  enabled: boolean;
  port: number;
  /** Подложка под текст: включена и её непрозрачность в процентах (0…100). */
  backdropEnabled: boolean;
  backdropOpacity: number;
  running: boolean;
  lastError: string | null;
  /** Адреса страницы-оверлея: локальный и (если есть) адрес в локальной сети. */
  urls: string[];
};

/**
 * Ключ последнего отправленного текста: одинаковые строки в OBS не дублируем.
 * Состояние вывода живёт в приложении, поэтому страница-оверлей в любом случае
 * получит актуальный слайд при подключении.
 */
let sentSlideKey = "";
/** Последний показанный слайд: при смене оформления строку собираем заново. */
let lastSlide: { lines: string[]; caption: string; mode: string } | null = null;
/** Активный стиль: из него берём, показывать ли подпись стиха и где именно. */
let lastStyle: StyleConfig | null = null;

function element<T extends HTMLElement>(selector: string): T | null {
  return document.querySelector<T>(selector);
}

function inputValue(selector: string): string {
  return element<HTMLInputElement>(selector)?.value.trim() ?? "";
}

/* ——— Отправка состояния в OBS ——— */

/**
 * Собирает текст для OBS одной строкой — как в прежней версии программы: строки
 * слайда склеиваются пробелом, подпись стиха («Ин 3:16») идёт в ту же строку —
 * в начало или в конец, как задано в активном стиле. Переносы по ширине источника
 * делает оверлей, поэтому отдельными строками текст в OBS не передаётся.
 */
function composeObsText(lines: string[], caption: string): string {
  const text = lines
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .join(" ");
  const reference = caption.trim();
  if (!reference) {
    return text;
  }
  if (lastStyle && !lastStyle.bibleCaptionEnabled) {
    return text;
  }
  if (!text) {
    return reference;
  }
  const after = captionGoesAfterText(lastStyle?.bibleCaptionPosition ?? "above");
  return after ? `${text} ${reference}` : `${reference} ${text}`;
}

/** Подпись стиха в конце строки: положения «под текстом» и у нижнего края экрана. */
function captionGoesAfterText(position: BibleCaptionPosition): boolean {
  return (
    position === "below" ||
    position === "inline-end" ||
    position === "screen-bottom-left" ||
    position === "screen-bottom-right"
  );
}

/**
 * Что вообще уходит в OBS: только слова песен и стихов Библии. Объявления
 * показывают в зале, в трансляцию они не идут — поэтому для любого другого
 * режима оверлей очищается, иначе в эфире остались бы слова предыдущей песни.
 */
export function obsAcceptsTextMode(mode: TextMode): boolean {
  return mode === "song" || mode === "bible";
}

/** Отправляет текст слайда; пустой слайд означает очистку. */
export async function pushObsSlide(lines: string[], caption = "", mode = ""): Promise<void> {
  lastSlide = { lines, caption, mode };
  const text = composeObsText(lines, caption);
  if (text === sentSlideKey) {
    return;
  }
  sentSlideKey = text;
  try {
    await invoke("obs_push_slide", { text, mode });
  } catch (error) {
    logError("obs", "не удалось отправить слайд в OBS", error);
  }
}

/** Отправляет оформление активного стиля; строку слайда пересобираем заново. */
export async function pushObsStyle(style: StyleConfig): Promise<void> {
  lastStyle = style;
  try {
    await invoke("obs_push_style", { style });
  } catch (error) {
    logError("obs", "не удалось отправить оформление в OBS", error);
  }
  if (lastSlide) {
    // Подпись стиха могла переехать в начало или в конец строки.
    await pushObsSlide(lastSlide.lines, lastSlide.caption, lastSlide.mode);
  }
}

/**
 * Зеркалирует команду окна вывода в OBS. Медиа в оверлей не попадает — там только
 * слова, поэтому «очистить текст» и полная очистка для OBS одно и то же.
 * В OBS уходят только песни и стихи Библии (см. `obsAcceptsTextMode`): объявления
 * на экране есть, а в трансляции их нет — оверлей в этом случае гасится.
 */
export function mirrorEventToObs(event: string, payload: unknown): void {
  switch (event) {
    case EVENTS.setText: {
      const text = payload as SetTextPayload;
      if (!obsAcceptsTextMode(text.mode)) {
        void pushObsSlide([], "", "");
        break;
      }
      const lines = text.mode === "song" ? cleanSongLines(text.lines, text.title) : text.lines;
      void pushObsSlide(lines, text.verseRef ?? "", text.mode);
      break;
    }
    case EVENTS.clear:
      void pushObsSlide([], "", "");
      break;
    case EVENTS.setStyle:
      void pushObsStyle(payload as StyleConfig);
      break;
    default:
      break;
  }
}

/* ——— Настройки вкладки «Трансляция» ——— */

function obsStatus(text: string, error = false) {
  const el = element<HTMLElement>("#obs-status");
  if (!el) {
    return;
  }
  el.textContent = text;
  el.classList.toggle("error", error);
}

/** Текст состояния: включён ли вывод и отвечает ли сервер. */
function statusText(settings: ObsSettings): string {
  if (settings.lastError) {
    return `Не удалось запустить сервер вывода: ${settings.lastError}`;
  }
  if (!settings.enabled) {
    return "Вывод выключен — слова в OBS не уходят.";
  }
  if (!settings.running) {
    return "Вывод включён, но сервер не отвечает — сохраните настройки ещё раз.";
  }
  const backdrop = settings.backdropEnabled
    ? `Подложка под текст: ${Math.round(settings.backdropOpacity)}%.`
    : "Подложка под текст выключена.";
  return (
    `Слова уходят в OBS на порт ${settings.port}: источник «Браузер» с адресом выше ` +
    `покажет текст слайда. ${backdrop} ` +
    "В OBS уходят только песни и стихи Библии — объявления в трансляцию не выводятся."
  );
}

/** Поле непрозрачности активно только при включённой подложке. */
function syncBackdropControls() {
  const enabled = element<HTMLInputElement>("#obs-backdrop-enabled")?.checked ?? false;
  const opacity = element<HTMLInputElement>("#obs-backdrop-opacity");
  if (opacity) {
    opacity.disabled = !enabled;
  }
}

/** Заполняет форму и подсказки по состоянию вывода. */
function applyObsSettings(settings: ObsSettings) {
  const enabled = element<HTMLInputElement>("#obs-enabled");
  if (enabled) {
    enabled.checked = settings.enabled;
  }
  const backdrop = element<HTMLInputElement>("#obs-backdrop-enabled");
  if (backdrop) {
    backdrop.checked = settings.backdropEnabled;
  }
  const opacity = element<HTMLInputElement>("#obs-backdrop-opacity");
  // Поле не перебиваем, пока его правят.
  if (opacity && document.activeElement !== opacity) {
    opacity.value = String(Math.round(settings.backdropOpacity));
  }
  syncBackdropControls();
  const port = element<HTMLInputElement>("#obs-port");
  // Поле не перебиваем, пока его правят.
  if (port && document.activeElement !== port) {
    port.value = String(settings.port);
  }
  const url = element<HTMLInputElement>("#obs-url");
  if (url) {
    url.value = settings.urls[0] ?? "";
  }
  const extra = element<HTMLElement>("#obs-urls");
  if (extra) {
    const lan = settings.urls.slice(1);
    extra.textContent =
      lan.length > 0
        ? `В локальной сети (если OBS на другом компьютере): ${lan.join(", ")}`
        : "";
  }
  obsStatus(statusText(settings), Boolean(settings.lastError));
}

/** Читает настройки вывода (при старте и при каждом открытии вкладки). */
export async function refreshObsStatus(): Promise<void> {
  try {
    applyObsSettings(await invoke<ObsSettings>("obs_settings"));
  } catch (error) {
    logError("obs", "не удалось прочитать настройки вывода", error);
    obsStatus(`Не удалось прочитать настройку: ${String(error)}`, true);
  }
}

/** Сохраняет настройки: сервер вывода запускается или останавливается сразу. */
async function saveObsSettings(): Promise<ObsSettings | null> {
  const enabled = element<HTMLInputElement>("#obs-enabled")?.checked ?? false;
  const backdropEnabled = element<HTMLInputElement>("#obs-backdrop-enabled")?.checked ?? false;
  const backdropOpacity = Math.round(Number(inputValue("#obs-backdrop-opacity") || "0")) || 0;
  const port = Math.trunc(Number(inputValue("#obs-port") || "0")) || 0;
  try {
    const settings = await invoke<ObsSettings>("obs_save_settings", {
      enabled,
      port,
      backdropEnabled,
      backdropOpacity,
    });
    applyObsSettings(settings);
    logInfo(
      "obs",
      `настройки вывода сохранены: включён=${settings.enabled}, порт=${settings.port}, ` +
        `подложка=${settings.backdropEnabled} ${settings.backdropOpacity}%`,
    );
    return settings;
  } catch (error) {
    logError("obs", "не удалось сохранить настройки вывода", error);
    obsStatus(String(error), true);
    return null;
  }
}

/** Копирует адрес оверлея в буфер обмена (иначе выделяет его в поле). */
async function copyObsUrl() {
  const field = element<HTMLInputElement>("#obs-url");
  const url = field?.value.trim() ?? "";
  if (!url) {
    obsStatus("Сначала сохраните настройки — адрес появится после запуска сервера.", true);
    return;
  }
  try {
    await navigator.clipboard.writeText(url);
    obsStatus("Адрес скопирован — вставьте его в поле «URL» источника «Браузер» в OBS.");
  } catch (error) {
    // Буфер обмена может быть недоступен: выделяем адрес, его можно скопировать вручную.
    logError("obs", "не удалось скопировать адрес оверлея", error);
    field?.select();
    obsStatus(`Скопируйте адрес вручную: ${url}`, true);
  }
}

/** Привязывает элементы вкладки «Трансляция» к настройкам вывода в OBS. */
export function bindObsUi(): void {
  element("#obs-settings-save")?.addEventListener("click", () => void saveObsSettings());
  element("#obs-copy-url")?.addEventListener("click", () => void copyObsUrl());
  element("#obs-backdrop-enabled")?.addEventListener("change", syncBackdropControls);
  void refreshObsStatus();
}
