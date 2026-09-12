import { emitTo, invoke, listen } from "../shared/ipc";
import { logInfo, logWarn } from "../shared/logger";
import { EVENTS, PREVIEW_CHANNEL } from "../shared/events";
import { postToPreview } from "./preview-frame";

const DISPLAY = "display";
const READY_TIMEOUT_MS = 8000;

let displayReady = false;
let readyWatchStarted = false;
let monitorIndex: number | null = null;
let previewFrame: HTMLIFrameElement | null = null;

/**
 * Register the preview iframe so it mirrors everything sent to the display.
 * Call once during initialization with the broadcast preview frame element.
 */
export function setPreviewFrame(iframe: HTMLIFrameElement | null) {
  previewFrame = iframe;
}

export function setDisplayMonitorIndex(index: number | null) {
  monitorIndex = index;
}

export function markDisplayClosed() {
  displayReady = false;
  console.log("[show] display marked closed (ready=false)");
  logInfo("display", "окно вывода помечено закрытым");
}

async function watchReadyEvents() {
  if (readyWatchStarted) {
    return;
  }
  readyWatchStarted = true;
  await listen(EVENTS.displayReady, () => {
    displayReady = true;
    console.log("[show] ← display:ready received");
    logInfo("display", "окно вывода сообщило о готовности");
  });
}

function waitForReadySignal(): Promise<void> {
  if (displayReady) {
    return Promise.resolve();
  }

  return new Promise((resolve, reject) => {
    let settled = false;
    let unlistenFn: (() => void) | null = null;
    let pingTimer = 0;

    const finish = (ok: boolean, err?: Error) => {
      if (settled) {
        return;
      }
      settled = true;
      window.clearTimeout(timer);
      // Готовность пришла — «догоняющий» ping уже не нужен и вреден: окно
      // вывода отвечает на него переприменением активного стиля и затирает
      // только что отправленное медиа (вместо видео остаётся фон стиля).
      window.clearTimeout(pingTimer);
      unlistenFn?.();
      if (ok) {
        displayReady = true;
        resolve();
      } else {
        reject(err ?? new Error("ready wait failed"));
      }
    };

    const timer = window.setTimeout(() => {
      finish(false, new Error(`timeout ${READY_TIMEOUT_MS}ms`));
    }, READY_TIMEOUT_MS);

    void listen(EVENTS.displayReady, () => {
      console.log("[show] ← display:ready (wait)");
      finish(true);
    }).then((unlisten) => {
      unlistenFn = unlisten;
      if (settled) {
        unlisten();
      }
    });

    // Already-open Display may have missed our listener — ask it to re-announce.
    pingTimer = window.setTimeout(() => {
      if (settled) {
        return;
      }
      console.log("[show] → display:ping");
      void emitTo(DISPLAY, EVENTS.displayPing, null).catch((err) => {
        console.warn("[show] ping failed", err);
      });
    }, 80);
  });
}

/**
 * Creates/shows the Display window and waits until its webview has registered listeners.
 */
export async function ensureDisplayReady(): Promise<void> {
  await watchReadyEvents();

  if (monitorIndex != null) {
    console.log("[show] set_display_monitor", monitorIndex);
    await invoke("set_display_monitor", { index: monitorIndex }).catch((err) => {
      console.warn("[show] set_display_monitor failed", err);
    });
  }

  console.log("[show] invoke ensure_display…", { alreadyReady: displayReady });
  await invoke("ensure_display");
  console.log("[show] ensure_display ok");

  if (displayReady) {
    console.log("[show] display already ready — skip wait");
    return;
  }

  console.log("[show] waiting for display:ready…");
  try {
    await waitForReadySignal();
  } catch (err) {
    console.warn("[show] ready wait failed, short fallback delay", err);
    logWarn("display", "окно вывода не подтвердило готовность, работаем с задержкой", {
      error: String(err),
    });
    await new Promise((r) => window.setTimeout(r, 600));
  }

  console.log("[show] ensureDisplayReady done", { displayReady });
}

export async function sendToDisplay<T>(event: string, payload: T): Promise<void> {
  console.log("[show] sendToDisplay start", event, payload);
  await ensureDisplayReady();
  console.log("[show] → emitTo", DISPLAY, event);
  try {
    await emitTo(DISPLAY, event, payload);
    console.log("[show] emitTo done", event);
  } catch (err) {
    console.error("[show] emitTo FAILED", event, err);
    throw err;
  }
  // Mirror to preview iframe so it always shows what's live on the display
  if (previewFrame) {
    postToPreview(previewFrame, {
      channel: PREVIEW_CHANNEL,
      type: event as any,
      payload,
    } as any);
  }
}

export async function closeDisplayWindow(): Promise<void> {
  console.log("[show] close_display");
  markDisplayClosed();
  await invoke("close_display").catch((err) => {
    console.warn("[show] close_display failed", err);
  });
}
