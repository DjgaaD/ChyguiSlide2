/**
 * Регулировка ширины левой колонки на вкладках «Песни», «Библия» и «Трансляция».
 *
 * Раскладка страниц — CSS Grid: левая колонка берёт ширину из переменной
 * `--left-col-width`, средняя забирает остаток (`minmax(0, 1fr)`), правая имеет
 * собственную фиксированную ширину (`--right-col-width`) и при перетаскивании не
 * меняется. Переменная живёт на элементе конкретной вкладки, поэтому у каждой
 * вкладки своя ширина; сохранённые значения лежат в БД (`app_settings`).
 */
import { invoke } from "../shared/ipc";
import { logError, logInfo } from "../shared/logger";

/** Вкладки с регулируемой левой колонкой. */
export type SplitterTab = "songs" | "bible" | "broadcast";

/** Ключи настроек в таблице `app_settings` — своя ширина на каждую вкладку. */
export const LEFT_WIDTH_KEYS: Record<SplitterTab, string> = {
  songs: "ui.songs.left_width",
  bible: "ui.bible.left_width",
  broadcast: "ui.broadcast.left_width",
};

const TABS: SplitterTab[] = ["songs", "bible", "broadcast"];

/** Минимальная ширина левой колонки, px. */
const MIN_LEFT_WIDTH = 200;
/** Максимальная доля ширины окна, которую может занять левая колонка. */
const MAX_LEFT_RATIO = 0.5;
/** Ширина по умолчанию — та же доля, что в CSS (`--left-col-width`). */
const DEFAULT_LEFT_WIDTH = "17%";

/** Ширины, выставленные пользователем: нужны для пересчёта при resize окна. */
const appliedWidths = new Map<SplitterTab, number>();

/**
 * Завершение текущего перетаскивания. Хранится отдельно, чтобы закончить жест
 * и в случае, когда `mouseup` не дошёл (кнопку отпустили вне окна приложения).
 */
let endActiveDrag: (() => void) | null = null;

function viewElement(tab: SplitterTab): HTMLElement | null {
  return document.querySelector<HTMLElement>(`#view-${tab}`);
}

function leftColumn(tab: SplitterTab): HTMLElement | null {
  return viewElement(tab)?.querySelector<HTMLElement>(".col-left") ?? null;
}

/** Ограничения: не уже 200px и не больше половины ширины окна. */
function clampLeftWidth(width: number): number {
  const max = Math.max(MIN_LEFT_WIDTH, Math.round(window.innerWidth * MAX_LEFT_RATIO));
  return Math.min(Math.max(Math.round(width), MIN_LEFT_WIDTH), max);
}

/** Применяет ширину левой колонки к конкретной вкладке (px). */
export function applyLeftColumnWidth(tab: SplitterTab, width: number): void {
  const view = viewElement(tab);
  if (!view) {
    return;
  }
  const value = clampLeftWidth(width);
  view.style.setProperty("--left-col-width", `${value}px`);
  appliedWidths.set(tab, value);
}

/** Возврат к ширине по умолчанию (двойной клик по разделителю). */
function resetLeftColumnWidth(tab: SplitterTab): void {
  appliedWidths.delete(tab);
  viewElement(tab)?.style.setProperty("--left-col-width", DEFAULT_LEFT_WIDTH);
}

/** Значение из БД: `"320"` — px, `"17%"` — доля (значение по умолчанию). */
function applyStoredWidth(tab: SplitterTab, raw: string | undefined): void {
  const value = (raw ?? "").trim();
  if (!value) {
    return;
  }
  if (value.endsWith("%")) {
    resetLeftColumnWidth(tab);
    viewElement(tab)?.style.setProperty("--left-col-width", value);
    return;
  }
  const px = Number(value);
  if (Number.isFinite(px) && px > 0) {
    applyLeftColumnWidth(tab, px);
  }
}

async function saveLeftColumnWidth(tab: SplitterTab, value: string): Promise<void> {
  try {
    await invoke("set_app_setting", { key: LEFT_WIDTH_KEYS[tab], value });
    logInfo("ui", `ширина левой колонки «${tab}» сохранена: ${value}`);
  } catch (error) {
    logError("ui", "не удалось сохранить ширину левой колонки", {
      tab,
      error: String(error),
    });
  }
}

/** Перетаскивание: mousemove/mouseup слушаем на document, иначе курсор «слетает». */
function beginDrag(resizer: HTMLElement, tab: SplitterTab, startX: number): void {
  const column = leftColumn(tab);
  const startWidth = column ? column.getBoundingClientRect().width : MIN_LEFT_WIDTH;

  const onMove = (event: MouseEvent) => {
    applyLeftColumnWidth(tab, startWidth + (event.clientX - startX));
  };
  const onUp = () => {
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    document.body.classList.remove("col-resizing");
    resizer.classList.remove("dragging");
    endActiveDrag = null;
    const width = appliedWidths.get(tab);
    if (width != null) {
      void saveLeftColumnWidth(tab, String(width));
    }
  };

  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
  document.body.classList.add("col-resizing");
  resizer.classList.add("dragging");
  endActiveDrag = onUp;
}

function bindResizers(): void {
  document.querySelectorAll<HTMLElement>("[data-resizer]").forEach((resizer) => {
    const tab = resizer.dataset.resizer as SplitterTab | undefined;
    if (!tab || !TABS.includes(tab)) {
      return;
    }
    resizer.addEventListener("mousedown", (event) => {
      if (event.button !== 0) {
        return;
      }
      // preventDefault: иначе начинается выделение текста в соседних колонках.
      event.preventDefault();
      // Предыдущий жест мог не завершиться (например, окно потеряло фокус).
      endActiveDrag?.();
      beginDrag(resizer, tab, event.clientX);
    });
    // Двойной клик — быстрый возврат к ширине по умолчанию.
    resizer.addEventListener("dblclick", () => {
      resetLeftColumnWidth(tab);
      void saveLeftColumnWidth(tab, DEFAULT_LEFT_WIDTH);
    });
  });
}

/** Читает сохранённые ширины из БД и применяет их к вкладкам. */
async function restoreLeftColumnWidths(): Promise<void> {
  const keys = TABS.map((tab) => LEFT_WIDTH_KEYS[tab]);
  const stored = await invoke<Record<string, string>>("get_app_settings", { keys }).catch(
    (error) => {
      logError("ui", "не удалось прочитать ширины левых колонок", { error: String(error) });
      return {} as Record<string, string>;
    },
  );
  for (const tab of TABS) {
    applyStoredWidth(tab, stored?.[LEFT_WIDTH_KEYS[tab]]);
  }
}

/** Окно сузили — сохранённые px-ширины заново ограничиваем правилами. */
function reapplyLeftColumnWidths(): void {
  for (const [tab, width] of [...appliedWidths]) {
    applyLeftColumnWidth(tab, width);
  }
}

/** Инициализация: разделители, восстановление ширин из БД, реакция на resize. */
export async function bootColumnSplitters(): Promise<void> {
  bindResizers();
  // Кнопку могли отпустить вне окна — жест завершаем по потере фокуса.
  window.addEventListener("blur", () => endActiveDrag?.());
  await restoreLeftColumnWidths();
  window.addEventListener("resize", reapplyLeftColumnWidths, { passive: true });
}
