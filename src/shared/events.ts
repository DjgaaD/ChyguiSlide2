import type { StyleConfig } from "./style";

export const EVENTS = {
  setText: "display:set-text",
  clear: "display:clear",
  setMedia: "display:set-media",
  mediaControl: "display:media-control",
  videoSeek: "video:seek",
  videoSetLoop: "video:set-loop",
  setOverlay: "display:set-overlay",
  /** Push active presentation style (text, stroke, transitions, background). */
  setStyle: "display:set-style",
  mediaStatus: "controller:media-status",
  /** Display webview finished wiring listeners and can receive emitTo. */
  displayReady: "display:ready",
  /** Ask an already-open Display to re-emit display:ready. */
  displayPing: "display:ping",
} as const;

export type TextMode = "bible" | "song" | "announcement";

export type SetTextPayload = {
  /** Optional heading — shown above the text (not used for songs). */
  title?: string;
  lines: string[];
  mode: TextMode;
  /** Bible reference ("Ин 3:16") — rendered per the active style caption settings. */
  verseRef?: string;
};

/** Full presentation style pushed to Display / Preview when activated or changed. */
export type SetStylePayload = StyleConfig;

export type SetMediaPayload = {
  kind: "video" | "image" | "none";
  path?: string;
};

export type MediaControlPayload = {
  action: "play" | "pause" | "seek" | "volume" | "fade-out";
  value?: number;
};

export type VideoSeekPayload = {
  time: number;
};

export type VideoLoopPayload = {
  loop: boolean;
};

export type OverlayPayload = {
  opacity: number;
};

/** textOnly: очистить только текст, фон (медиа) оставить. */
export type ClearPayload = {
  textOnly?: boolean;
};

export type MediaStatusPayload = {
  currentTime: number;
  duration: number;
  paused: boolean;
};

/** Local iframe preview protocol — never use tauri.emit for preview. */
export type PreviewMessage =
  | { channel: "chyguislide-preview"; type: typeof EVENTS.setText; payload: SetTextPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.clear; payload?: ClearPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.setMedia; payload: SetMediaPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.mediaControl; payload: MediaControlPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.videoSeek; payload: VideoSeekPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.videoSetLoop; payload: VideoLoopPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.setOverlay; payload: OverlayPayload }
  | { channel: "chyguislide-preview"; type: typeof EVENTS.setStyle; payload: SetStylePayload };

export const PREVIEW_CHANNEL = "chyguislide-preview";

export type MonitorInfo = {
  index: number;
  name: string;
  width: number;
  height: number;
  isPrimary: boolean;
  scaleFactor: number;
  x: number;
  y: number;
};
