/**
 * Обёртки над Tauri IPC: каждое обращение к бэкенду, каждый диалог и каждое
 * событие попадает в журнал приложения (см. `./logger`).
 *
 * Модули импортируют `invoke`/`listen`/`emit`/`open`/`save`/`confirmDialog`/
 * `readTextFile` отсюда вместо пакетов `@tauri-apps/*`, поэтому журналирование
 * включено везде.
 */
import {
  convertFileSrc as tauriConvertFileSrc,
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";
import {
  emit as tauriEmit,
  emitTo as tauriEmitTo,
  listen as tauriListen,
  type EventCallback,
  type EventName,
  type EventTarget as TauriEventTarget,
  type Options,
  type UnlistenFn,
} from "@tauri-apps/api/event";
import {
  confirm as tauriConfirm,
  open as tauriOpen,
  save as tauriSave,
  type ConfirmDialogOptions,
  type OpenDialogOptions,
  type OpenDialogReturn,
  type SaveDialogOptions,
} from "@tauri-apps/plugin-dialog";
import { readTextFile as tauriReadTextFile, type ReadFileOptions } from "@tauri-apps/plugin-fs";
import { logDebug, logError, logInfo, logTrace } from "./logger";

/** Ответы команд бывают большими — в журнал пишем краткую выжимку. */
function summarize(value: unknown): unknown {
  if (Array.isArray(value)) {
    return { элементов: value.length, образец: value.slice(0, 3) };
  }
  if (typeof value === "string" && value.length > 300) {
    return `${value.slice(0, 300)}… (${value.length} симв.)`;
  }
  return value;
}

function elapsed(started: number): number {
  return Math.round(performance.now() - started);
}

/** Вызов Rust-команды с журналированием аргументов, результата и ошибок. */
export async function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  const started = performance.now();
  logDebug("ipc", `→ ${cmd}`, args);
  try {
    const result = await tauriInvoke<T>(cmd, args, options);
    logDebug("ipc", `← ${cmd} (${elapsed(started)} мс)`, summarize(result));
    return result;
  } catch (error) {
    logError("ipc", `✗ ${cmd} (${elapsed(started)} мс)`, { error: String(error), args });
    throw error;
  }
}

/** Преобразование пути в asset-URL (журналируем на уровне trace). */
export function convertFileSrc(filePath: string, protocol?: string): string {
  logTrace("ipc", `convertFileSrc ${filePath}`);
  return tauriConvertFileSrc(filePath, protocol);
}

/** Подписка на событие Tauri с журналированием входящих сообщений. */
export async function listen<T>(
  event: EventName,
  handler: EventCallback<T>,
  options?: Options,
): Promise<UnlistenFn> {
  const unlisten = await tauriListen<T>(
    event,
    (payload) => {
      logDebug("event", `← ${payload.event}`, payload.payload);
      handler(payload);
    },
    options,
  );
  logDebug("event", `слушатель зарегистрирован: ${event}`);
  return () => {
    logDebug("event", `слушатель снят: ${event}`);
    unlisten();
  };
}

/** Глобальная отправка события. */
export async function emit<T>(event: string, payload?: T): Promise<void> {
  logDebug("event", `→ emit ${event}`, payload);
  await tauriEmit(event, payload);
}

/** Отправка события конкретному окну. */
export async function emitTo<T>(
  target: TauriEventTarget | string,
  event: string,
  payload?: T,
): Promise<void> {
  const label = typeof target === "string" ? target : JSON.stringify(target);
  logDebug("event", `→ emitTo ${label} ${event}`, payload);
  await tauriEmitTo(target, event, payload);
}

/** Диалог выбора файла с журналированием выбранного пути. */
export function open<T extends OpenDialogOptions>(options?: T): Promise<OpenDialogReturn<T>> {
  logDebug("dialog", "→ open", options);
  return tauriOpen<T>(options).then((result) => {
    logInfo("dialog", "← open", result ?? "отменено");
    return result;
  });
}

/** Диалог сохранения файла с журналированием выбранного пути. */
export function save(options?: SaveDialogOptions): Promise<string | null> {
  logDebug("dialog", "→ save", options);
  return tauriSave(options).then((result) => {
    logInfo("dialog", "← save", result ?? "отменено");
    return result;
  });
}

/**
 * Диалог подтверждения с журналированием ответа.
 *
 * Важно: плагин `dialog` при инициализации переопределяет `window.confirm` на
 * асинхронную функцию (см. `init-iife.js` в `tauri-plugin-dialog`), поэтому
 * привычная проверка `if (!window.confirm(...))` никогда не срабатывает: она
 * получает промис (он всегда «истинный») и действие выполняется без вопроса.
 * Для подтверждений используем этот враппер.
 */
export function confirmDialog(message: string, options?: ConfirmDialogOptions): Promise<boolean> {
  logDebug("dialog", `→ confirm: ${message}`, options);
  return tauriConfirm(message, options).then((confirmed) => {
    logInfo("dialog", `← confirm: ${confirmed ? "подтверждено" : "отменено"}`, { message });
    return confirmed;
  });
}

/** Чтение текстового файла с журналированием. */
export function readTextFile(path: string | URL, options?: ReadFileOptions): Promise<string> {
  logDebug("fs", `→ readTextFile ${String(path)}`);
  return tauriReadTextFile(path, options);
}
