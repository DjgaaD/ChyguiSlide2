import {
  EVENTS,
  PREVIEW_CHANNEL,
  type PreviewMessage,
  type SetStylePayload,
  type SetTextPayload,
} from "../shared/events";
import { cleanSongLines } from "../shared/style";

export function postToPreview(iframe: HTMLIFrameElement, message: PreviewMessage) {
  iframe.contentWindow?.postMessage(message, "*");
}

export function previewSetText(iframe: HTMLIFrameElement, payload: SetTextPayload) {
  postToPreview(iframe, {
    channel: PREVIEW_CHANNEL,
    type: EVENTS.setText,
    payload,
  });
}

/**
 * Единый payload слайда для превью — точная копия того, что получает окно вывода.
 * Правила те же, что в `renderText` (`src/display/main.ts`):
 * — заголовок служебный и на экран никогда не попадает;
 * — для песен строки проходят жёсткую фильтрацию (`cleanSongLines`);
 * — подпись стиха (`verseRef`) передаётся всегда, когда есть: без неё в превью
 *   пропадает ссылка, которую рисует активный стиль.
 */
export function slideTextPayload(payload: SetTextPayload): SetTextPayload {
  return {
    lines:
      payload.mode === "song" ? cleanSongLines(payload.lines, payload.title) : payload.lines,
    mode: payload.mode,
    ...(payload.verseRef ? { verseRef: payload.verseRef } : {}),
  };
}

/** Отправка слайда в превью по единым правилам (см. `slideTextPayload`). */
export function previewSetSlide(iframe: HTMLIFrameElement, payload: SetTextPayload) {
  previewSetText(iframe, slideTextPayload(payload));
}

export function previewClear(iframe: HTMLIFrameElement) {
  // textOnly: очищаем только текстовый слой — фон активного стиля
  // (анимированный/медиа) в превью должен сохраняться.
  postToPreview(iframe, {
    channel: PREVIEW_CHANNEL,
    type: EVENTS.clear,
    payload: { textOnly: true },
  });
}

/** Full clear — removes both text AND media backgrounds. Use on boot/initialization. */
export function previewFullClear(iframe: HTMLIFrameElement) {
  postToPreview(iframe, {
    channel: PREVIEW_CHANNEL,
    type: EVENTS.clear,
    payload: {},
  });
}

export function previewSetStyle(iframe: HTMLIFrameElement, payload: SetStylePayload) {
  postToPreview(iframe, {
    channel: PREVIEW_CHANNEL,
    type: EVENTS.setStyle,
    payload,
  });
}

/** Apply target monitor aspect to the preview container (mockup `.preview-box`). */
export function applyPreviewAspect(
  iframe: HTMLIFrameElement,
  width: number,
  height: number,
) {
  const box = iframe.parentElement;
  if (box) {
    box.style.aspectRatio = `${width} / ${height}`;
  }
  iframe.style.width = "100%";
  iframe.style.height = "100%";
  iframe.style.pointerEvents = "none";
  iframe.style.border = "0";
  iframe.style.display = "block";
  iframe.style.background = "#000";
}
