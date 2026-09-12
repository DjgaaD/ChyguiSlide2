import {
  EVENTS,
  PREVIEW_CHANNEL,
  type PreviewMessage,
  type SetStylePayload,
  type SetTextPayload,
} from "../shared/events";

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
