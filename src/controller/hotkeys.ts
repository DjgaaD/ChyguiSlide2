import { invoke } from "../shared/ipc";
import { logError, logInfo } from "../shared/logger";

/**
 * Кастомные горячие клавиши: загрузка из SQLite (get_hotkeys / save_hotkeys),
 * захват в настройках и глобальный перехватчик keydown с preventDefault.
 */

export type HotkeyAction =
  | "show.start"
  | "show.end"
  | "slides.next"
  | "slides.prev"
  | "search.focus"
  | "nav.songs"
  | "nav.bible"
  | "nav.announcements";

type HotkeyRow = { action: string; key: string };

export const HOTKEY_DEFAULTS: Record<HotkeyAction, string> = {
  "show.start": "F5",
  "show.end": "Escape",
  "slides.next": "ArrowRight",
  "slides.prev": "ArrowLeft",
  "search.focus": "F4",
  "nav.songs": "F1",
  "nav.bible": "F2",
  "nav.announcements": "F3",
};

export const HOTKEY_ACTION_LABELS: Record<HotkeyAction, string> = {
  "show.start": "Начать показ",
  "show.end": "Завершить показ",
  "slides.next": "Следующий слайд",
  "slides.prev": "Предыдущий слайд",
  "search.focus": "Поиск в текущем разделе",
  "nav.songs": "Раздел «Песни»",
  "nav.bible": "Раздел «Библия»",
  "nav.announcements": "Раздел «Объявления»",
};

/** Поля поиска по разделам (для действия search.focus). */
const SEARCH_INPUT_BY_TAB: Record<string, string> = {
  songs: "#song-query",
  bible: "#bible-search-query",
  announcements: "#ann-query",
};

export type HotkeysHooks = {
  switchTab: (tab: string) => void;
  activeTab: () => string;
  startShow: () => void | Promise<void>;
  endShow: () => void | Promise<void>;
  nextSlide: () => void | Promise<void>;
  prevSlide: () => void | Promise<void>;
};

let bindings: Record<HotkeyAction, string> = { ...HOTKEY_DEFAULTS };
let hooks: HotkeysHooks | null = null;
let captureAction: HotkeyAction | null = null;
let saveTimer = 0;

/** Человекочитаемая подпись для кода клавиши (event.code). */
export function hotkeyLabel(code: string): string {
  if (!code) {
    return "— не назначено —";
  }
  const named: Record<string, string> = {
    Escape: "Esc",
    ArrowRight: "→",
    ArrowLeft: "←",
    ArrowUp: "↑",
    ArrowDown: "↓",
    Space: "Пробел",
    Enter: "Enter",
    Tab: "Tab",
    Backspace: "Backspace",
    Delete: "Delete",
    Insert: "Insert",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
    Minus: "-",
    Equal: "=",
    BracketLeft: "[",
    BracketRight: "]",
    Semicolon: ";",
    Quote: "'",
    Backquote: "`",
    Comma: ",",
    Period: ".",
    Slash: "/",
    Backslash: "\\",
  };
  if (named[code]) {
    return named[code];
  }
  const keyLike = /^(?:Key|Digit)(.+)$/.exec(code);
  if (keyLike) {
    return keyLike[1];
  }
  const numpad = /^Numpad(.+)$/.exec(code);
  if (numpad) {
    return `Num${numpad[1]}`;
  }
  return code;
}

/** Загрузка привязок из БД; отсутствующие действия — значения по умолчанию. */
export async function bootHotkeys(): Promise<void> {
  const rows = await invoke<HotkeyRow[]>("get_hotkeys").catch(() => [] as HotkeyRow[]);
  const map: Record<HotkeyAction, string> = { ...HOTKEY_DEFAULTS };
  for (const row of rows ?? []) {
    if (
      row &&
      typeof row.action === "string" &&
      typeof row.key === "string" &&
      row.action in HOTKEY_DEFAULTS
    ) {
      map[row.action as HotkeyAction] = row.key;
    }
  }
  bindings = map;
}

/** Сохранение в БД (debounce, чтобы частые изменения не спамили командами). */
function persistHotkeys() {
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    const list = (Object.keys(HOTKEY_DEFAULTS) as HotkeyAction[]).map((action) => ({
      action,
      key: bindings[action] ?? "",
    }));
    void invoke("save_hotkeys", { bindings: list })
      .then(() => logInfo("hotkey", "привязки сохранены в базу"))
      .catch((err) => {
        console.warn("[hotkeys] save failed", err);
        logError("hotkey", "не удалось сохранить привязки", { error: String(err) });
      });
  }, 250);
}

function isTypingTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) {
    return false;
  }
  const tag = el.tagName;
  return (
    tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable === true
  );
}

function actionForCode(code: string): HotkeyAction | null {
  for (const action of Object.keys(bindings) as HotkeyAction[]) {
    if (bindings[action] && bindings[action] === code) {
      return action;
    }
  }
  return null;
}

function runAction(action: HotkeyAction) {
  if (!hooks) {
    return;
  }
  logInfo("hotkey", `сработала горячая клавиша: ${action}`);
  switch (action) {
    case "show.start":
      void hooks.startShow();
      break;
    case "show.end":
      void hooks.endShow();
      break;
    case "slides.next":
      void hooks.nextSlide();
      break;
    case "slides.prev":
      void hooks.prevSlide();
      break;
    case "search.focus": {
      const selector = SEARCH_INPUT_BY_TAB[hooks.activeTab()];
      const field = selector ? document.querySelector<HTMLInputElement>(selector) : null;
      if (field) {
        field.focus();
        field.select?.();
      }
      break;
    }
    case "nav.songs":
      hooks.switchTab("songs");
      break;
    case "nav.bible":
      hooks.switchTab("bible");
      break;
    case "nav.announcements":
      hooks.switchTab("announcements");
      break;
  }
}

/**
 * Глобальный перехватчик: единая точка срабатывания горячих клавиш.
 * preventDefault блокирует браузерное поведение F1–F5 (справка, перезагрузка и т.д.).
 */
export function registerHotkeys(hooksIn: HotkeysHooks): void {
  hooks = hooksIn;
  window.addEventListener("keydown", (event) => {
    // Режим захвата в настройках обрабатывается отдельным слушателем.
    if (captureAction) {
      return;
    }
    const action = actionForCode(event.code);
    // Function keys such as F5 must never reach the browser/WebView2 reload handler.
    // The operator action itself remains disabled while typing in a field.
    if (action) {
      event.preventDefault();
      event.stopPropagation();
    }
    // Не мешаем вводу текста и открытым модальным окнам.
    if (
      action !== "show.start" &&
      (isTypingTarget(event.target) || document.querySelector("dialog[open]"))
    ) {
      return;
    }
    if (!action) {
      return;
    }
    runAction(action);
  });
}

/* ——— Вкладка настроек: список привязок + захват клавиш ——— */

function el<T extends HTMLElement>(selector: string): T {
  const found = document.querySelector<T>(selector);
  if (!found) {
    throw new Error(`Missing ${selector}`);
  }
  return found;
}

function renderHotkeysList() {
  const list = el<HTMLUListElement>("#hotkeys-list");
  list.replaceChildren();
  for (const action of Object.keys(HOTKEY_DEFAULTS) as HotkeyAction[]) {
    const li = document.createElement("li");
    li.className =
      "hotkey-row" +
      (captureAction === action ? " capturing" : "") +
      (bindings[action] ? "" : " unbound");

    const label = document.createElement("span");
    label.className = "hotkey-label";
    label.textContent = HOTKEY_ACTION_LABELS[action];

    const key = document.createElement("button");
    key.type = "button";
    key.className = "hotkey-capture";
    key.textContent =
      captureAction === action ? "Нажмите клавишу…" : hotkeyLabel(bindings[action]);
    key.title = "Кликните и нажмите клавишу (Esc — отмена)";
    key.addEventListener("click", () => {
      captureAction = captureAction === action ? null : action;
      renderHotkeysList();
    });

    const clear = document.createElement("button");
    clear.type = "button";
    clear.className = "hotkey-clear";
    clear.textContent = "×";
    clear.title = "Убрать привязку";
    clear.addEventListener("click", () => {
      logInfo("hotkey", `привязка снята: ${action}`);
      bindings[action] = "";
      captureAction = null;
      renderHotkeysList();
      persistHotkeys();
    });

    li.append(label, key, clear);
    list.appendChild(li);
  }
}

/** Захват клавиши: слушатель на фазе перехвата — перекрывает ввод и браузер. */
let captureListenerBound = false;

export function bindHotkeysUi(): void {
  renderHotkeysList();

  if (!captureListenerBound) {
    captureListenerBound = true;
    window.addEventListener(
      "keydown",
      (event) => {
        if (!captureAction) {
          return;
        }
        event.preventDefault();
        event.stopPropagation();
        // Esc — отмена захвата (сам Escape назначается кнопкой «×» + повторный захват без Esc).
        if (event.key === "Escape") {
          captureAction = null;
          renderHotkeysList();
          return;
        }
        if (
          event.key === "Shift" ||
          event.key === "Control" ||
          event.key === "Alt" ||
          event.key === "Meta"
        ) {
          return;
        }
        const code = event.code || event.key;
        if (!code) {
          return;
        }
        // Одна клавиша — одно действие: снимаем дубликаты.
        for (const other of Object.keys(bindings) as HotkeyAction[]) {
          if (other !== captureAction && bindings[other] === code) {
            bindings[other] = "";
          }
        }
        logInfo("hotkey", `назначена клавиша ${code} → ${captureAction}`);
        bindings[captureAction] = code;
        captureAction = null;
        renderHotkeysList();
        persistHotkeys();
      },
      true,
    );
  }

  el<HTMLButtonElement>("#hotkeys-reset").addEventListener("click", () => {
    logInfo("hotkey", "привязки сброшены к значениям по умолчанию");
    bindings = { ...HOTKEY_DEFAULTS };
    captureAction = null;
    renderHotkeysList();
    persistHotkeys();
  });
}