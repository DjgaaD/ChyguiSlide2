import { invoke, listen, open } from "../shared/ipc";
import { logInfo } from "../shared/logger";
import {
  EVENTS,
  type ClearPayload,
  type MediaControlPayload,
  type MediaStatusPayload,
  type SetMediaPayload,
  type SetTextPayload,
  type VideoLoopPayload,
  type VideoSeekPayload,
} from "../shared/events";
import {
  applyPreviewAspect,
  previewClear,
  previewFullClear,
  previewSetSlide,
} from "./preview-frame";
import { closeDisplayWindow, onDisplayReady, sendToDisplay, setPreviewFrame } from "./display-bridge";
import { applyActiveStyleToOutputs } from "./styles";
import { cleanSongLines } from "../shared/style";

export type SongDetail = {
  id: number;
  title: string;
  slides: string[];
  collection_id?: number | null;
};

export type QuickItem = {
  key: string;
  kind: "song" | "media" | "text";
  title: string;
  songId?: number;
  mediaPath?: string;
  mediaKind?: "image" | "video";
  /** Текстовые элементы (объявление/стих Библии): слайды и режим вывода. */
  slides?: string[];
  textMode?: "announcement" | "bible";
  verseRef?: string;
  /** Ссылка на каждый слайд отдельно (для показа главы стих за стихом). */
  verseRefs?: string[];
};

type PlaylistSummary = { id: number; name: string; itemCount: number };
type PlaylistItemRow = {
  kind: string;
  songId?: number | null;
  mediaPath?: string | null;
  mediaKind?: string | null;
  title: string;
};
type PlaylistDetail = { id: number; name: string; items: PlaylistItemRow[] };

export type BroadcastHooks = {
  persistentEnabled: () => boolean;
  slideLabel: (text: string, index: number) => string;
  bumpShow: (songId: number) => void;
  refreshIcons: () => void;
  previewAspect: () => { width: number; height: number };
};

let hooks: BroadcastHooks;
let quick: QuickItem[] = [];
let saved: PlaylistSummary[] = [];
let selectedKey = "";
let selectedSong: SongDetail | null = null;
let selectedText: {
  title: string;
  slides: string[];
  mode: "announcement" | "bible";
  verseRef?: string;
  verseRefs?: string[];
} | null = null;
let selectedSlide = -1;
let live = false;
let liveKey = "";
let liveSlide = -1;
let liveHasMedia = false;
/**
 * Слайд, который сейчас реально стоит на экране (null — экран пуст). По нему
 * гасим повторные отправки: клик по уже активному слайду не должен перестраивать
 * DOM окна вывода, иначе сбрасываются анимации перехода и текст заметно мерцает.
 * Тем же объектом восстанавливаем превью, если его iframe перезагрузился.
 */
let liveSlidePayload: SetTextPayload | null = null;
let pendingText: SetTextPayload | null = null;
let pendingMedia: SetMediaPayload | null = null;
/** Состояние оптимизации MP4 в «Быстром плейлисте» (ключ — путь к файлу). */
const quickVideoChecks = new Map<string, boolean>(); // true = faststart, false = нужна конвертация
const quickVideoChecking = new Set<string>();
const quickVideoConverting = new Set<string>();
const quickVideoProgress = new Map<string, number>();

/**
 * Время плавного гашения кадра в окне вывода (см. `#display-root` в display.css):
 * столько ждём перед закрытием окна, иначе финал показа обрывается.
 */
const DISPLAY_FADE_MS = 400;

function $(sel: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(sel);
  if (!el) {
    throw new Error(`Missing ${sel}`);
  }
  return el;
}

function frame(): HTMLIFrameElement {
  return $("#bc-preview-frame") as HTMLIFrameElement;
}

function fileName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

/**
 * Подпись текста слайда: всё, что видно на экране (строки, режим и подпись стиха).
 * Совпадение подписей означает, что слайд на экране уже показан и перерисовывать
 * его не нужно. Пустой экран (`null`) даёт пустую подпись.
 */
function textSignature(payload: SetTextPayload | null): string {
  return payload ? JSON.stringify([payload.mode, payload.verseRef ?? "", payload.lines]) : "";
}

export function mediaKindFromPath(path: string): "image" | "video" {
  if (/\.(mp4|m4v|webm|mkv|avi|mov|ogg|ogv|ts|m2ts|mpg|mpeg|flv|wmv|3gp)$/i.test(path)) {
    return "video";
  }
  return "image";
}

// Фильтрация строк песен — общая утилита cleanSongLines из shared/style.

function updateSlideStripe() {
  const host = document.getElementById("bc-slide-host");
  const stripe = document.getElementById("bc-slide-stripe");
  const selected = document.querySelector<HTMLElement>("#bc-slide-list .slide-item.selected");
  if (!host || !stripe) {
    return;
  }
  if (!selected) {
    stripe.style.opacity = "0";
    return;
  }
  stripe.style.opacity = "1";
  const hostTop = host.getBoundingClientRect().top;
  const itemTop = selected.getBoundingClientRect().top;
  const offset =
    itemTop - hostTop + host.scrollTop + (selected.offsetHeight - 40) / 2;
  stripe.style.top = `${Math.max(0, offset)}px`;
}

function setPreviewIdle(text: string) {
  const idle = document.getElementById("bc-preview-idle");
  if (!idle) {
    return;
  }
  idle.textContent = text;
  idle.removeAttribute("hidden");
}

function hidePreviewIdle() {
  document.getElementById("bc-preview-idle")?.setAttribute("hidden", "");
}

/**
 * Возвращает текущий живой слайд на превью и на экран. Нужно после перезагрузки
 * iframe превью или пересоздания окна вывода (Esc в окне Display): без этого
 * «Сейчас на экране» и сам экран показывают разное.
 *
 * Повтор для окна вывода, которое уже показывает этот слайд, гасится проверкой
 * `isSameSlide` в `src/display/main.ts`, поэтому мерцания не будет.
 */
function restoreLiveSlide(): void {
  const payload = liveSlidePayload;
  if (!payload) {
    return;
  }
  hidePreviewIdle();
  // Превью зеркалит команду внутри `sendToDisplay`, поэтому отдельная отправка
  // нужна только как запасной путь — когда окно вывода поднять не удалось.
  void sendToDisplay(EVENTS.setText, payload).catch((err) => {
    console.warn("[show] restore live slide failed", err);
    previewSetSlide(frame(), payload);
  });
}

function syncLiveUi() {
  const idle = document.getElementById("bc-idle-actions");
  const liveActions = document.getElementById("bc-live-actions");
  const media = document.getElementById("bc-media-controls");
  const start = document.getElementById("bc-start-show") as HTMLButtonElement | null;
  // Пока показа нет — только «Начать показ»; во время показа — Prev/Next/Clear/End.
  if (idle) {
    idle.hidden = live;
  }
  if (liveActions) {
    liveActions.hidden = !live;
  }
  if (media) {
    media.hidden = !(live && liveHasMedia);
  }
  if (start) {
    start.disabled = !(pendingText || pendingMedia);
  }
}

function syncVideoPosition(payload: MediaStatusPayload) {
  const seek = document.getElementById("bc-video-seek") as HTMLInputElement | null;
  if (!seek || !Number.isFinite(payload.duration) || payload.duration <= 0) {
    return;
  }
  seek.max = String(payload.duration);
  seek.value = String(Math.min(payload.duration, Math.max(0, payload.currentTime)));
}

async function restoreAfterQuickMedia(itemKey: string): Promise<void> {
  if (!live || liveKey !== itemKey || !liveHasMedia) {
    return;
  }
  await sendToDisplay(EVENTS.setMedia, { kind: "none" } satisfies SetMediaPayload).catch((err) => {
    console.warn("[show] quick media clear failed", err);
  });
  // Reapply the active style so Keep Background restores the style background.
  await applyActiveStyleToOutputs({ background: true });
  if (!hooks.persistentEnabled()) {
    await closeDisplayWindow();
  }
  live = false;
  liveKey = "";
  liveSlide = -1;
  liveHasMedia = false;
  liveSlidePayload = null;
  setPreviewIdle("Нет сигнала");
  syncLiveUi();
  renderQuickList();
}

/**
 * Проверяем все видеофайлы, а не только MP4: контейнер может не совпадать
 * с расширением (например MPEG-TS, переименованный в `.mp4`), и тогда
 * браузер покажет чёрный экран вместо картинки.
 */
function isVideoFile(path: string): boolean {
  return mediaKindFromPath(path) === "video";
}

/** Автопроверка добавленного видео: играется как есть или нужна конвертация. */
function ensureQuickVideoCheck(path: string): void {
  if (!isVideoFile(path)) {
    return;
  }
  if (quickVideoChecks.has(path) || quickVideoChecking.has(path) || quickVideoConverting.has(path)) {
    return;
  }
  quickVideoChecking.add(path);
  void invoke<boolean>("check_video_optimization", { path })
    .then((optimized) => {
      quickVideoChecks.set(path, optimized);
      quickVideoChecking.delete(path);
      renderQuickList();
    })
    .catch((error) => {
      // Ошибка проверки — не блокируем файл, чтобы не показывать ложную кнопку.
      console.warn("[broadcast] video optimization check failed", error);
      quickVideoChecks.set(path, true);
      quickVideoChecking.delete(path);
      renderQuickList();
    });
}

function convertQuickVideo(path: string): void {
  if (quickVideoConverting.has(path)) {
    return;
  }
  quickVideoConverting.add(path);
  quickVideoProgress.set(path, 0);
  renderQuickList();
  void invoke("optimize_video", { path }).catch((error) => {
    quickVideoConverting.delete(path);
    quickVideoChecks.set(path, false);
    window.alert(`Ошибка конвертации: ${String(error)}`);
    renderQuickList();
  });
}

/** После конвертации файл перезаписан: если он сейчас в эфире — перезапустить вывод. */
async function resendLiveMediaIfPath(path: string): Promise<void> {
  if (!live || !liveHasMedia) {
    return;
  }
  const liveItem = quick.find((q) => q.key === liveKey);
  if (liveItem?.kind === "media" && liveItem.mediaPath === path) {
    await sendToDisplay(EVENTS.setMedia, { kind: "video", path } satisfies SetMediaPayload).catch(() => undefined);
  }
}

function renderQuickList() {
  const list = $("#quick-playlist");
  list.replaceChildren();
  if (quick.length === 0) {
    const li = document.createElement("li");
    li.className = "list-status";
    li.textContent = "Пусто — добавьте песню или медиа";
    list.appendChild(li);
    return;
  }
  for (const item of quick) {
    const li = document.createElement("li");
    li.className = "playlist-item" + (item.key === selectedKey ? " selected" : "");
    if (live && item.key === liveKey) {
      li.classList.add("live-now");
    }
    const title = document.createElement("span");
    title.className = "item-title";
    title.textContent = item.title;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "item-remove";
    remove.title = "Удалить";
    remove.innerHTML = `<i data-lucide="trash-2"></i>`;
    remove.addEventListener("click", (event) => {
      event.stopPropagation();
      void restoreAfterQuickMedia(item.key).then(() => {
        quick = quick.filter((q) => q.key !== item.key);
        if (selectedKey === item.key) {
          clearSelection();
        }
        renderQuickList();
        hooks.refreshIcons();
      });
    });
    li.appendChild(title);
    const actions = document.createElement("div");
    actions.className = "item-actions";
    // Видео: авто-проверка faststart, кнопка «Конвертировать» и прогресс.
    if (item.kind === "media" && item.mediaKind === "video" && item.mediaPath) {
      ensureQuickVideoCheck(item.mediaPath);
      if (quickVideoChecks.get(item.mediaPath) === false) {
        if (quickVideoConverting.has(item.mediaPath)) {
          const percent = Math.round(quickVideoProgress.get(item.mediaPath) || 0);
          const convert = document.createElement("div");
          convert.className = "quick-convert";
          convert.title = "Оптимизация видео…";
          convert.innerHTML = `<div class="quick-convert-track"><i style="width:${percent}%"></i></div><span>${percent}%</span>`;
          actions.appendChild(convert);
        } else {
          const convert = document.createElement("button");
          convert.type = "button";
          convert.className = "quick-convert-btn";
          convert.textContent = "Конвертировать";
          convert.title = "Оптимизировать видео (moov в начало файла)";
          convert.addEventListener("click", (event) => {
            event.stopPropagation();
            convertQuickVideo(item.mediaPath!);
          });
          actions.appendChild(convert);
        }
      }
    }
    actions.appendChild(remove);
    li.appendChild(actions);
    li.addEventListener("click", () => void selectQuickItem(item.key));
    list.appendChild(li);
  }
  hooks.refreshIcons();
}

async function renderSavedList() {
  saved = await invoke<PlaylistSummary[]>("list_playlists").catch(() => []);
  const list = $("#saved-playlists");
  list.replaceChildren();
  if (saved.length === 0) {
    const li = document.createElement("li");
    li.className = "list-status";
    li.textContent = "Нет сохранённых плейлистов";
    list.appendChild(li);
    return;
  }
  for (const pl of saved) {
    const li = document.createElement("li");
    li.className = "playlist-item";
    const title = document.createElement("span");
    title.className = "item-title";
    title.textContent = `${pl.name} (${pl.itemCount})`;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "item-remove";
    remove.title = "Удалить плейлист";
    remove.innerHTML = `<i data-lucide="trash-2"></i>`;
    remove.addEventListener("click", async (event) => {
      event.stopPropagation();
      if (!window.confirm(`Удалить плейлист «${pl.name}»?`)) {
        return;
      }
      await invoke("delete_playlist", { id: pl.id });
      await renderSavedList();
    });
    li.appendChild(title);
    li.appendChild(remove);
    li.addEventListener("click", () => void loadSavedPlaylist(pl.id));
    list.appendChild(li);
  }
  hooks.refreshIcons();
}

function clearSelection() {
  selectedKey = "";
  selectedSong = null;
  selectedText = null;
  selectedSlide = -1;
  pendingText = null;
  pendingMedia = null;
  $("#bc-empty").hidden = false;
  $("#bc-detail").hidden = true;
  $("#bc-slide-list").replaceChildren();
  previewClear(frame());
  setPreviewIdle("Нет сигнала");
  syncLiveUi();
}

async function selectQuickItem(key: string) {
  const item = quick.find((q) => q.key === key);
  if (!item) {
    return;
  }
  selectedKey = key;
  renderQuickList();
  $("#bc-empty").hidden = true;
  $("#bc-detail").hidden = false;
  $("#bc-title").textContent = item.title;

  if (item.kind === "song" && item.songId != null) {
    $("#bc-subtitle").textContent = "Слайды";
    selectedSong = await invoke<SongDetail | null>("get_song", { id: item.songId });
    if (!selectedSong) {
      clearSelection();
      window.alert("Песня не найдена.");
      return;
    }
    selectedSlide = 0;
    selectedText = null;
    pendingMedia = null;
    renderSlides();
    pickSlide(0, false);
    return;
  }

  if (item.kind === "text") {
    $("#bc-subtitle").textContent = "Слайды";
    selectedText = {
      title: item.title,
      slides: [...(item.slides ?? [])],
      mode: item.textMode ?? "announcement",
      verseRef: item.verseRef,
      verseRefs: item.verseRefs ? [...item.verseRefs] : undefined,
    };
    selectedSong = null;
    selectedSlide = 0;
    pendingMedia = null;
    renderSlides();
    pickSlide(0, false);
    return;
  }

  selectedSong = null;
  selectedText = null;
  selectedSlide = -1;
  $("#bc-subtitle").textContent = item.mediaKind === "video" ? "Видео" : "Изображение";
  $("#bc-slide-list").replaceChildren();
  document.getElementById("bc-slide-stripe")!.style.opacity = "0";
  pendingText = null;
  pendingMedia = {
    kind: item.mediaKind || "image",
    path: item.mediaPath,
  };
  const seek = document.getElementById("bc-video-seek") as HTMLInputElement | null;
  if (seek) {
    seek.max = "0";
    seek.value = "0";
  }
  const loop = document.getElementById("bc-video-loop") as HTMLInputElement | null;
  if (loop) {
    loop.checked = false;
  }
  previewClear(frame());
  setPreviewIdle(item.mediaKind === "video" ? "Видеофон готов к показу" : "Фон готов к показу");
  syncLiveUi();
}

function currentSlideSource(): string[] {
  if (selectedSong) {
    return selectedSong.slides;
  }
  if (selectedText) {
    return selectedText.slides;
  }
  return [];
}

function renderSlides() {
  const slides = currentSlideSource();
  if (slides.length === 0) {
    return;
  }
  const list = $("#bc-slide-list");
  list.replaceChildren();
  slides.forEach((slide, index) => {
    const label = hooks.slideLabel(slide, index);
    const body = slide.startsWith(label) ? slide.slice(label.length).trim() : slide;
    const li = document.createElement("li");
    const isSelected = index === selectedSlide;
    const isLive = live && selectedKey === liveKey && index === liveSlide;
    li.className =
      "slide-item" +
      (isSelected ? " selected" : "") +
      (isLive ? " live-now" : "");
    li.dataset.key = String(index);
    li.innerHTML = `<div class="slide-label">${label}</div><div class="slide-text">${body || slide}</div>`;
    li.addEventListener("click", () => {
      void pickSlide(index, live && selectedKey === liveKey);
    });
    list.appendChild(li);
  });
  requestAnimationFrame(updateSlideStripe);
}

async function pickSlide(index: number, sendLive: boolean) {
  const slides = currentSlideSource();
  if (index < 0 || index >= slides.length) {
    return;
  }
  selectedSlide = index;
  const slide = slides[index];
  const allLines = slide.split("\n").filter((l) => l.length > 0);
  let lines = allLines;
  const mode: "song" | "announcement" | "bible" = selectedSong
    ? "song"
    : selectedText?.mode ?? "song";
  // Для режима «Библия» — ссылка на конкретный слайд (стих), иначе единая ссылка элемента.
  const verseRef = selectedText?.verseRefs?.[index] ?? selectedText?.verseRef;
  if (selectedSong) {
    // Жёстко: только текст песни — без «Куплет N», «Припев», «Хор» и названия песни.
    lines = cleanSongLines(allLines, selectedSong.title);
  }
  pendingText = {
    lines,
    mode,
    ...(verseRef ? { verseRef } : {}),
  };
  pendingMedia = null;
  renderSlides();
  syncLiveUi();

  if (sendLive) {
    liveSlide = index;
    const payload: SetTextPayload = {
      lines,
      mode,
      ...(verseRef ? { verseRef } : {}),
    };
    const signature = textSignature(payload);
    if (signature === textSignature(liveSlidePayload)) {
      // Этот слайд уже стоит на экране: DOM окна вывода не трогаем, иначе
      // перезапускаются анимации перехода и текст «моргает».
      logInfo("show", "слайд уже на экране — повторная отправка пропущена", { index });
    } else {
      console.log("[show] pickSlide → live send", { index, title: selectedSong?.title ?? selectedText?.title });
      liveSlidePayload = payload;
      if (selectedSong) {
        hooks.bumpShow(selectedSong.id);
      }
      await sendToDisplay(EVENTS.setText, payload);
    }
    renderSlides();
  }
  // Ensure the active slide stays visible in the scrollable host (#bc-slide-host)
  const activeEl = document.querySelector<HTMLElement>(`#bc-slide-list .slide-item[data-key="${index}"]`);
  const slideHost = document.getElementById("bc-slide-host");
  if (activeEl && slideHost) {
    const targetScroll = activeEl.offsetTop - (slideHost.clientHeight / 2) + (activeEl.clientHeight / 2);
    slideHost.scrollTo({ top: Math.max(0, targetScroll), behavior: "smooth" });
  }
}

async function startShow() {
  console.log("[show] startShow click", {
    hasMedia: !!pendingMedia?.path,
    hasText: !!pendingText,
    songId: selectedSong?.id,
    slide: selectedSlide,
  });
  // На старте показа всегда явно пересылаем активный стиль (в т.ч. фон),
  // чтобы превью/Display не остались без фона, если стиль не успел до них дойти.
  await applyActiveStyleToOutputs({ background: true }).catch(() => undefined);
  if (pendingMedia?.path) {
    live = true;
    liveKey = selectedKey;
    liveSlide = -1;
    liveHasMedia = true;
    liveSlidePayload = null;
    hidePreviewIdle();
    await sendToDisplay(EVENTS.setMedia, pendingMedia);
    syncLiveUi();
    renderQuickList();
    return;
  }
  if (!pendingText || (!selectedSong && !selectedText)) {
    console.warn("[show] startShow aborted — nothing pending");
    return;
  }
  live = true;
  liveKey = selectedKey;
  liveSlide = selectedSlide;
  hidePreviewIdle();
  if (selectedSong) {
    hooks.bumpShow(selectedSong.id);
  }
  const payload: SetTextPayload = {
    lines: pendingText.lines,
    mode: pendingText.mode,
    ...(pendingText.verseRef ? { verseRef: pendingText.verseRef } : {}),
  };
  // Слайд уходит на экран — запоминаем его, чтобы клик по нему же не
  // перерисовывал DOM (см. `pickSlide`) и чтобы превью могло восстановиться.
  liveSlidePayload = payload;
  await sendToDisplay(EVENTS.setText, payload);
  syncLiveUi();
  renderSlides();
  renderQuickList();
}

async function clearTextOnly() {
  console.log("[show] clearTextOnly");
  const payload: ClearPayload = { textOnly: true };
  // Превью зеркалит эту же команду внутри sendToDisplay — отдельная команда
  // превью привела бы к тому, что «Сейчас на экране» и экран расходились.
  await sendToDisplay(EVENTS.clear, payload).catch((err) => {
    console.warn("[show] clear failed", err);
    previewClear(frame());
  });
  // Текста на экране больше нет — тот же слайд снова должен уйти в эфир.
  liveSlidePayload = null;
  if (pendingMedia) {
    setPreviewIdle("Фон на экране");
  } else {
    setPreviewIdle("Нет сигнала");
  }
}

async function endShow() {
  console.log("[show] endShow", { persistent: hooks.persistentEnabled() });
  const liveItem = quick.find((item) => item.key === liveKey);
  const liveQuickMedia = liveHasMedia && liveItem?.kind === "media";
  if (liveQuickMedia) {
    await restoreAfterQuickMedia(liveKey);
    return;
  }
  const textOnly = hooks.persistentEnabled();
  // Та же команда уходит и в превью: при `textOnly: false` оно тоже обязано
  // убрать фон, иначе «Сейчас на экране» показывает картинку, которой на экране нет.
  await sendToDisplay(EVENTS.clear, { textOnly } satisfies ClearPayload).catch((err) => {
    console.warn("[show] end clear failed", err);
    if (textOnly) {
      previewClear(frame());
    } else {
      previewFullClear(frame());
    }
  });
  if (!textOnly) {
    // Даём окну вывода погасить кадр (fade-out в display.css), иначе финал
    // показа обрывается мгновенным закрытием окна.
    await new Promise((resolve) => window.setTimeout(resolve, DISPLAY_FADE_MS));
    await closeDisplayWindow();
    pendingMedia = null;
    liveHasMedia = false;
  }
  live = false;
  liveKey = "";
  liveSlide = -1;
  // Экран пуст: следующий показ того же слайда обязан снова уйти в окно вывода.
  liveSlidePayload = null;
  setPreviewIdle("Нет сигнала");
  syncLiveUi();
  renderSlides();
  renderQuickList();
}

async function step(delta: number) {
  const slides = currentSlideSource();
  if (slides.length === 0 || !live || selectedKey !== liveKey) {
    return;
  }
  const last = slides.length - 1;
  // Автоматическое завершение показа: «Следующий»/ArrowRight на последнем слайде
  // останавливает показ (как Esc → endShow), вместо бездействия на месте.
  // Исключение: для Библии — загружаем следующую главу.
  if (delta > 0 && liveSlide >= last) {
    const current = quick.find((q) => q.key === liveKey);
    console.log("[step] Bible transition debug:", {
      delta,
      liveSlide,
      last,
      liveKey,
      current,
      kind: current?.kind,
      textMode: current?.textMode,
      verseRefs: current?.verseRefs,
      selectedTextExists: !!selectedText,
    });
    if (current?.textMode === "bible" && selectedText && current.verseRefs && current.verseRefs.length > 0) {
      // Get the last verse reference to determine current book/chapter
      const lastVerseRef = current.verseRefs[current.verseRefs.length - 1];
      // Robust parsing: split from the right to handle book names with spaces/numbers
      // Format: "Book Name Chapter:Verse" (e.g., "Genesis 1:31", "1 Corinthians 5:10")
      const colonIdx = lastVerseRef.lastIndexOf(":");
      console.log("[step] Parsing verseRef:", { lastVerseRef, colonIdx });
      if (colonIdx > 0) {
        const beforeColon = lastVerseRef.slice(0, colonIdx);
        const spaceIdx = beforeColon.lastIndexOf(" ");
        console.log("[step] Parsing beforeColon:", { beforeColon, spaceIdx });
        if (spaceIdx > 0) {
          const book = beforeColon.slice(0, spaceIdx).trim();
          const chapter = parseInt(beforeColon.slice(spaceIdx + 1), 10);
          console.log("[step] Parsed book/chapter:", { book, chapter, isNaN: isNaN(chapter) });
          if (book && !isNaN(chapter)) {
            // Try to fetch next chapter
            console.log("[step] Calling get_next_bible_chapter...");
            const result = await invoke<[string, number, { book: string; chapter: number; verse: number; text: string }[]] | null>(
              "get_next_bible_chapter",
              { book, chapter }
            ).catch((err) => {
              console.error("[step] get_next_bible_chapter error:", err);
              return null;
            });
            const [nextBook, nextChapterNum, verses] = result ?? [];
            console.log("[step] get_next_bible_chapter result:", { nextBook, nextChapterNum, verses });
            if (verses && verses.length > 0) {
              // Update the current item with new chapter slides
              const newSlides = verses.map((v) => v.text);
              const newVerseRefs = verses.map((v) => `${v.book} ${v.chapter}:${v.verse}`);

              // Update both QuickItem AND selectedText (currentSlideSource returns selectedText.slides)
              current.slides = newSlides;
              current.verseRefs = newVerseRefs;
              current.verseRef = newVerseRefs[0];
              current.title = `${nextBook} · Глава ${nextChapterNum}`;

              // Update selectedText in-place so currentSlideSource() returns the new slides
              selectedText.slides = newSlides;
              selectedText.verseRefs = newVerseRefs;
              selectedText.verseRef = newVerseRefs[0];
              selectedText.title = current.title;

              console.log("[step] Transitioning to next chapter:", { title: current.title, slideCount: newSlides.length });
              renderSlides();
              renderQuickList();
              await pickSlide(0, true);
              return;
            } else {
              console.log("[step] No verses in nextChapter, ending show");
            }
          } else {
            console.log("[step] Invalid book/chapter, skipping");
          }
        } else {
          console.log("[step] No space found in beforeColon, skipping");
        }
      } else {
        console.log("[step] No colon found in verseRef, skipping");
      }
    } else {
      console.log("[step] Bible transition conditions not met:", {
        isBible: current?.textMode === "bible",
        hasSelectedText: !!selectedText,
        hasVerseRefs: !!(current?.verseRefs && current.verseRefs.length > 0),
      });
    }
    await endShow();
    return;
  }
  const next = Math.min(last, Math.max(0, liveSlide + delta));
  await pickSlide(next, true);
}

export async function addSongToQuickPlaylist(song: SongDetail, slideIndex = 0) {
  logInfo("playlist", `песня «${song.title}» → быстрый плейлист (слайд ${slideIndex})`);
  const key = `song-${song.id}`;
  if (!quick.some((q) => q.key === key)) {
    quick.unshift({
      key,
      kind: "song",
      title: song.title,
      songId: song.id,
    });
  }
  renderQuickList();
  await selectQuickItem(key);
  if (slideIndex > 0) {
    await pickSlide(slideIndex, false);
  }
}

/** Проверка: есть ли песня в «Быстром плейлисте» (для UI-состояния кнопки). */
export function isSongInQuickPlaylist(songId: number): boolean {
  return quick.some((q) => q.kind === "song" && q.songId === songId);
}

export async function openSongInBroadcast(
  songId: number,
  goLiveNow: boolean,
  slideIndex = 0,
) {
  const song = await invoke<SongDetail | null>("get_song", { id: songId });
  if (!song) {
    return;
  }
  logInfo("show", `песня «${song.title}» → трансляция (сразу в эфир: ${goLiveNow})`);
  await addSongToQuickPlaylist(song, slideIndex);
  if (goLiveNow) {
    await startShow();
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
    const kind = mediaKindFromPath(path);
    const key = `media-${path}`;
    if (quick.some((q) => q.key === key)) {
      continue;
    }
    quick.unshift({
      key,
      kind: "media",
      title: fileName(path),
      mediaPath: path,
      mediaKind: kind,
    });
  }
  renderQuickList();
  if (paths[0]) {
    await selectQuickItem(`media-${paths[0]}`);
  }
}

async function saveQuickPlaylist() {
  if (quick.length === 0) {
    window.alert("Быстрый плейлист пуст.");
    return;
  }
  const name = window.prompt("Название плейлиста:");
  if (!name || !name.trim()) {
    return;
  }
  const items: PlaylistItemRow[] = quick.map((q) => ({
    kind: q.kind,
    songId: q.songId ?? null,
    mediaPath: q.mediaPath ?? null,
    mediaKind: q.mediaKind ?? null,
    title: q.title,
  }));
  try {
    await invoke("create_playlist", { name: name.trim(), items });
    await renderSavedList();
    window.alert("Плейлист сохранён.");
  } catch (error) {
    window.alert(String(error));
  }
}

async function loadSavedPlaylist(id: number) {
  const detail = await invoke<PlaylistDetail | null>("get_playlist", { id });
  if (!detail) {
    return;
  }
  for (const item of detail.items) {
    if (item.kind === "song" && item.songId != null) {
      const key = `song-${item.songId}`;
      if (!quick.some((q) => q.key === key)) {
        quick.push({
          key,
          kind: "song",
          title: item.title || `Песня ${item.songId}`,
          songId: item.songId,
        });
      }
    } else if (item.kind === "media" && item.mediaPath) {
      const key = `media-${item.mediaPath}`;
      if (!quick.some((q) => q.key === key)) {
        quick.push({
          key,
          kind: "media",
          title: item.title || fileName(item.mediaPath),
          mediaPath: item.mediaPath,
          mediaKind: (item.mediaKind as "image" | "video") || mediaKindFromPath(item.mediaPath),
        });
      }
    }
  }
  renderQuickList();
  if (quick[0]) {
    await selectQuickItem(quick[0].key);
  }
}

function resetQuickPlaylist() {
  if (quick.length > 0 && !window.confirm("Сбросить быстрый плейлист?")) {
    return;
  }
  quick = [];
  clearSelection();
  renderQuickList();
}

export function bindBroadcast(h: BroadcastHooks) {
  hooks = h;
  applyPreviewAspect(frame(), hooks.previewAspect().width, hooks.previewAspect().height);

  $("#bc-add-media").addEventListener("click", () => void addMediaFiles());
  $("#bc-save-playlist").addEventListener("click", () => void saveQuickPlaylist());
  $("#bc-reset-quick").addEventListener("click", () => resetQuickPlaylist());
  $("#bc-start-show").addEventListener("click", () => {
    console.log("[show] click Начать показ");
    void startShow();
  });
  $("#bc-prev").addEventListener("click", () => void step(-1));
  $("#bc-next").addEventListener("click", () => void step(1));
  $("#bc-clear").addEventListener("click", () => void clearTextOnly());
  $("#bc-end").addEventListener("click", () => void endShow());

  $("#bc-media-play").addEventListener("click", () => {
    void sendToDisplay(EVENTS.mediaControl, {
      action: "play",
    } satisfies MediaControlPayload);
  });
  $("#bc-media-pause").addEventListener("click", () => {
    void sendToDisplay(EVENTS.mediaControl, {
      action: "pause",
    } satisfies MediaControlPayload);
  });
  $("#bc-video-seek").addEventListener("input", (event) => {
    const time = Number((event.target as HTMLInputElement).value);
    if (!Number.isFinite(time)) {
      return;
    }
    void sendToDisplay(EVENTS.videoSeek, { time } satisfies VideoSeekPayload);
  });
  $("#bc-video-loop").addEventListener("change", (event) => {
    void sendToDisplay(EVENTS.videoSetLoop, {
      loop: (event.target as HTMLInputElement).checked,
    } satisfies VideoLoopPayload);
  });
  ($("#bc-media-volume") as HTMLInputElement).addEventListener("input", (event) => {
    const value = Number((event.target as HTMLInputElement).value) / 100;
    void sendToDisplay(EVENTS.mediaControl, {
      action: "volume",
      value,
    } satisfies MediaControlPayload);
  });
  ($("#bc-overlay") as HTMLInputElement).addEventListener("input", async (event) => {
    void sendToDisplay(EVENTS.setOverlay, {
      opacity: Number((event.target as HTMLInputElement).value) / 100,
    }).catch(() => undefined);
  });

  document.getElementById("bc-slide-host")?.addEventListener("scroll", () => updateSlideStripe());
  // Register preview frame to mirror everything sent to the display
  setPreviewFrame(frame());
  // Окно вывода (пере)создано: после Esc в окне Display экран пуст, поэтому
  // возвращаем на него живой слайд. Если слайда нет — ничего не делаем.
  onDisplayReady(() => {
    restoreLiveSlide();
  });
  // Clear preview on boot - it should start completely empty (no text, no media bg)
  previewFullClear(frame());
  setPreviewIdle("Нет сигнала");
  void listen<MediaStatusPayload>(EVENTS.mediaStatus, (event) => syncVideoPosition(event.payload));
  // Прогресс конвертации MP4 из бэкенда (ffmpeg -progress pipe:2 → ffmpeg-progress).
  void listen<{ path: string; percent: number; completed: boolean; error?: string }>("ffmpeg-progress", (event) => {
    const progress = event.payload;
    quickVideoProgress.set(progress.path, progress.percent);
    if (progress.error) {
      quickVideoConverting.delete(progress.path);
      quickVideoChecks.set(progress.path, false);
    } else if (progress.completed) {
      quickVideoConverting.delete(progress.path);
      quickVideoChecks.set(progress.path, true);
      quickVideoProgress.set(progress.path, 100);
      // Файл заменён оптимизированной версией — если он в эфире, перезапустить вывод.
      void resendLiveMediaIfPath(progress.path);
    }
    renderQuickList();
  });
  frame().addEventListener("load", () => {
    applyPreviewAspect(frame(), hooks.previewAspect().width, hooks.previewAspect().height);
    previewFullClear(frame());
    if (liveSlidePayload) {
      // iframe превью перезагрузился — возвращаем на него то, что стоит на экране,
      // иначе «Сейчас на экране» окажется пустым при живом слайде.
      restoreLiveSlide();
    } else {
      setPreviewIdle("Нет сигнала");
    }
  });

  // Esc управляется централизованно модулем горячих клавиш (show.end).

  syncLiveUi();
  renderQuickList();
  void renderSavedList();
}

export function refreshBroadcastPreviewAspect(width: number, height: number) {
  applyPreviewAspect(frame(), width, height);
}

export function isBroadcastLive(): boolean {
  return live;
}

export async function endBroadcastShow(): Promise<void> {
  if (live) {
    await endShow();
  }
}

/** Горячая клавиша «Начать показ»: старт, если показ ещё не идёт. */
export async function startBroadcastShow(): Promise<void> {
  if (live) {
    return;
  }
  logInfo("show", "старт показа");
  await startShow();
}

/** Горячая клавиша «Завершить показ»: если показ идёт — завершаем, иначе ничего. */
export async function hotkeyEndShow(): Promise<void> {
  if (live) {
    logInfo("show", "завершение показа");
    await endShow();
  }
}

/** Горячие клавиши «Следующий/Предыдущий слайд» (работают только во время показа). */
export async function stepSlides(delta: number): Promise<void> {
  logInfo("show", delta > 0 ? "следующий слайд" : "предыдущий слайд");
  await step(delta);
}

type TextShowItem = {
  title: string;
  slides: string[];
  mode: "announcement" | "bible";
  verseRef?: string;
  /** Ссылка на каждый слайд (для показа главы стих за стихом). */
  verseRefs?: string[];
};

/**
 * Единая логика запуска показа текста (объявление / стих Библии):
 * добавляет элемент в «Быстрый плейлист» Трансляции и стартует показ.
 * Возвращает true, если показ запущен.
 */
export async function openTextInBroadcast(
  item: TextShowItem,
  goLiveNow: boolean,
  startSlide = 0,
): Promise<boolean> {
  const slides = item.slides.filter((s) => s.trim().length > 0);
  if (slides.length === 0) {
    return false;
  }
  const key = `text-${Date.now()}-${Math.round(Math.random() * 1e6)}`;
  quick.unshift({
    key,
    kind: "text",
    title: item.title,
    slides,
    textMode: item.mode,
    verseRef: item.verseRef,
    verseRefs: item.verseRefs,
  });
  renderQuickList();
  await selectQuickItem(key);
  if (startSlide > 0 && startSlide < slides.length) {
    await pickSlide(startSlide, false);
  }
  if (goLiveNow) {
    await startShow();
  }
  return true;
}
