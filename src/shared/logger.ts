/**
 * Журнал приложения на стороне фронтенда.
 *
 * Все записи уходят пакетами в Rust-команду `log_events` и пишутся в файл
 * текущего сеанса (см. `src-tauri/src/logger.rs`). Дополнительно дублируются
 * в консоль для отладки в dev-режиме.
 */
import { invoke } from "@tauri-apps/api/core";

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

type LogRecord = {
  level: LogLevel;
  scope: string;
  message: string;
  data?: unknown;
};

/** Ограничение очереди: при переполнении самые старые записи отбрасываются. */
const MAX_QUEUE = 2000;
const FLUSH_DELAY_MS = 30;

const queue: LogRecord[] = [];
let flushTimer = 0;
let dropped = 0;
let installed = false;
let enabled = true;

/** Приводит произвольное значение к виду, который безопасно сериализовать в JSON. */
function safeData(value: unknown): unknown {
  if (value === null || value === undefined) {
    return value;
  }
  if (value instanceof Error) {
    return { name: value.name, message: value.message, stack: value.stack };
  }
  const type = typeof value;
  if (type === "string" || type === "number" || type === "boolean") {
    return value;
  }
  try {
    return JSON.parse(
      JSON.stringify(value, (_key, item) => {
        if (typeof item === "function") {
          return "<function>";
        }
        if (typeof item === "bigint") {
          return item.toString();
        }
        if (typeof Node !== "undefined" && item instanceof Node) {
          return `<${item.nodeName.toLowerCase()}>`;
        }
        return item;
      }),
    );
  } catch {
    return String(value);
  }
}

function consoleWrite(record: LogRecord) {
  const prefix = `[${record.scope}]`;
  const hasData = record.data !== undefined;
  if (record.level === "error") {
    hasData ? console.error(prefix, record.message, record.data) : console.error(prefix, record.message);
  } else if (record.level === "warn") {
    hasData ? console.warn(prefix, record.message, record.data) : console.warn(prefix, record.message);
  } else if (record.level === "debug" || record.level === "trace") {
    hasData ? console.debug(prefix, record.message, record.data) : console.debug(prefix, record.message);
  } else {
    hasData ? console.log(prefix, record.message, record.data) : console.log(prefix, record.message);
  }
}

function flush() {
  window.clearTimeout(flushTimer);
  flushTimer = 0;
  if (queue.length === 0) {
    return;
  }
  const batch = queue.splice(0, queue.length);
  void invoke("log_events", { entries: batch }).catch((error) => {
    dropped += batch.length;
    console.error("[logger] не удалось записать журнал", error);
  });
}

/** Немедленно отправляет накопленные записи (например, перед закрытием окна). */
export function flushNow() {
  flush();
}

function enqueue(record: LogRecord) {
  if (queue.length >= MAX_QUEUE) {
    queue.shift();
    dropped += 1;
  }
  queue.push(record);
  if (!flushTimer) {
    flushTimer = window.setTimeout(flush, FLUSH_DELAY_MS);
  }
}

/** Основная точка входа: одна запись журнала. */
export function log(level: LogLevel, scope: string, message: string, data?: unknown): void {
  if (!enabled) {
    return;
  }
  const record: LogRecord = { level, scope, message };
  if (data !== undefined) {
    record.data = safeData(data);
  }
  enqueue(record);
  consoleWrite(record);
}

export const logTrace = (scope: string, message: string, data?: unknown) =>
  log("trace", scope, message, data);
export const logDebug = (scope: string, message: string, data?: unknown) =>
  log("debug", scope, message, data);
export const logInfo = (scope: string, message: string, data?: unknown) =>
  log("info", scope, message, data);
export const logWarn = (scope: string, message: string, data?: unknown) =>
  log("warn", scope, message, data);
export const logError = (scope: string, message: string, data?: unknown) =>
  log("error", scope, message, data);

/** Сколько записей потеряно из-за переполнения очереди. */
export function droppedCount(): number {
  return dropped;
}

/**
 * Полностью отключает журналирование (используется в preview-iframe,
 * которые получают сообщения через postMessage и не должны писать в файл).
 */
export function disableLogging(): void {
  enabled = false;
  queue.length = 0;
}

/** Человекочитаемое описание элемента интерфейса для журнала. */
export function describeElement(target: EventTarget | null): string {
  const node = target as HTMLElement | null;
  if (!node || typeof node.tagName !== "string") {
    return String(target);
  }
  const parts: string[] = [node.tagName.toLowerCase()];
  if (node.id) {
    parts.push(`#${node.id}`);
  }
  const classes =
    typeof node.className === "string"
      ? node.className.trim().split(/\s+/).filter(Boolean)
      : [];
  if (classes.length > 0) {
    parts.push(`.${classes.slice(0, 3).join(".")}`);
  }
  const data = node.dataset
    ? Object.entries(node.dataset)
        .slice(0, 4)
        .map(([key, value]) => `[data-${key}=${value}]`)
        .join("")
    : "";
  const text = (node.textContent ?? "").trim().replace(/\s+/g, " ").slice(0, 60);
  return `${parts.join("")}${data}${text ? ` "${text}"` : ""}`;
}

/** Ближайший осмысленный интерактивный элемент (для записи кликов). */
function interactiveAncestor(target: EventTarget | null): EventTarget | null {
  const node = target as HTMLElement | null;
  if (!node || typeof node.closest !== "function") {
    return target;
  }
  return (
    node.closest(
      "button, a, [role='button'], li, tr, label, input, select, textarea, summary, [data-tab], [data-settings-tab]",
    ) ?? node
  );
}

function isEditable(element: Element | null): boolean {
  if (!element) {
    return false;
  }
  const tag = element.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    (element as HTMLElement).isContentEditable === true
  );
}

function handleClick(event: MouseEvent) {
  logInfo("click", describeElement(interactiveAncestor(event.target)));
}

/** Поля, значения которых в журнал не попадают: пароли и поля с `data-secret`. */
function isSecretField(element: Element & { type?: string }): boolean {
  return element.type === "password" || element.hasAttribute("data-secret");
}

function handleChange(event: Event) {
  const element = event.target as HTMLInputElement | null;
  if (!element || typeof element.tagName !== "string") {
    return;
  }
  let value: unknown = element.value;
  if (isSecretField(element)) {
    value = "<скрыто>";
  } else if (element.type === "checkbox" || element.type === "radio") {
    value = element.checked;
  }
  logInfo("change", describeElement(element), { value });
}

function handleKeydown(event: KeyboardEvent) {
  const target = event.target as Element | null;
  // Одиночные символы в полях ввода не пишем — иначе журнал заполнится текстом песни.
  if (isEditable(target) && event.key.length === 1 && !event.ctrlKey && !event.altKey) {
    return;
  }
  logDebug("key", `${event.key} (${event.code})`, {
    target: describeElement(target),
    ctrl: event.ctrlKey,
    alt: event.altKey,
    shift: event.shiftKey,
    meta: event.metaKey,
  });
}

/**
 * Включает журналирование действий пользователя и ошибок.
 * Вызывается один раз при старте каждого окна (controller / display).
 */
export function installGlobalLogging(role: "controller" | "display"): void {
  if (installed) {
    return;
  }
  installed = true;

  logInfo("app", `старт фронтенда: ${role}`, {
    href: window.location.href,
    preview: new URLSearchParams(window.location.search).get("preview") === "1",
    screen: `${window.screen.width}x${window.screen.height}`,
    dpr: window.devicePixelRatio,
    userAgent: navigator.userAgent,
  });

  window.addEventListener("error", (event) => {
    logError("window", "необработанная ошибка", {
      message: event.message,
      source: event.filename,
      line: event.lineno,
      column: event.colno,
    });
  });

  window.addEventListener("unhandledrejection", (event) => {
    const reason = (event as PromiseRejectionEvent).reason;
    logError("window", "необработанный отказ промиса", {
      reason: reason instanceof Error ? `${reason.name}: ${reason.message}` : String(reason),
    });
  });

  document.addEventListener("click", handleClick, true);
  document.addEventListener("change", handleChange, true);
  document.addEventListener("keydown", handleKeydown, true);

  document.addEventListener("visibilitychange", () => {
    logDebug("window", `видимость страницы: ${document.visibilityState}`);
  });
  window.addEventListener("focus", () => logTrace("window", "окно получило фокус"));
  window.addEventListener("blur", () => logTrace("window", "окно потеряло фокус"));
  window.addEventListener("beforeunload", () => {
    logInfo("app", "окно закрывается (beforeunload)");
    flushNow();
  });
}


export type JournalInfo = {
  dir: string;
  currentFile: string;
  maxFiles: number;
  files: string[];
};

/** Сводка о файлах журнала (каталог, текущий файл, список). */
export async function fetchJournalInfo(): Promise<JournalInfo | null> {
  try {
    return await invoke<JournalInfo>("get_log_info");
  } catch (error) {
    logWarn("app", "не удалось получить сведения о журнале", { error: String(error) });
    return null;
  }
}

/** Пишет в журнал путь к текущему файлу — чтобы пользователь знал, где искать. */
export async function logJournalLocation(): Promise<JournalInfo | null> {
  const info = await fetchJournalInfo();
  if (info) {
    logInfo("app", "файл журнала текущего сеанса", {
      currentFile: info.currentFile,
      dir: info.dir,
      files: info.files.length,
      maxFiles: info.maxFiles,
    });
  }
  return info;
}

/** Открывает каталог с журналами в системном файловом менеджере. */
export async function openJournalFolder(): Promise<void> {
  try {
    const dir = await invoke<string>("open_logs_folder");
    logInfo("app", `каталог журналов открыт: ${dir}`);
  } catch (error) {
    logError("app", "не удалось открыть каталог журналов", { error: String(error) });
  }
}

