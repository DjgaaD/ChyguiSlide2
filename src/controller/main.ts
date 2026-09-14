import { confirmDialog, invoke, listen, open, readTextFile, save } from "../shared/ipc";
import { installExternalLinkHandler } from "../shared/external-links";
import {
  fetchJournalInfo,
  installGlobalLogging,
  logError,
  logInfo,
  logJournalLocation,
  openJournalFolder,
} from "../shared/logger";
import {
  ALargeSmall,
  ArrowUpDown,
  BookOpenText,
  createIcons,
  DatabaseBackup,
  Folder,
  Globe,
  Hash,
  Info,
  Keyboard,
  LayoutDashboard,
  List,
  Megaphone,
  Music,
  Paintbrush,
  Palette,
  PenLine,
  Pencil,
  Plus,
  Presentation,
  Radio,
  RotateCcw,
  Save,
  ScrollText,
  Settings,
  Trash2,
  Upload,
  Zap,
} from "lucide";
import {
  type MonitorInfo,
  type TextMode,
} from "../shared/events";
import { applyPreviewAspect, previewClear, previewSetSlide } from "./preview-frame";
import {
  addSongToQuickPlaylist,
  bindBroadcast,
  hotkeyEndShow,
  isSongInQuickPlaylist,
  openSongInBroadcast,
  openTextInBroadcast,
  refreshBroadcastPreviewAspect,
  startBroadcastShow,
  stepSlides,
  isBroadcastLive,
} from "./broadcast";
import {
  closeDisplayWindow,
  ensureDisplayReady,
  setDisplayMonitorIndex,
} from "./display-bridge";
import {
  bindCollectionEditor,
  bindSongEditor,
  openCollectionDeleteDialog,
  openCollectionEditor,
  openSongEditor,
  slidesFromPlainText,
  type Collection,
} from "./song-editor";
import {
  bindStylesUi,
  bootStyles,
  getActiveStyleConfig,
  refreshActiveStylePreviews,
} from "./styles";
import { bindHotkeysUi, bootHotkeys, registerHotkeys } from "./hotkeys";
import { bootColumnSplitters } from "./column-splitter";
import { bindObsUi, pushObsStyle, refreshObsStatus } from "./obs";

type SongHit = { id: number; number: number; title: string };
type SongDetail = {
  id: number;
  title: string;
  slides: string[];
  collection_id?: number | null;
};
/** Ответ `fetch_website_song`: название песни со страницы и её текст. */
type WebsiteSong = { title: string; text: string };
type Verse = { book: string; chapter: number; verse: number; text: string };
type Announcement = { id: string; title: string; text: string };
type PendingSlide = {
  title?: string;
  lines: string[];
  mode: TextMode;
  songId?: number;
  slideIndex?: number;
  /** Bible reference ("Ин 3:16") — отдельно от текста стиха. */
  verseRef?: string;
};

/**
 * Ответ команды `yandex_settings`: настройки и состояние авторизации.
 * Токен приходит целиком — поле «OAuth-токен» всегда показывает сохранённое
 * значение, чтобы его можно было править или удалить.
 */
type YandexSettings = {
  configured: boolean;
  token: string;
  clientId: string;
  folder: string;
  keepCopies: number;
  cloudDir: string;
};
/** Ответ команды `check_yandex_token`. */
type YandexAccount = { login: string; totalSpace: number; usedSpace: number };
/** Ответ команды `yandex_backup` — для уведомления пользователю. */
type YandexBackupResult = {
  fileName: string;
  cloudPath: string;
  cloudDir: string;
  /** Путь так, как его отдаёт Яндекс: `disk:/Приложения/…`. */
  displayPath: string;
  /** Ссылка на папку копий в веб-интерфейсе Диска (может быть пустой). */
  webUrl: string;
  sizeBytes: number;
  uploadedAt: string;
  removedCount: number;
  removed: string[];
};

const STORAGE = {
  theme: "chyguislide.theme",
  confirmClose: "chyguislide.confirmClose",
  shows: "chyguislide.shows",
  announcements: "chyguislide.announcements",
  monitor: "chyguislide.displayMonitor",
  persistentDisplay: "chyguislide.persistentDisplay",
};

const OT_COUNT = 39;

let selectedSong: SongDetail | null = null;
let selectedSlideIndex = -1;
let pending: PendingSlide | null = null;
let books: string[] = [];
let selectedBook = "";
let selectedChapter = 1;
let chapterVerses: Verse[] = [];
let selectedBibleVerseIndex = 0;
/** Стихи, выбранные Ctrl+кликом (индексы в текущем списке стихов). */
let selectedBibleVerseIndexes: number[] = [];
/** Что сейчас показано в блоке стихов: глава книги или результаты поиска. */
let bibleListMode: "chapter" | "search" = "chapter";
let bibleSearchResults: Verse[] = [];
let bibleSearchIndex = -1;
let bibleSearchRequestId = 0;
let announcements: Announcement[] = [];
let selectedAnnId = "";
let selectedAnnSlide = 0;
let monitors: MonitorInfo[] = [];
let selectedMonitorIndex: number | null = null;
let previewAspect = { width: 16, height: 9 };
let songSort: "title" | "id" = "title";
let songsRequestId = 0;
let collections: Collection[] = [];

function $(sel: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(sel);
  if (!el) {
    throw new Error(`Missing ${sel}`);
  }
  return el;
}

function input(sel: string): HTMLInputElement {
  return $(sel) as HTMLInputElement;
}

function select(sel: string): HTMLSelectElement {
  return $(sel) as HTMLSelectElement;
}

function songsPreviewFrame(): HTMLIFrameElement {
  return $("#songs-preview-frame") as HTMLIFrameElement;
}

function annPreviewFrame(): HTMLIFrameElement {
  return $("#ann-preview-frame") as HTMLIFrameElement;
}

function biblePreviewFrame(): HTMLIFrameElement {
  return $("#bible-preview-frame") as HTMLIFrameElement;
}

function loadJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function saveJson(key: string, value: unknown) {
  localStorage.setItem(key, JSON.stringify(value));
}

function showCounts(): Record<string, number> {
  return loadJson<Record<string, number>>(STORAGE.shows, {});
}

function bumpShow(songId?: number) {
  if (songId == null) {
    return;
  }
  const counts = showCounts();
  counts[String(songId)] = (counts[String(songId)] || 0) + 1;
  saveJson(STORAGE.shows, counts);
}

/**
 * Сброс статистики показов (кнопка в «Обзоре»).
 *
 * Обнуляет счётчики, по которым строится топ: после сброса список «Обзор»
 * пуст и наполняется заново по мере показов. Спрашиваем подтверждение — действие
 * необратимо, поэтому диалог асинхронный и ждём ответа пользователя
 * (`window.confirm` для этого не годится, см. `confirmDialog`).
 */
async function resetShowCounts() {
  if (Object.keys(showCounts()).length === 0) {
    window.alert("Статистика показов уже пуста — сбрасывать нечего.");
    return;
  }
  const ok = await confirmDialog(
    "Сбросить статистику показов? Счётчики всех песен обнулятся, а список «Обзор» станет пустым. Отменить это действие нельзя.",
    { title: "Сброс статистики", kind: "warning", okLabel: "Сбросить", cancelLabel: "Отмена" },
  );
  if (!ok) {
    return;
  }
  localStorage.removeItem(STORAGE.shows);
  logInfo("ui", "статистика показов сброшена");
  await loadOverview();
}

function bookGroup(index: number): string {
  if (index < 5) return "g0";
  if (index < 17) return "g1";
  if (index < 22) return "g2";
  if (index < 39) return "g3";
  if (index < 43) return "g4";
  if (index < 44) return "g5";
  if (index < 57) return "g6";
  return "g7";
}

function parseAnnSlides(text: string): string[] {
  return text
    .split(/\n\s*\n/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function slideLabel(text: string, index: number): string {
  const first = text.split("\n")[0]?.trim() || "";
  if (/^куплет/i.test(first) || /^припев/i.test(first)) {
    return first;
  }
  return `Слайд ${index + 1}`;
}

function setBiblePreview(payload: PendingSlide | null) {
  pending = payload;
  const frame = biblePreviewFrame();
  const idle = document.getElementById("bible-preview-idle");
  const showBtn = document.getElementById("bible-show") as HTMLButtonElement | null;
  if (!payload || payload.lines.length === 0) {
    previewClear(frame);
    idle?.removeAttribute("hidden");
    if (showBtn) {
      showBtn.disabled = true;
      showBtn.textContent = "Показать на экране";
    }
    return;
  }
  idle?.setAttribute("hidden", "");
  if (showBtn) {
    showBtn.disabled = false;
    // При мультивыделении сразу видно, сколько стихов уйдёт на экран.
    const count = selectedBibleVerseIndexes.length;
    showBtn.textContent =
      count > 1 ? `Показать на экране (${verseCountLabel(count)})` : "Показать на экране";
  }
  // В превью — то же, что уходит на экран: без заголовка, но с подписью стиха
  // (`verseRef`), которую рисует активный стиль (см. `previewSetSlide`).
  previewSetSlide(frame, payload);
  refreshActiveStylePreviews();
}

/** Список стихов, к которому относятся индексы выделения. */
function currentVerseList(): Verse[] {
  return bibleListMode === "search" ? bibleSearchResults : chapterVerses;
}

/** Выбранные стихи в порядке клика (пустые тексты отбрасываются). */
function selectedBibleVerses(): Verse[] {
  const list = currentVerseList();
  const indexes =
    selectedBibleVerseIndexes.length > 0 ? selectedBibleVerseIndexes : [selectedBibleVerseIndex];
  return indexes
    .map((index) => list[index])
    .filter((verse): verse is Verse => Boolean(verse))
    .filter((verse) => verse.text.trim().length > 0);
}

/**
 * Ссылка на стихи с группировкой подряд идущих номеров:
 * «От Иоанна 3:16-18», «От Иоанна 3:16,18», «Ин 3:16-18; 4:1».
 */
function formatVerseRef(verses: Verse[]): string {
  const groups = new Map<string, { book: string; chapter: number; numbers: number[] }>();
  for (const verse of verses) {
    const key = `${verse.book}\u0000${verse.chapter}`;
    const group = groups.get(key);
    if (group) {
      group.numbers.push(verse.verse);
    } else {
      groups.set(key, { book: verse.book, chapter: verse.chapter, numbers: [verse.verse] });
    }
  }

  const parts: string[] = [];
  let firstBook = "";
  groups.forEach((group) => {
    const numbers = [...new Set(group.numbers)].sort((a, b) => a - b);
    const ranges: string[] = [];
    let start = numbers[0];
    let previous = numbers[0];
    for (let i = 1; i <= numbers.length; i += 1) {
      const current = numbers[i];
      if (current !== previous + 1) {
        ranges.push(start === previous ? String(start) : `${start}-${previous}`);
        start = current;
      }
      previous = current;
    }
    // Название книги повторяется только при переходе к другой книге.
    const bookPart = group.book === firstBook ? "" : `${group.book} `;
    if (!firstBook) {
      firstBook = group.book;
    }
    parts.push(`${bookPart}${group.chapter}:${ranges.join(",")}`);
  });
  return parts.join("; ");
}

/** «1 стих», «2 стиха», «5 стихов» — для подписи кнопки показа. */
function verseCountLabel(count: number): string {
  const mod10 = count % 10;
  const mod100 = count % 100;
  if (mod10 === 1 && mod100 !== 11) {
    return `${count} стих`;
  }
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) {
    return `${count} стиха`;
  }
  return `${count} стихов`;
}

/** Подсветка выделения: «якорный» стих — `.selected`, добавленные — `.multi-selected`. */
function syncBibleVerseSelection() {
  const anchor = selectedBibleVerseIndexes[0];
  const multiple = selectedBibleVerseIndexes.length > 1;
  document.querySelectorAll<HTMLElement>("#bible-verses li").forEach((node) => {
    const index = Number(node.dataset.key);
    const selected = selectedBibleVerseIndexes.includes(index);
    node.classList.toggle("selected", selected);
    node.classList.toggle("multi-selected", selected && multiple && index !== anchor);
  });
}

/**
 * Выбор стиха в списке. Обычный клик выделяет один стих (как раньше),
 * клик с Ctrl добавляет стих к выделению, повторный Ctrl+клик убирает его.
 */
function pickBibleVerse(index: number, event?: MouseEvent) {
  const list = currentVerseList();
  const verse = list[index];
  if (!verse) {
    return;
  }
  if (bibleListMode === "search") {
    // Результат поиска становится активной главой Библии — по нему работает F5.
    selectedBook = verse.book;
    selectedChapter = verse.chapter;
    chapterVerses = [verse];
    bibleSearchIndex = index;
  }

  if (event?.ctrlKey || event?.metaKey) {
    const at = selectedBibleVerseIndexes.indexOf(index);
    if (at >= 0) {
      selectedBibleVerseIndexes.splice(at, 1);
    } else {
      selectedBibleVerseIndexes.push(index);
    }
    // Пустое выделение не оставляем: стих, с которого сняли отметку, остаётся активным.
    if (selectedBibleVerseIndexes.length === 0) {
      selectedBibleVerseIndexes.push(index);
    }
  } else {
    selectedBibleVerseIndexes = [index];
  }
  selectedBibleVerseIndex = selectedBibleVerseIndexes[selectedBibleVerseIndexes.length - 1];

  const verses = selectedBibleVerses();
  setBiblePreview(
    verses.length > 0
      ? { lines: verses.map((item) => item.text), mode: "bible", verseRef: formatVerseRef(verses) }
      : null,
  );
  syncBibleVerseSelection();
  requestAnimationFrame(updateBibleVerseStripe);
}

function updateBibleVerseStripe() {
  const host = document.getElementById("bible-verse-host");
  const stripe = document.getElementById("bible-verse-stripe");
  const selected = document.querySelector<HTMLElement>(
    "#bible-verses .slide-item.selected",
  );
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

async function ensureDisplayWindow() {
  setDisplayMonitorIndex(selectedMonitorIndex);
  await ensureDisplayReady();
}

function isSelectedMonitorPrimary(): boolean {
  if (selectedMonitorIndex == null) {
    return true;
  }
  const monitor = monitors.find((m) => m.index === selectedMonitorIndex);
  return monitor?.isPrimary ?? true;
}

function persistentDisplayEnabled(): boolean {
  return localStorage.getItem(STORAGE.persistentDisplay) === "1";
}

function setPersistentDisplayEnabled(on: boolean) {
  localStorage.setItem(STORAGE.persistentDisplay, on ? "1" : "0");
}

function syncPersistentDisplayUi() {
  const checkbox = document.getElementById("persistent-display") as HTMLInputElement | null;
  const hint = document.getElementById("persistent-display-hint");
  if (!checkbox) {
    return;
  }
  const primary = isSelectedMonitorPrimary();
  checkbox.disabled = primary;
  if (primary) {
    checkbox.checked = false;
    setPersistentDisplayEnabled(false);
    if (hint) {
      hint.textContent =
        "Опция недоступна: выбран основной монитор. Укажите внешний экран, чтобы не перекрыть панель управления.";
    }
  } else {
    checkbox.checked = persistentDisplayEnabled();
    if (hint) {
      hint.textContent =
        "Окно Display остаётся на втором экране постоянно. Откроется также при любом показе слайда.";
    }
  }
}

function applyTheme(theme: "system" | "dark" | "light") {
  const resolved = theme === "system"
    ? (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")
    : theme;
  document.documentElement.dataset.theme = resolved;
  localStorage.setItem(STORAGE.theme, theme);
  const label = resolved === "dark" ? "Светлая тема" : "Тёмная тема";
  const toggle = document.getElementById("theme-toggle");
  if (toggle) {
    toggle.textContent = label;
  }
}

function placeNavStripe() {
  const active = document.querySelector<HTMLElement>(".nav-item.active");
  const stripe = document.getElementById("nav-stripe");
  const nav = document.getElementById("nav");
  if (!active || !stripe || !nav) {
    return;
  }
  const a = active.getBoundingClientRect();
  const n = nav.getBoundingClientRect();
  const w = 40;
  stripe.style.width = `${w}px`;
  stripe.style.transform = `translateX(${a.left - n.left + (a.width - w) / 2}px)`;
}

function switchTab(name: string) {
  logInfo("nav", `вкладка: ${name}`);
  document.querySelectorAll(".nav-item").forEach((tab) => {
    tab.classList.toggle("active", (tab as HTMLElement).dataset.tab === name);
  });
  document.querySelectorAll(".view").forEach((view) => {
    view.classList.toggle("active", (view as HTMLElement).dataset.view === name);
  });
  requestAnimationFrame(placeNavStripe);
}

function setSongsPreview(payload: PendingSlide | null) {
  pending = payload;
  const frame = songsPreviewFrame();
  const idle = document.getElementById("songs-preview-idle");
  const showBtn = document.getElementById("songs-start-show") as HTMLButtonElement | null;
  if (!payload) {
    previewClear(frame);
    idle?.removeAttribute("hidden");
    if (showBtn) {
      showBtn.disabled = true;
    }
    return;
  }
  idle?.setAttribute("hidden", "");
  if (showBtn) {
    showBtn.disabled = false;
  }
  // В превью — тот же слайд, что уйдёт на экран (фильтрация песни и снятие
  // заголовка выполняются в `previewSetSlide` ровно как в окне вывода).
  previewSetSlide(frame, payload);
  // Гарантируем, что активный стиль (фон, шрифт) не потеряется при выборе.
  refreshActiveStylePreviews();
}

function setAnnPreview(payload: PendingSlide | null) {
  pending = payload;
  const frame = annPreviewFrame();
  const idle = document.getElementById("ann-preview-idle");
  const showBtn = document.getElementById("ann-show") as HTMLButtonElement | null;
  if (!payload || payload.lines.length === 0) {
    previewClear(frame);
    idle?.removeAttribute("hidden");
    if (showBtn) {
      showBtn.disabled = true;
    }
    return;
  }
  idle?.setAttribute("hidden", "");
  if (showBtn) {
    showBtn.disabled = false;
  }
  // В превью только текст объявления — заголовок остаётся в интерфейсе.
  previewSetSlide(frame, payload);
  refreshActiveStylePreviews();
}

function updateSlideStripe() {
  const host = document.getElementById("slide-host");
  const stripe = document.getElementById("slide-stripe");
  const selected = document.querySelector<HTMLElement>("#slide-list .slide-item.selected");
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

function fillList(
  root: HTMLElement,
  items: { key: string; html: string; title?: string; className?: string }[],
  onPick: (key: string, event: MouseEvent) => void,
  activeKey?: string,
  activeClass = "active",
) {
  root.replaceChildren();
  for (const item of items) {
    const li = document.createElement("li");
    li.dataset.key = item.key;
    if (item.className) {
      li.className = item.className;
    }
    li.innerHTML = item.html;
    if (item.title) {
      li.title = item.title;
    }
    if (item.key === activeKey) {
      li.classList.add(activeClass);
    }
    li.addEventListener("click", (event) => onPick(item.key, event));
    root.appendChild(li);
  }
}

function closeMenus() {
  document.querySelectorAll(".menu-panel").forEach((panel) => {
    (panel as HTMLElement).hidden = true;
  });
}

function refreshIcons() {
  createIcons({
    icons: {
      BookOpenText,
      LayoutDashboard,
      Music,
      Radio,
      Megaphone,
      Settings,
      Paintbrush,
      Palette,
      Keyboard,
      DatabaseBackup,
      ScrollText,
      Info,
      ArrowUpDown,
      ALargeSmall,
      Hash,
      Plus,
      Presentation,
      Folder,
      Globe,
      List,
      PenLine,
      Pencil,
      Trash2,
      Save,
      RotateCcw,
      Upload,
      Zap,
    },
  });
}

async function resolvePreviewAspect() {
  monitors = await invoke<MonitorInfo[]>("list_monitors");
  const stored = localStorage.getItem(STORAGE.monitor);
  const storedIndex = stored != null ? Number(stored) : NaN;

  let target =
    monitors.find((m) => m.index === storedIndex) ||
    monitors.find((m) => !m.isPrimary) ||
    monitors[0];

  if (target) {
    selectedMonitorIndex = target.index;
    previewAspect = { width: target.width, height: target.height };
  } else {
    selectedMonitorIndex = null;
    previewAspect = { width: 16, height: 9 };
  }

  applyPreviewAspect(songsPreviewFrame(), previewAspect.width, previewAspect.height);
  applyPreviewAspect(annPreviewFrame(), previewAspect.width, previewAspect.height);
  applyPreviewAspect(biblePreviewFrame(), previewAspect.width, previewAspect.height);
  refreshBroadcastPreviewAspect(previewAspect.width, previewAspect.height);
}

async function loadMonitorsSettings() {
  await resolvePreviewAspect();
  const list = $("#monitor-list");
  list.replaceChildren();

  if (monitors.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = "Мониторы не найдены. Превью: 16:9.";
    list.appendChild(empty);
    syncPersistentDisplayUi();
    return;
  }

  for (const monitor of monitors) {
    const card = document.createElement("button");
    card.type = "button";
    card.className = `monitor-card${monitor.index === selectedMonitorIndex ? " active" : ""}`;
    card.innerHTML = `
      <div>
        <strong>${monitor.name}${monitor.isPrimary ? " (основной)" : ""}</strong>
        <div class="meta">${monitor.width}×${monitor.height} · scale ${monitor.scaleFactor.toFixed(2)} · pos ${monitor.x},${monitor.y}</div>
      </div>
      <span>${monitor.index === selectedMonitorIndex ? "выбран" : "выбрать"}</span>
    `;
    card.addEventListener("click", async () => {
      selectedMonitorIndex = monitor.index;
      setDisplayMonitorIndex(monitor.index);
      localStorage.setItem(STORAGE.monitor, String(monitor.index));
      previewAspect = { width: monitor.width, height: monitor.height };
      applyPreviewAspect(songsPreviewFrame(), monitor.width, monitor.height);
      applyPreviewAspect(annPreviewFrame(), monitor.width, monitor.height);
      applyPreviewAspect(biblePreviewFrame(), monitor.width, monitor.height);
      await invoke("set_display_monitor", { index: monitor.index }).catch(() => undefined);
      if (monitor.isPrimary) {
        setPersistentDisplayEnabled(false);
        await closeDisplayWindow();
        refreshActiveStylePreviews();
      } else if (persistentDisplayEnabled()) {
        await ensureDisplayWindow();
        refreshActiveStylePreviews();
      }
      await loadMonitorsSettings();
    });
    list.appendChild(card);
  }
  syncPersistentDisplayUi();
}

async function loadCollections(selectId?: number | null) {
  try {
    const rows = await invoke<Array<{ id: number; title?: string; name?: string }>>(
      "list_collections",
    );
    collections = (Array.isArray(rows) ? rows : []).map((row) => ({
      id: Number(row.id),
      title: String(row.title ?? row.name ?? "").trim() || `Сборник ${row.id}`,
    }));
  } catch (error) {
    console.error("loadCollections failed", error);
    window.alert(`Не удалось загрузить сборники: ${error}`);
    collections = [];
  }

  const songSel = select("#song-collection");
  const overviewSel = select("#overview-collection");
  const keepSong = selectId != null ? String(selectId) : songSel.value || "all";
  const keepOverview = overviewSel.value || "all";

  const fill = (sel: HTMLSelectElement, keep: string) => {
    const options = [
      `<option value="all">Все песни</option>`,
      ...collections.map(
        (c) =>
          `<option value="${c.id}">${escapeHtml(c.title)}</option>`,
      ),
    ];
    sel.innerHTML = options.join("");
    if ([...sel.options].some((o) => o.value === keep)) {
      sel.value = keep;
    } else if (collections.length === 1) {
      sel.value = String(collections[0].id);
    } else {
      sel.value = "all";
    }
  };

  fill(songSel, keepSong);
  fill(overviewSel, keepOverview);
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function selectedCollectionFilter(): number | null {
  const value = select("#song-collection").value;
  if (!value || value === "all") {
    return null;
  }
  const id = Number(value);
  return Number.isFinite(id) ? id : null;
}

async function loadSongs() {
  const list = $("#song-list");
  const requestId = ++songsRequestId;
  list.innerHTML = `<li class="list-status">Загрузка…</li>`;

  try {
    const hits = await invoke<SongHit[]>("search_songs", {
      query: input("#song-query").value,
      sort: songSort,
      collectionId: selectedCollectionFilter(),
    });
    if (requestId !== songsRequestId) {
      return;
    }
    if (hits.length === 0) {
      list.innerHTML = `<li class="list-status">Ничего не найдено</li>`;
      return;
    }
    fillList(
      list,
      hits.map((hit) => ({
        key: String(hit.id),
        className: "song-item",
        html: `<span class="song-num">${hit.number}</span><span class="song-title-text">${hit.title}</span>`,
        title: hit.title,
      })),
      async (key) => {
        await openSong(Number(key));
      },
      selectedSong ? String(selectedSong.id) : undefined,
      "selected",
    );
  } catch (error) {
    if (requestId !== songsRequestId) {
      return;
    }
    console.error("loadSongs failed", error);
    list.innerHTML = `<li class="list-status">Ошибка загрузки списка</li>`;
  }
}

async function backupDatabase() {
  const destination = await save({
    defaultPath: "chyguislide-backup.sqlite",
    filters: [{ name: "SQLite database", extensions: ["sqlite", "db"] }],
  });
  if (!destination) {
    return;
  }
  try {
    await invoke("backup_database", { destinationPath: destination });
    window.alert("Backup created successfully.");
  } catch (error) {
    console.error("Database backup failed", error);
    window.alert(`Backup failed: ${String(error)}`);
  }
}

async function restoreDatabase() {
  const source = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "SQLite database", extensions: ["sqlite", "db", "bak"] }],
  });
  if (!source || Array.isArray(source)) {
    return;
  }
  if (!(await confirmDialog("Restore this database backup? Current data will be replaced."))) {
    return;
  }
  try {
    await invoke("restore_database", { sourcePath: source });
    await loadCollections();
    await loadSongs();
    await loadBooks();
    window.alert("Database restored successfully.");
  } catch (error) {
    console.error("Database restore failed", error);
    window.alert(`Restore failed: ${String(error)}`);
  }
}

// ——— Резервная копия на Яндекс.Диск ———

/**
 * Статус последней операции с Яндекс.Диском. Подробности лежат внутри спойлера,
 * поэтому при ошибке спойлер раскрывается — иначе сообщение осталось бы скрытым.
 */
function yandexStatus(text: string, error = false) {
  const el = document.getElementById("yandex-status");
  if (!el) {
    return;
  }
  el.textContent = text;
  el.classList.toggle("error", error);
  if (error) {
    const spoiler = document.getElementById("yandex-settings") as HTMLDetailsElement | null;
    if (spoiler) {
      spoiler.open = true;
    }
  }
}

/** «12.4 МБ» — человекочитаемый размер. */
function formatBytes(bytes: number): string {
  if (bytes >= 1024 ** 3) {
    return `${(bytes / 1024 ** 3).toFixed(2)} ГБ`;
  }
  if (bytes >= 1024 ** 2) {
    return `${(bytes / 1024 ** 2).toFixed(1)} МБ`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(0)} КБ`;
  }
  return `${bytes} Б`;
}

/**
 * Расположение копий для человека: `app:/Папка/backups` и `disk:/Приложения/…`
 * превращаются в цепочку «Яндекс.Диск → Приложения → …». Копии лежат в папке
 * приложения, поэтому в списке «Все файлы» их не видно.
 */
function yandexPathText(path: string): string {
  const segments = path.replace(/^[a-z]+:\//i, "").split("/").filter(Boolean);
  if (!segments.length) {
    return path;
  }
  const tail = segments.join(" → ");
  return /^app:/i.test(path) ? `папка приложения → ${tail}` : `Яндекс.Диск → ${tail}`;
}

/** Токен, показанный в поле: по нему видно, менял ли пользователь поле вручную. */
let yandexSavedToken = "";

/** Заполняет поле, не мешая набору: значение не перебиваем, если поле в фокусе. */
function setYandexField(selector: string, value: string) {
  const el = input(selector);
  if (document.activeElement !== el) {
    el.value = value;
  }
}

/**
 * Показывает настройки Яндекс.Диска. Токен подставляется прямо в поле — так его
 * видно, можно поправить или стереть (пустое поле = токена нет).
 */
function applyYandexSettings(settings: YandexSettings) {
  setYandexField("#yandex-client-id", settings.clientId);
  setYandexField("#yandex-token", settings.token);
  setYandexField("#yandex-folder", settings.folder);
  setYandexField("#yandex-keep", String(settings.keepCopies));
  yandexSavedToken = settings.token;
  const state = document.getElementById("yandex-state");
  if (state) {
    state.textContent = settings.configured ? "Токен сохранён" : "Токен не сохранён";
  }
  yandexStatus(
    settings.configured
      ? `Токен сохранён. Копии: ${yandexPathText(settings.cloudDir)}, храним ${settings.keepCopies}.`
      : `Токен не сохранён — нажмите «Получить токен». Копии: ${yandexPathText(settings.cloudDir)}.`,
  );
}

/** Показывает настройки Яндекс.Диска (обновляются при открытии вкладки). */
async function refreshYandexStatus() {
  try {
    applyYandexSettings(await invoke<YandexSettings>("yandex_settings"));
  } catch (error) {
    yandexStatus(`Не удалось прочитать настройку: ${String(error)}`, true);
  }
}

/** Значения формы настроек Яндекс.Диска. */
function yandexSettingsForm() {
  return {
    clientId: input("#yandex-client-id").value.trim(),
    folder: input("#yandex-folder").value.trim(),
    // Пустое или некорректное поле означает «не ограничивать число копий».
    keepCopies: Math.trunc(Number(input("#yandex-keep").value || "0")) || 0,
    // Пустое поле токена означает «удалить сохранённый токен».
    token: input("#yandex-token").value.trim(),
  };
}

/** Сохраняет настройки Яндекс.Диска вместе с токеном; `quiet` — без уведомления. */
async function saveYandexSettings(quiet = false): Promise<YandexSettings | null> {
  const form = yandexSettingsForm();
  try {
    const settings = await invoke<YandexSettings>("save_yandex_settings", form);
    applyYandexSettings(settings);
    logInfo(
      "yandex",
      `настройки Яндекс.Диска сохранены: ${settings.cloudDir}, токен ${settings.configured ? "есть" : "нет"}`,
    );
    if (!quiet) {
      window.alert(
        settings.configured ? "Настройки сохранены." : "Настройки сохранены, токен удалён.",
      );
    }
    return settings;
  } catch (error) {
    console.error("Yandex.Disk settings save failed", error);
    yandexStatus(`Не удалось сохранить настройки: ${String(error)}`, true);
    return null;
  }
}

/** Проверяет токен (из поля или сохранённый) и показывает объём диска. */
async function checkYandexToken() {
  const token = input("#yandex-token").value.trim();
  yandexStatus("Проверяем токен…");
  try {
    const account = await invoke<YandexAccount>("check_yandex_token", { token });
    yandexStatus(
      `Токен рабочий: ${account.login} — занято ${formatBytes(account.usedSpace)} из ${formatBytes(account.totalSpace)}.`,
    );
  } catch (error) {
    console.error("Yandex.Disk token check failed", error);
    yandexStatus(`Токен не принят: ${String(error)}`, true);
  }
}

/**
 * Открывает страницу, на которой Яндекс показывает OAuth-токен.
 *
 * Client ID — единственное поле, без которого страница не откроется, поэтому его
 * отсутствие проверяется до сохранения настроек.
 */
async function openYandexTokenPage() {
  if (!input("#yandex-client-id").value.trim()) {
    yandexStatus("Укажите Client ID приложения Яндекс.Диска.", true);
    return;
  }
  // Client ID из поля сохраняем сразу: иначе страница откроется без приложения.
  if (!(await saveYandexSettings(true))) {
    return;
  }
  // Поле для вставки токена сразу в фокусе: со страницы Яндекса токен скопируют сюда.
  input("#yandex-token").focus();
  try {
    await invoke("open_yandex_token_page");
    yandexStatus(
      "Страница Яндекса открыта: разрешите доступ, скопируйте токен со страницы в поле «OAuth-токен» и нажмите «Сохранить настройки».",
    );
  } catch (error) {
    console.error("Yandex.Disk token page failed", error);
    yandexStatus(`Не удалось открыть браузер: ${String(error)}`, true);
  }
}

/** Резервная копия на Яндекс.Диск: архив + загрузка, со статусом в настройках. */
async function backupToYandex() {
  const button = document.getElementById("yandex-backup") as HTMLButtonElement | null;
  // Правки формы (в том числе новый токен) сохраняем сразу — иначе копия ушла бы
  // по старым настройкам. Если поле токена не трогали, лишней записи не будет.
  const typedToken = input("#yandex-token").value.trim();
  if (typedToken !== yandexSavedToken && !(await saveYandexSettings(true))) {
    return;
  }
  if (button) {
    button.disabled = true;
  }
  yandexStatus("Собираем архив и отправляем на Яндекс.Диск…");
  try {
    const result = await invoke<YandexBackupResult>("yandex_backup");
    const rotated =
      result.removedCount > 0 ? ` Убрано старых копий: ${result.removedCount}.` : "";
    const where = yandexPathText(result.displayPath);
    yandexStatus(
      `Копия загружена: ${where} (${formatBytes(result.sizeBytes)}, ${result.uploadedAt}).${rotated}`,
    );
    window.alert(
      `Резервная копия загружена на Яндекс.Диск:\n${where}` +
        (result.removedCount > 0
          ? `\nСтарые копии убраны в корзину: ${result.removed.join(", ")}`
          : "") +
        "\n\nКопии лежат в папке приложения: в списке «Все файлы» её не видно. " +
        "Кнопка «Открыть папку с копиями на Диске» покажет её в браузере.",
    );
  } catch (error) {
    console.error("Yandex.Disk backup failed", error);
    yandexStatus(`Не удалось создать копию: ${String(error)}`, true);
    window.alert(`Не удалось создать копию на Яндекс.Диске:\n${String(error)}`);
  } finally {
    if (button) {
      button.disabled = false;
    }
  }
}

/** Открывает в браузере папку с копиями на Яндекс.Диске. */
async function openYandexFolder() {
  yandexStatus("Открываем папку с копиями на Диске…");
  try {
    await invoke("open_yandex_backups_folder");
    yandexStatus("Папка с копиями открыта в браузере.");
  } catch (error) {
    console.error("Yandex.Disk folder open failed", error);
    yandexStatus(`Не удалось открыть папку: ${String(error)}`, true);
  }
}

/** Обновляет подсказку с путём к каталогу журналов во вкладке «Журнал». */
async function refreshLogsHint() {
  const hint = document.getElementById("logs-dir-hint");
  if (!hint) {
    return;
  }
  const info = await fetchJournalInfo();
  hint.textContent = info
    ? `Каталог журналов: ${info.dir} (файлов: ${info.files.length} из ${info.maxFiles})`
    : "Каталог журналов: недоступен";
}

async function importLegacyChorusJson() {
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Chorus JSON", extensions: ["json"] }],
  });
  if (!selected || Array.isArray(selected)) {
    return;
  }
  try {
    const rawJson = await readTextFile(selected);
    const result = await invoke<{ importedSongs: number; importedSections: number }>(
      "import_legacy_chorus_json",
      { rawJson },
    );
    await loadCollections();
    await loadSongs();
    window.alert(`Импортировано песен: ${result.importedSongs}. Разделов: ${result.importedSections}.`);
  } catch (error) {
    console.error("Legacy chorus JSON import failed", error);
    window.alert(`Ошибка импорта: ${String(error)}`);
  }
}

/* ——— Импорт песен: из презентации и с сайта ——— */

/**
 * Импорт песни из презентации.
 *
 * Пользователь выбирает файл `.pptx`/`.odp`, Rust (`importer.rs`) вытаскивает
 * текст слайдов, а редактор новой песни открывается уже заполненным: остаётся
 * проверить текст и сохранить.
 */
async function importSongFromPresentation() {
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Презентация", extensions: ["pptx", "odp"] }],
  });
  if (!selected || Array.isArray(selected)) {
    return;
  }
  try {
    const text = await invoke<string>("parse_presentation", { path: selected });
    openImportedSong(text, fileStem(selected));
  } catch (error) {
    logError("import", "не удалось разобрать презентацию", { error: String(error) });
    window.alert(`Не удалось разобрать презентацию: ${String(error)}`);
  }
}

/**
 * Импорт песни с сайта: адрес спрашивается своей формой, потому что `prompt` в
 * WebView2 не поддерживается (вызов молча возвращает `null`).
 */
async function importSongFromWebsite() {
  const url = await askWebsiteUrl();
  if (!url) {
    return;
  }
  try {
    const song = await invoke<WebsiteSong>("fetch_website_song", { url });
    openImportedSong(song.text, song.title);
  } catch (error) {
    logError("import", "не удалось загрузить текст песни", { url, error: String(error) });
    window.alert(`Не удалось загрузить текст песни: ${String(error)}`);
  }
}

/**
 * Открывает редактор новой песни с импортированным текстом.
 *
 * Название подставляет источник: у презентации — имя файла, у сайта — первая
 * строка названия песни со страницы.
 */
function openImportedSong(text: string, title: string) {
  const slides = slidesFromPlainText(text);
  logInfo("import", `открыт редактор импортированной песни: слайдов ${slides.length}`);
  openSongEditor({
    mode: "create",
    collections,
    initialSlides: slides,
    initialTitle: title,
    onSaved: onSongSaved,
  });
}

/** Имя файла без пути и расширения — им предзаполняется название песни. */
function fileStem(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? "";
  return name.replace(/\.[^.]+$/, "").trim();
}

/** Ответ на вопрос об адресе страницы: `resolve` ожидающего кода. */
let urlRequest: ((value: string | null) => void) | null = null;

function urlDialog(): HTMLDialogElement {
  return $("#import-url") as HTMLDialogElement;
}

/**
 * Закрывает окно ввода адреса и отдаёт ответ: строку — при подтверждении,
 * `null` — при отмене.
 */
function closeUrlDialog(value: string | null) {
  if (urlDialog().open) {
    urlDialog().close();
  }
  const resolve = urlRequest;
  urlRequest = null;
  resolve?.(value);
}

/**
 * Спрашивает адрес страницы своей формой.
 *
 * `window.prompt` в WebView2 не реализован (вызов молча возвращает `null`),
 * поэтому вопрос задаётся модальным окном `#import-url`.
 */
function askWebsiteUrl(): Promise<string | null> {
  const dlg = urlDialog();
  input("#import-url-input").value = "";
  // Если предыдущий вопрос остался без ответа, закрываем его без результата.
  urlRequest?.(null);
  return new Promise((resolve) => {
    urlRequest = resolve;
    dlg.showModal();
    input("#import-url-input").focus();
  });
}

/** Привязка окна ввода адреса: подтверждение, отмена и закрытие по Esc. */
function bindImportUrlDialog() {
  $("#import-url-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const url = input("#import-url-input").value.trim();
    if (!url) {
      window.alert("Введите адрес страницы с текстом песни.");
      return;
    }
    closeUrlDialog(url);
  });
  $("#import-url-cancel").addEventListener("click", () => closeUrlDialog(null));
  urlDialog().addEventListener("cancel", (event) => {
    event.preventDefault();
    closeUrlDialog(null);
  });
}

async function openSong(id: number) {
  selectedSong = await invoke<SongDetail | null>("get_song", { id });
  selectedSlideIndex = -1;
  pending = null;
  setSongsPreview(null);

  const empty = $("#songs-empty");
  const detail = $("#songs-detail");

  document.querySelectorAll("#song-list .song-item").forEach((node) => {
    node.classList.toggle(
      "selected",
      (node as HTMLElement).dataset.key === String(id),
    );
  });

  if (!selectedSong) {
    empty.hidden = false;
    detail.hidden = true;
    $("#song-title").textContent = "";
    $("#slide-list").replaceChildren();
    updateSlideStripe();
    return;
  }

  empty.hidden = true;
  detail.hidden = false;
  $("#song-title").textContent = selectedSong.title;
  // Как в макете: сразу выбираем первый слайд.
  selectedSlideIndex = 0;
  renderSongSlides();
  pickSongSlide(0);
  updateSongToPlaylistButton();
}

function pickSongSlide(index: number) {
  if (!selectedSong || index < 0 || index >= selectedSong.slides.length) {
    return;
  }
  selectedSlideIndex = index;
  const slide = selectedSong.slides[index];
  const lines = slide.split("\n").filter((line) => line.length > 0);
  setSongsPreview({
    title: selectedSong.title,
    lines,
    mode: "song",
    songId: selectedSong.id,
    slideIndex: index,
  });
  document.querySelectorAll("#slide-list .slide-item").forEach((node) => {
    node.classList.toggle(
      "selected",
      (node as HTMLElement).dataset.key === String(index),
    );
  });
  requestAnimationFrame(updateSlideStripe);
}

function renderSongSlides() {
  if (!selectedSong) {
    return;
  }
  fillList(
    $("#slide-list"),
    selectedSong.slides.map((slide, index) => {
      const label = slideLabel(slide, index);
      const body = slide.startsWith(label) ? slide.slice(label.length).trim() : slide;
      return {
        key: String(index),
        className: "slide-item",
        html: `<div class="slide-label">${label}</div><div class="slide-text">${body || slide}</div>`,
      };
    }),
    (key) => {
      pickSongSlide(Number(key));
    },
    selectedSlideIndex >= 0 ? String(selectedSlideIndex) : undefined,
    "selected",
  );
  requestAnimationFrame(updateSlideStripe);
}

/** Состояние кнопки «В плейлист»: если песня уже в быстром плейлисте — «В плейлисте» и отключена. */
function updateSongToPlaylistButton() {
  const btn = document.getElementById("song-to-playlist") as HTMLButtonElement | null;
  if (!btn) {
    return;
  }
  const inPlaylist = selectedSong ? isSongInQuickPlaylist(selectedSong.id) : false;
  btn.classList.toggle("in-playlist", inPlaylist);
  btn.title = inPlaylist ? "Уже в быстром плейлисте" : "Добавить в быстрый плейлист";
  btn.innerHTML = `<i data-lucide="list"></i>${inPlaylist ? "В плейлисте" : "В плейлист"}`;
  btn.disabled = inPlaylist;
  refreshIcons();
}

async function startSongShow() {
  if (!pending || pending.mode !== "song" || !selectedSong) {
    return;
  }
  await openSongInBroadcast(selectedSong.id, true, pending.slideIndex ?? 0);
  switchTab("broadcast");
}

/** Горячая клавиша «Начать показ»: запускает показ активного раздела. */
async function startShowForActiveView(): Promise<void> {
  const view = document.querySelector<HTMLElement>(".view.active")?.dataset.view ?? "";
  if (view === "songs") {
    await startSongShow();
    return;
  }
  if (view === "bible") {
    await startBibleVerseShow();
    return;
  }
  if (view === "announcements") {
    await startAnnouncementShow();
    return;
  }
  // Трансляция (и любой другой раздел) — старт показа плейлиста.
  await startBroadcastShow();
}

/**
 * Единая логика показа объявления: добавление в «Быстрый плейлист»
 * Трансляции, старт вывода и авто-переключение интерфейса на «Трансляцию».
 */
async function startAnnouncementShow(): Promise<boolean> {
  const item = selectedAnnouncement();
  if (!item) {
    return false;
  }
  const slides = parseAnnSlides(item.text);
  if (slides.length === 0) {
    window.alert("Объявление пусто — добавьте текст слайдов.");
    return false;
  }
  const ok = await openTextInBroadcast(
    {
      title: item.title,
      slides,
      mode: "announcement",
    },
    true,
    Math.min(selectedAnnSlide, Math.max(0, slides.length - 1)),
  );
  if (ok) {
    switchTab("broadcast");
  }
  return ok;
}

/**
 * Показ стихов Библии.
 * Один стих — прежнее поведение: в быстрый плейлист уходит вся глава, а стартовый
 * слайд — выбранный стих, чтобы «Следующий» показывал следующие стихи.
 * Несколько стихов (Ctrl+клик) — один слайд: тексты объединяются в общий блок,
 * а ссылка группируется («От Иоанна 3:16-18»).
 */
async function startBibleVerseShow(): Promise<boolean> {
  if (!pending || pending.mode !== "bible") {
    return false;
  }
  const picked = selectedBibleVerses();
  if (picked.length === 0) {
    return false;
  }

  if (picked.length > 1) {
    const reference = formatVerseRef(picked);
    const ok = await openTextInBroadcast(
      {
        title: reference,
        slides: [picked.map((verse) => verse.text).join("\n")],
        mode: "bible",
        verseRef: reference,
        verseRefs: [reference],
      },
      true,
      0,
    );
    if (ok) {
      switchTab("broadcast");
    }
    return ok;
  }

  const verses = (bibleListMode === "chapter" ? chapterVerses : picked).filter(
    (verse) => verse.text.trim().length > 0,
  );
  if (verses.length === 0) {
    return false;
  }
  const slides = verses.map((verse) => verse.text);
  const verseRefs = verses.map((verse) => `${verse.book} ${verse.chapter}:${verse.verse}`);
  const startIdx = Math.min(Math.max(0, selectedBibleVerseIndex), slides.length - 1);
  const ok = await openTextInBroadcast(
    {
      title: `${selectedBook} · Глава ${selectedChapter}`,
      slides,
      mode: "bible",
      verseRef: verseRefs[0],
      verseRefs,
    },
    true,
    startIdx,
  );
  if (ok) {
    switchTab("broadcast");
  }
  return ok;
}

async function loadOverview() {
  const top = Number(select("#overview-top").value) || 20;
  const overviewValue = select("#overview-collection").value;
  const collectionId =
    overviewValue && overviewValue !== "all" ? Number(overviewValue) : null;
  const hits = await invoke<SongHit[]>("search_songs", {
    query: "",
    sort: "title",
    collectionId: Number.isFinite(collectionId as number) ? collectionId : null,
  });
  const counts = showCounts();
  // В «Обзор» попадают только песни, которые уже выходили в показ: на новом
  // компьютере список пуст и наполняется по мере работы (счётчик ведёт
  // `bumpShow`). Иначе после установки здесь висел бы алфавитный список песен
  // поставляемого сборника с нулём показов.
  const ranked = hits
    .map((hit) => ({
      ...hit,
      shows: counts[String(hit.id)] || 0,
    }))
    .filter((hit) => hit.shows > 0)
    .sort((a, b) => b.shows - a.shows || a.title.localeCompare(b.title, "ru"))
    .slice(0, top);

  const body = $("#overview-body");
  body.replaceChildren();
  // Пустой список — показываем подсказку вместо таблицы, а не пустую сетку.
  const isEmpty = ranked.length === 0;
  $("#overview-empty").hidden = !isEmpty;
  $("#overview-table-wrap").hidden = isEmpty;
  ranked.forEach((hit, index) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${index + 1}</td><td>${hit.id}</td><td>${hit.title}</td><td>${hit.shows}</td>`;
    tr.addEventListener("click", async () => {
      switchTab("songs");
      await openSong(hit.id);
    });
    body.appendChild(tr);
  });
}

async function onSongSaved(song: SongDetail) {
  if (song.collection_id != null) {
    select("#song-collection").value = String(song.collection_id);
  }
  await loadSongs();
  await openSong(song.id);
  await loadOverview();
}

function clearSongSelection() {
  selectedSong = null;
  selectedSlideIndex = -1;
  pending = null;
  setSongsPreview(null);
  $("#songs-empty").hidden = false;
  $("#songs-detail").hidden = true;
  $("#song-title").textContent = "";
  $("#slide-list").replaceChildren();
  updateSlideStripe();
}

async function deleteSelectedSong() {
  if (!selectedSong) {
    window.alert("Сначала выберите песню.");
    return;
  }
  const ok = await confirmDialog(
    `Вы точно хотите удалить песню «${selectedSong.title}» (№ ${selectedSong.id})?`,
    { title: "Удаление песни", kind: "warning", okLabel: "Удалить", cancelLabel: "Отмена" },
  );
  if (!ok) {
    return;
  }
  const id = selectedSong.id;
  try {
    await invoke("delete_song", { id });
  } catch (error) {
    window.alert(String(error));
    return;
  }
  clearSongSelection();
  await loadSongs();
  await loadOverview();
}

function currentCollectionFromFilter(): Collection | null {
  const id = selectedCollectionFilter();
  if (id == null) {
    return null;
  }
  return collections.find((c) => c.id === id) || null;
}

async function onCollectionSaved(saved: Collection) {
  const title =
    saved.title ||
    String((saved as { name?: string }).name || "").trim() ||
    `Сборник ${saved.id}`;
  const normalized = { id: saved.id, title };
  const idx = collections.findIndex((c) => c.id === normalized.id);
  if (idx >= 0) {
    collections[idx] = normalized;
  } else {
    collections.push(normalized);
  }
  await loadCollections(normalized.id);
  select("#song-collection").value = String(normalized.id);
  await loadSongs();
  await loadOverview();
}

async function onCollectionDeleted() {
  clearSongSelection();
  await loadCollections();
  await loadSongs();
  await loadOverview();
}

async function loadBooks() {
  books = await invoke<string[]>("list_bible_books");
  renderBooks();
}

async function showBibleSearchResultOnScreen(verse: Verse) {
  await openTextInBroadcast(
    {
      title: `${verse.book} ${verse.chapter}:${verse.verse}`,
      slides: [verse.text],
      mode: "bible",
      verseRef: `${verse.book} ${verse.chapter}:${verse.verse}`,
      verseRefs: [`${verse.book} ${verse.chapter}:${verse.verse}`],
    },
    true,
    0,
  );
  switchTab("broadcast");
}

async function searchBibleQuery() {
  const query = input("#bible-search-query").value.trim();
  const requestId = ++bibleSearchRequestId;
  if (!query) {
    bibleSearchResults = [];
    bibleSearchIndex = -1;
    if (selectedBook) {
      await selectChapter(selectedChapter);
    }
    return;
  }
  bibleSearchResults = await invoke<Verse[]>("search_bible_query", { query });
  if (requestId !== bibleSearchRequestId) {
    return;
  }
  bibleSearchIndex = bibleSearchResults.length > 0 ? 0 : -1;
  // Поиск показывает собственный список — выделение относится к его результатам.
  bibleListMode = "search";
  selectedBibleVerseIndexes = [];
  const chapters = $("#bible-chapters");
  chapters.replaceChildren();
  chapters.hidden = true;
  fillList(
    $("#bible-verses"),
    bibleSearchResults.map((verse, index) => ({
      key: String(index),
      className: "slide-item",
      html: `<div class="slide-label">${verse.book} ${verse.chapter}:${verse.verse}</div><div class="slide-text">${verse.text}</div>`,
    })),
    (key, event) => {
      pickBibleVerse(Number(key), event);
    },
    bibleSearchIndex >= 0 ? String(bibleSearchIndex) : undefined,
    "selected",
  );
  // Первый результат сразу становится активным стихом — как было до мультивыделения.
  if (bibleSearchResults.length > 0) {
    pickBibleVerse(0);
  }
}

function renderBooks() {
  const panel = $("#bible-books");
  panel.replaceChildren();

  const groups = [
    { title: "Ветхий Завет", items: books.slice(0, OT_COUNT) },
    { title: "Новый Завет", items: books.slice(OT_COUNT) },
  ];

  groups.forEach((group, groupOffset) => {
    const title = document.createElement("div");
    title.className = "book-group-title";
    title.textContent = group.title;
    panel.appendChild(title);
    const grid = document.createElement("div");
    grid.className = "book-grid";
    group.items.forEach((book, localIndex) => {
      const index = groupOffset === 0 ? localIndex : OT_COUNT + localIndex;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `book-chip ${bookGroup(index)}${book === selectedBook ? " active" : ""}`;
      btn.textContent = book;
      btn.addEventListener("click", () => {
        void selectBook(book);
      });
      grid.appendChild(btn);
    });
    panel.appendChild(grid);
  });
}

async function selectBook(book: string) {
  selectedBook = book;
  selectedChapter = 1;
  renderBooks();
  $("#bible-heading").textContent = book;
  const count = await invoke<number>("bible_chapter_count", { book });
  const chapters = $("#bible-chapters");
  chapters.replaceChildren();
  for (let i = 1; i <= count; i++) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `chapter-chip${i === selectedChapter ? " active" : ""}`;
    btn.textContent = String(i);
    btn.addEventListener("click", () => {
      void selectChapter(i);
    });
    chapters.appendChild(btn);
  }
  await selectChapter(1);
}

async function selectChapter(chapter: number) {
  selectedChapter = chapter;
  // Показана глава — выделение относится к chapterVerses.
  bibleListMode = "chapter";
  selectedBibleVerseIndexes = [];
  selectedBibleVerseIndex = 0;
  $("#bible-chapters").hidden = false;
  document.querySelectorAll("#bible-chapters .chapter-chip").forEach((node) => {
    node.classList.toggle("active", node.textContent === String(chapter));
  });
  $("#bible-heading").textContent = selectedBook;
  $("#bible-chapter-sub").textContent = `Глава ${chapter}`;
  chapterVerses = await invoke<Verse[]>("get_bible_chapter", {
    book: selectedBook,
    chapter,
  });
  fillList(
    $("#bible-verses"),
    chapterVerses.map((verse, index) => ({
      key: String(index),
      className: "slide-item",
      html: `<div class="slide-label">Стих ${verse.verse}</div><div class="slide-text">${verse.text}</div>`,
    })),
    (key, event) => {
      pickBibleVerse(Number(key), event);
    },
  );
  setBiblePreview(null);
}

function loadAnnouncements() {
  announcements = loadJson<Announcement[]>(STORAGE.announcements, [
    {
      id: "phones",
      title: "Телефоны",
      text: "Братья и сёстры, пожалуйста, выключите звук на своих телефонах!!!",
    },
    {
      id: "prayer",
      title: "Молитва",
      text: "Сейчас время молитвы. Просим сохранять тишину.",
    },
  ]);
  renderAnnouncements();
}

function renderAnnouncements() {
  const q = input("#ann-query").value.trim().toLowerCase();
  const filtered = announcements.filter((item) => item.title.toLowerCase().includes(q));
  if (filtered.length === 0) {
    fillList(
      $("#ann-list"),
      [{ key: "-", className: "list-status", html: "Ничего не найдено" }],
      () => undefined,
    );
  } else {
    fillList(
      $("#ann-list"),
      filtered.map((item) => ({
        key: item.id,
        className: "song-item",
        html: `<span class="song-title-text">${item.title}</span>`,
        title: item.title,
      })),
      (key) => selectAnnouncement(key),
      selectedAnnId,
      "selected",
    );
  }
  if (!selectedAnnId && filtered[0]) {
    selectAnnouncement(filtered[0].id);
  }
}

function selectedAnnouncement(): Announcement | null {
  return announcements.find((a) => a.id === selectedAnnId) ?? null;
}

function currentAnnSlides(): string[] {
  const item = selectedAnnouncement();
  return item ? parseAnnSlides(item.text) : [];
}

function selectAnnouncement(id: string) {
  selectedAnnId = id;
  selectedAnnSlide = 0;
  const item = announcements.find((a) => a.id === id);
  document.querySelectorAll("#ann-list li").forEach((node) => {
    node.classList.toggle("selected", (node as HTMLElement).dataset.key === id);
  });
  if (!item) {
    $("#ann-empty").hidden = false;
    $("#ann-detail").hidden = true;
    $("#ann-heading").textContent = "";
    $("#ann-slide-list").replaceChildren();
    updateAnnSlideStripe();
    setAnnPreview(null);
    return;
  }
  $("#ann-empty").hidden = true;
  $("#ann-detail").hidden = false;
  $("#ann-heading").textContent = item.title;
  renderAnnSlides();
  pickAnnSlide(0);
}

function renderAnnSlides() {
  fillList(
    $("#ann-slide-list"),
    currentAnnSlides().map((slide, index) => ({
      key: String(index),
      className: "slide-item",
      html: `<div class="slide-label">Слайд ${index + 1}</div><div class="slide-text">${slide}</div>`,
    })),
    (key) => {
      pickAnnSlide(Number(key));
    },
    selectedAnnSlide >= 0 ? String(selectedAnnSlide) : undefined,
    "selected",
  );
  requestAnimationFrame(updateAnnSlideStripe);
}

function pickAnnSlide(index: number) {
  const slides = currentAnnSlides();
  if (index < 0 || index >= slides.length) {
    return;
  }
  selectedAnnSlide = index;
  const lines = slides[index].split("\n").filter((line) => line.length > 0);
  // На экран и в превью уходит только текст объявления — без заголовка.
  setAnnPreview({ lines, mode: "announcement" });
  document.querySelectorAll("#ann-slide-list .slide-item").forEach((node) => {
    node.classList.toggle(
      "selected",
      (node as HTMLElement).dataset.key === String(index),
    );
  });
  requestAnimationFrame(updateAnnSlideStripe);
}

function updateAnnSlideStripe() {
  const host = document.getElementById("ann-slide-host");
  const stripe = document.getElementById("ann-slide-stripe");
  const selected = document.querySelector<HTMLElement>(
    "#ann-slide-list .slide-item.selected",
  );
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

/* ——— Редактор объявления (модальное окно) ——— */

function annEditor(): HTMLDialogElement {
  return $("#ann-editor") as HTMLDialogElement;
}

function openAnnEditor(mode: "create" | "edit") {
  const editing = mode === "edit" ? selectedAnnouncement() : null;
  if (mode === "edit" && !editing) {
    window.alert("Сначала выберите объявление.");
    return;
  }
  const dlg = annEditor();
  dlg.dataset.mode = mode;
  input("#ann-edit-title").value = editing?.title ?? "";
  ($("#ann-edit-text") as HTMLTextAreaElement).value = editing?.text ?? "";
  dlg.showModal();
}

function saveAnnEditor() {
  const dlg = annEditor();
  const title = input("#ann-edit-title").value.trim() || "Без названия";
  const text = ($("#ann-edit-text") as HTMLTextAreaElement).value;
  const editing = dlg.dataset.mode === "edit" ? selectedAnnouncement() : null;
  if (editing) {
    editing.title = title;
    editing.text = text;
  } else {
    selectedAnnId = `ann-${Date.now()}`;
    announcements.unshift({ id: selectedAnnId, title, text });
  }
  saveJson(STORAGE.announcements, announcements);
  dlg.close();
  renderAnnouncements();
  if (selectedAnnId) {
    selectAnnouncement(selectedAnnId);
  }
}

async function deleteSelectedAnnouncement() {
  const item = selectedAnnouncement();
  if (!item) {
    window.alert("Сначала выберите объявление.");
    return;
  }
  if (!(await confirmDialog(`Удалить объявление «${item.title}»?`))) {
    return;
  }
  announcements = announcements.filter((a) => a.id !== item.id);
  selectedAnnId = announcements[0]?.id ?? "";
  saveJson(STORAGE.announcements, announcements);
  renderAnnouncements();
  selectAnnouncement(selectedAnnId);
}

/* ——— Быстрое объявление (модальное окно) ——— */

function annQuick(): HTMLDialogElement {
  return $("#ann-quick") as HTMLDialogElement;
}

/* ——— Обновление приложения (релизы GitHub) ——— */

type UpdateStatus = {
  currentVersion: string;
  latestVersion: string | null;
  available: boolean;
  skipped: boolean;
  mandatory: boolean;
  notes: string | null;
  publishedAt: string | null;
  downloadUrl: string | null;
  parts: string[] | null;
  sha256: string | null;
  sizeBytes: number | null;
  error: string | null;
  /** Каталог программы защищён: установка запросит права администратора. */
  needsElevation: boolean;
};

type UpdateProgress = { downloaded: number; total: number | null; percent: number | null };

/** Сведения о приложении для блока «О нас» (команда `get_app_info`). */
type AppInfo = { version: string; arch: string; os: string; identifier: string };

let pendingUpdate: UpdateStatus | null = null;
let updateProgressUnlisten: (() => void) | null = null;

function updateDialog(): HTMLDialogElement {
  return $("#update-dialog") as HTMLDialogElement;
}

/** Размер файла в человекочитаемом виде («12,4 МБ»). */
function formatSize(bytes: number | null): string {
  if (!bytes || bytes <= 0) {
    return "";
  }
  const units = ["Б", "КБ", "МБ", "ГБ"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const text = unit === 0 || value >= 10 ? Math.round(value).toString() : value.toFixed(1);
  return `${text} ${units[unit]}`;
}

/** Показывает список изменений: строка, заканчивающаяся двоеточием, — заголовок. */
function renderUpdateNotes(notes: string | null) {
  const list = document.getElementById("update-notes");
  if (!list) {
    return;
  }
  list.textContent = "";
  const lines = (notes ?? "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  if (lines.length === 0) {
    lines.push("Описание изменений не указано.");
  }
  for (const line of lines) {
    const item = document.createElement("li");
    const heading = /^[^:]{1,60}:$/.test(line) || line.startsWith("#");
    item.className = heading ? "update-note heading" : "update-note";
    item.textContent = line.replace(/^#+\s*/, "").replace(/^[-*•]\s*/, "");
    list.append(item);
  }
}

function setUpdateProgress(percent: number | null, label: string | null) {
  const bar = document.getElementById("update-progress-bar");
  if (bar) {
    bar.style.width = `${Math.min(100, Math.max(0, percent ?? 0))}%`;
  }
  const text = document.getElementById("update-progress-label");
  if (text) {
    text.textContent = label ?? "Скачивание…";
  }
}

/** Блокирует кнопки на время скачивания и показывает полосу прогресса. */
function setUpdateBusy(busy: boolean) {
  const install = document.getElementById("update-install") as HTMLButtonElement | null;
  if (install) {
    install.disabled = busy;
    install.textContent = busy ? "Идёт обновление…" : "Обновить";
  }
  const optional = pendingUpdate?.mandatory ?? false;
  const skip = document.getElementById("update-skip") as HTMLButtonElement | null;
  const later = document.getElementById("update-later") as HTMLButtonElement | null;
  if (skip) {
    skip.disabled = busy || optional;
  }
  if (later) {
    later.disabled = busy || optional;
  }
  const progress = document.getElementById("update-progress");
  if (progress) {
    progress.hidden = !busy;
  }
  if (!busy) {
    setUpdateProgress(0, null);
  }
}

function setUpdateError(message: string | null) {
  const el = document.getElementById("update-error");
  if (!el) {
    return;
  }
  el.hidden = !message;
  el.textContent = message ?? "";
}

function openUpdateDialog(status: UpdateStatus) {
  pendingUpdate = status;
  setUpdateError(null);
  setUpdateBusy(false);
  const versions = document.getElementById("update-versions");
  if (versions) {
    versions.textContent = status.mandatory
      ? `Требуется версия ${status.latestVersion} — у вас ${status.currentVersion}. Обновление обязательно.`
      : `Доступна версия ${status.latestVersion} — у вас ${status.currentVersion}.`;
  }
  const published = document.getElementById("update-published");
  if (published) {
    published.hidden = !status.publishedAt;
    published.textContent = status.publishedAt ? `Опубликовано: ${status.publishedAt}` : "";
  }
  // В «C:\Program Files» тихая установка ничего не заменит без прав
  // администратора — предупреждаем до скачивания, а не после отказа в UAC.
  const elevation = document.getElementById("update-elevation");
  if (elevation) {
    elevation.hidden = !status.needsElevation;
    elevation.textContent = status.needsElevation
      ? "Программа установлена в защищённую папку — при обновлении Windows запросит права администратора."
      : "";
  }
  renderUpdateNotes(status.notes);
  updateDialog().showModal();
}

/** Версия, разрядность и система в блоке «О нас». */
async function refreshAppInfo() {
  try {
    const info = await invoke<AppInfo>("get_app_info");
    const version = document.getElementById("about-version");
    if (version) {
      version.textContent = info.version;
    }
    const build = document.getElementById("about-build");
    if (build) {
      build.textContent = `Сборка ${info.arch} · ${info.os}`;
    }
  } catch (error) {
    logError("update", "не удалось получить сведения о приложении", error);
  }
}

/** Время последней проверки обновления для строки состояния. */
function checkedAt(): string {
  return new Date().toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
}

/** Строка состояния под кнопкой «Проверить обновление». */
function setAboutUpdateStatus(message: string | null, isError = false) {
  const el = document.getElementById("about-update-status");
  if (!el) {
    return;
  }
  el.hidden = !message;
  el.textContent = message ?? "";
  el.classList.toggle("error", isError);
}

/** Кнопка ручной проверки: пока идёт проверка, она заблокирована. */
function setCheckUpdatesBusy(busy: boolean) {
  const btn = document.getElementById("check-updates") as HTMLButtonElement | null;
  if (!btn) {
    return;
  }
  btn.disabled = busy;
  btn.textContent = busy ? "Проверка…" : "Проверить обновление";
}

/**
 * Проверка обновления. `force` — ручная проверка кнопкой в блоке «О нас»:
 * пропущенная версия предлагается снова, а результат виден и в диалоге,
 * и в строке состояния под кнопкой.
 */
async function checkForUpdates(force: boolean) {
  logInfo("update", force ? "ручная проверка обновлений" : "проверка обновлений при запуске");
  if (force) {
    setCheckUpdatesBusy(true);
    setAboutUpdateStatus("Проверка обновления…");
  }
  let status: UpdateStatus;
  try {
    status = await invoke<UpdateStatus>("check_app_update", { force });
  } catch (error) {
    logError("update", "не удалось проверить обновление", error);
    const message = "Не удалось проверить обновление. Подробности — в журнале работы.";
    setAboutUpdateStatus(message, true);
    if (force) {
      window.alert(message);
    }
    return;
  } finally {
    if (force) {
      setCheckUpdatesBusy(false);
    }
  }
  if (status.error) {
    logError("update", `ошибка проверки обновления: ${status.error}`);
    setAboutUpdateStatus(`Не удалось проверить обновление: ${status.error}`, true);
    if (force) {
      window.alert(`Не удалось проверить обновление:\n${status.error}`);
    }
    return;
  }
  if (!status.available) {
    logInfo("update", `обновлений нет: установлена ${status.currentVersion}`);
    const message = status.skipped
      ? `Версия ${status.latestVersion} пропущена — следующая будет предложена сама.`
      : `У вас последняя версия: ${status.currentVersion}.`;
    setAboutUpdateStatus(`${message} Проверено в ${checkedAt()}.`);
    if (force) {
      window.alert(message);
    }
    return;
  }
  logInfo("update", `доступна версия ${status.latestVersion}`);
  setAboutUpdateStatus(`Доступна версия ${status.latestVersion}. Проверено в ${checkedAt()}.`);
  openUpdateDialog(status);
}

/** «Пропустить эту версию» — версия запоминается до следующего релиза. */
async function skipCurrentUpdate() {
  const status = pendingUpdate;
  if (!status?.latestVersion) {
    updateDialog().close();
    return;
  }
  try {
    await invoke("skip_app_update", { version: status.latestVersion });
    logInfo("update", `версия ${status.latestVersion} пропущена до следующего релиза`);
    setAboutUpdateStatus(`Версия ${status.latestVersion} пропущена — следующая будет предложена сама.`);
  } catch (error) {
    logError("update", "не удалось запомнить пропущенную версию", error);
  }
  updateDialog().close();
}

/** «Обновить» — скачивание с прогрессом, затем установка и перезапуск. */
async function installUpdate() {
  const status = pendingUpdate;
  const hasParts = (status?.parts?.length ?? 0) > 0;
  if (!status || (!status.downloadUrl && !hasParts)) {
    setUpdateError("В описании обновления нет ссылок на файлы.");
    return;
  }
  setUpdateError(null);
  setUpdateBusy(true);
  setUpdateProgress(0, "Подготовка…");
  if (!updateProgressUnlisten) {
    updateProgressUnlisten = await listen<UpdateProgress>("update-progress", (event) => {
      const { downloaded, total, percent } = event.payload;
      const totalLabel = total ? ` из ${formatSize(total)}` : "";
      setUpdateProgress(
        percent,
        percent == null
          ? `Скачано ${formatSize(downloaded)}${totalLabel}`
          : `Скачивание… ${Math.round(percent)} %${totalLabel ? ` (${formatSize(downloaded)}${totalLabel})` : ""}`,
      );
    });
  }
  try {
    await invoke<string>("install_app_update", {
      url: status.downloadUrl,
      parts: status.parts,
      sha256: status.sha256,
    });
    setUpdateProgress(100, "Установка запущена. Приложение закроется и запустится заново.");
  } catch (error) {
    logError("update", "не удалось установить обновление", error);
    setUpdateBusy(false);
    setUpdateError(`Не удалось установить обновление: ${String(error)}`);
  }
}

function switchAnnQuickMode(mode: string) {
  document.querySelectorAll("[data-annquick-mode]").forEach((node) => {
    node.classList.toggle(
      "active",
      (node as HTMLElement).dataset.annquickMode === mode,
    );
  });
  document.querySelectorAll("[data-annquick-panel]").forEach((node) => {
    const el = node as HTMLElement;
    const match = el.dataset.annquickPanel === mode;
    el.hidden = !match;
    el.classList.toggle("active", match);
  });
}

function openAnnQuick() {
  input("#ann-quick-text").value = "";
  input("#ann-quick-plate").value = "";
  switchAnnQuickMode("manual");
  annQuick().showModal();
}

/** Показ быстрого объявления: формирует слайды и запускает показ через Трансляцию. */
async function showAnnQuick() {
  const mode =
    document.querySelector<HTMLElement>(".ann-quick-tab.active")?.dataset
      .annquickMode ?? "manual";
  let title = "Быстрое объявление";
  let slides: string[] = [];

  if (mode === "car") {
    const plate = input("#ann-quick-plate").value.trim();
    if (!plate) {
      window.alert("Введите госномер автомобиля.");
      return;
    }
    slides = [`Просьба убрать автомобиль госномер: ${plate.toUpperCase()}`];
    title = `Убрать автомобиль ${plate.toUpperCase()}`;
  } else {
    const text = ($("#ann-quick-text") as HTMLTextAreaElement).value;
    slides = text
      .split(/\n\s*\n/)
      .map((s) => s.trim())
      .filter(Boolean);
    if (slides.length === 0) {
      window.alert("Введите текст объявления.");
      return;
    }
    title = "Быстрое объявление";
  }

  const ok = await openTextInBroadcast({ title, slides, mode: "announcement" }, true, 0);
  if (ok) {
    annQuick().close();
    switchTab("broadcast");
  }
}

function bindSongsUi() {
  let songTimer = 0;
  input("#song-query").addEventListener("input", () => {
    window.clearTimeout(songTimer);
    songTimer = window.setTimeout(() => void loadSongs(), 120);
  });

  select("#song-collection").addEventListener("change", () => {
    void loadSongs();
  });

  document.querySelectorAll("[data-menu]").forEach((btn) => {
    btn.addEventListener("click", (event) => {
      event.stopPropagation();
      const id = (btn as HTMLElement).dataset.menu;
      if (!id) {
        return;
      }
      const panel = document.getElementById(id);
      if (!panel) {
        return;
      }
      const willOpen = panel.hidden;
      closeMenus();
      panel.hidden = !willOpen;
    });
  });

  document.addEventListener("click", () => closeMenus());

  document.querySelectorAll("[data-song-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      closeMenus();
      const action = (btn as HTMLElement).dataset.songAction;
      if (action === "manual") {
        openSongEditor({
          mode: "create",
          collections,
          onSaved: onSongSaved,
        });
        return;
      }
      if (action === "import-presentation") {
        void importSongFromPresentation();
        return;
      }
      if (action === "import-website") {
        void importSongFromWebsite();
        return;
      }
      if (action === "edit") {
        if (!selectedSong) {
          window.alert("Сначала выберите песню.");
          return;
        }
        openSongEditor({
          mode: "edit",
          song: selectedSong,
          collections,
          onSaved: onSongSaved,
        });
        return;
      }
      if (action === "delete") {
        void deleteSelectedSong();
      }
    });
  });

  document.querySelectorAll("[data-collection-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      closeMenus();
      const action = (btn as HTMLElement).dataset.collectionAction;
      if (action === "create") {
        openCollectionEditor({
          mode: "create",
          onSaved: onCollectionSaved,
        });
        return;
      }
      if (action === "edit") {
        const current = currentCollectionFromFilter();
        if (!current) {
          window.alert("Сначала выберите сборник в списке (не «Все песни»).");
          return;
        }
        openCollectionEditor({
          mode: "edit",
          collection: current,
          onSaved: onCollectionSaved,
        });
        return;
      }
      if (action === "delete") {
        const current = currentCollectionFromFilter();
        if (!current) {
          window.alert("Сначала выберите сборник в списке (не «Все песни»).");
          return;
        }
        openCollectionDeleteDialog({
          collection: current,
          others: collections.filter((c) => c.id !== current.id),
          onDeleted: onCollectionDeleted,
        });
        return;
      }
      if (action === "import-json") {
        void importLegacyChorusJson();
      }
    });
  });
  document.querySelectorAll("[data-sort]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const value = (btn as HTMLElement).dataset.sort;
      songSort = value === "id" ? "id" : "title";
      closeMenus();
      void loadSongs();
    });
  });

  $("#songs-start-show").addEventListener("click", () => {
    void startSongShow();
  });

  $("#song-to-playlist").addEventListener("click", () => {
    if (!selectedSong) {
      return;
    }
    void addSongToQuickPlaylist(
      selectedSong,
      selectedSlideIndex >= 0 ? selectedSlideIndex : 0,
    ).then(() => {
      // Без перехода в «Трансляцию»: остаёмся в «Песнях», чтобы добавлять несколько песен.
      updateSongToPlaylistButton();
    });
  });

  const slideHost = document.getElementById("slide-host");
  slideHost?.addEventListener("scroll", () => updateSlideStripe());
  document.getElementById("ann-slide-host")?.addEventListener("scroll", () => {
    updateAnnSlideStripe();
  });
  document.getElementById("bible-verse-host")?.addEventListener("scroll", () => {
    updateBibleVerseStripe();
  });
  window.addEventListener("resize", () => {
    placeNavStripe();
    updateSlideStripe();
    updateAnnSlideStripe();
    updateBibleVerseStripe();
  });

  songsPreviewFrame().addEventListener("load", () => {
    applyPreviewAspect(songsPreviewFrame(), previewAspect.width, previewAspect.height);
    if (pending?.mode === "song") {
      setSongsPreview(pending);
    } else {
      setSongsPreview(null);
    }
  });
}

function bind() {
  document.querySelectorAll(".nav-item").forEach((tab) => {
    tab.addEventListener("click", () => {
      const name = (tab as HTMLElement).dataset.tab;
      if (!name || (tab as HTMLButtonElement).disabled) {
        return;
      }
      switchTab(name);
      if (name === "overview") {
        void loadOverview();
      }
      if (name === "settings") {
        void loadMonitorsSettings();
      }
      if (name === "songs") {
        applyPreviewAspect(songsPreviewFrame(), previewAspect.width, previewAspect.height);
        requestAnimationFrame(updateSlideStripe);
        updateSongToPlaylistButton();
        void loadCollections().then(() => loadSongs());
      }
      if (name === "announcements") {
        applyPreviewAspect(annPreviewFrame(), previewAspect.width, previewAspect.height);
        requestAnimationFrame(updateAnnSlideStripe);
      }
      if (name === "bible") {
        applyPreviewAspect(biblePreviewFrame(), previewAspect.width, previewAspect.height);
        requestAnimationFrame(updateBibleVerseStripe);
      }
      if (name === "broadcast") {
        refreshBroadcastPreviewAspect(previewAspect.width, previewAspect.height);
      }
    });
  });

  $("#theme-toggle").addEventListener("click", () => {
    const next = document.documentElement.dataset.theme === "light" ? "dark" : "light";
    applyTheme(next);
  });
  const themeSelect = document.getElementById("interface-theme") as HTMLSelectElement | null;
  if (themeSelect) {
    const storedTheme = localStorage.getItem(STORAGE.theme) || "system";
    themeSelect.value = storedTheme;
    themeSelect.addEventListener("change", () => {
      applyTheme(themeSelect.value as "system" | "dark" | "light");
    });
  }
  const confirmClose = document.getElementById("confirm-close") as HTMLInputElement | null;
  if (confirmClose) {
    confirmClose.checked = localStorage.getItem(STORAGE.confirmClose) === "1";
    confirmClose.addEventListener("change", () => {
      localStorage.setItem(STORAGE.confirmClose, confirmClose.checked ? "1" : "0");
      void invoke("set_close_confirmation", { enabled: confirmClose.checked }).catch((error) => {
        console.error("Failed to update close confirmation", error);
      });
    });
    void invoke("set_close_confirmation", { enabled: confirmClose.checked }).catch((error) => {
      console.error("Failed to initialize close confirmation", error);
    });
  }
  bindSongsUi();
  bindSongEditor();
  bindImportUrlDialog();
  bindCollectionEditor();
  bindBroadcast({
    persistentEnabled: persistentDisplayEnabled,
    slideLabel,
    bumpShow,
    refreshIcons,
    previewAspect: () => previewAspect,
  });

  const broadcastPreview = document.getElementById("bc-preview-frame") as HTMLIFrameElement | null;
  songsPreviewFrame().addEventListener("load", () => refreshActiveStylePreviews());
  broadcastPreview?.addEventListener("load", () => refreshActiveStylePreviews());
  annPreviewFrame().addEventListener("load", () => {
    applyPreviewAspect(annPreviewFrame(), previewAspect.width, previewAspect.height);
    // iframe перезагрузился — повторить текущий слайд/пустое состояние.
    if (pending?.mode === "announcement") {
      setAnnPreview(pending);
    } else {
      setAnnPreview(null);
    }
    refreshActiveStylePreviews();
  });
  biblePreviewFrame().addEventListener("load", () => {
    applyPreviewAspect(biblePreviewFrame(), previewAspect.width, previewAspect.height);
    // iframe перезагрузился — повторить текущий стих/пустое состояние.
    if (pending?.mode === "bible") {
      setBiblePreview(pending);
    } else {
      setBiblePreview(null);
    }
    refreshActiveStylePreviews();
  });

  bindStylesUi({
    refreshIcons,
    previewFrames: () => {
      const frames: HTMLIFrameElement[] = [];
      const songs = songsPreviewFrame();
      if (songs) {
        frames.push(songs);
      }
      const broadcast = document.getElementById("bc-preview-frame") as HTMLIFrameElement | null;
      if (broadcast) {
        frames.push(broadcast);
      }
      frames.push(annPreviewFrame());
      frames.push(biblePreviewFrame());
      return frames;
    },
    previewBackgroundEnabled: (frame) =>
      frame.id !== "bc-preview-frame" || isBroadcastLive() || persistentDisplayEnabled(),
    // Фон стиля «жив» только во время показа или при постоянном фоне: иначе
    // сохранение стиля включало бы фон на пустом экране.
    backgroundEnabled: () => isBroadcastLive() || persistentDisplayEnabled(),
  });

  // Кастомные горячие клавиши (глобальный keydown + вкладка настроек).
  registerHotkeys({
    switchTab: (tab: string) => switchTab(tab),
    activeTab: () =>
      document.querySelector<HTMLElement>(".view.active")?.dataset.view ?? "",
    startShow: () => startShowForActiveView(),
    endShow: () => hotkeyEndShow(),
    nextSlide: () => stepSlides(1),
    prevSlide: () => stepSlides(-1),
  });
  bindHotkeysUi();

  select("#overview-collection").addEventListener("change", () => void loadOverview());
  $("#overview-reset").addEventListener("click", () => void resetShowCounts());

  let bibleSearchTimer = 0;
  input("#bible-search-query").addEventListener("input", () => {
    window.clearTimeout(bibleSearchTimer);
    bibleSearchTimer = window.setTimeout(() => void searchBibleQuery(), 120);
  });
  input("#bible-search-query").addEventListener("keydown", (event) => {
    if (event.key !== "Enter") {
      return;
    }
    event.preventDefault();
    const verse = bibleSearchResults[bibleSearchIndex >= 0 ? bibleSearchIndex : 0];
    if (verse) {
      void showBibleSearchResultOnScreen(verse);
    }
  });
  $("#bible-show").addEventListener("click", async () => {
    await startBibleVerseShow();
  });

  document.querySelectorAll("[data-ann-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      closeMenus();
      const action = (btn as HTMLElement).dataset.annAction;
      if (action === "create") {
        openAnnEditor("create");
      } else if (action === "edit") {
        openAnnEditor("edit");
      } else if (action === "delete") {
        void deleteSelectedAnnouncement();
      }
    });
  });

  input("#ann-query").addEventListener("input", () => renderAnnouncements());

  $("#ann-editor-form").addEventListener("submit", (event) => {
    event.preventDefault();
    saveAnnEditor();
  });
  $("#ann-edit-cancel").addEventListener("click", () => annEditor().close());

  $("#ann-show").addEventListener("click", async () => {
    await startAnnouncementShow();
  });

  // Быстрое объявление (модалка с двумя режимами).
  $("#ann-quick-btn").addEventListener("click", () => openAnnQuick());
  document.querySelectorAll("[data-annquick-mode]").forEach((btn) => {
    btn.addEventListener("click", () => {
      switchAnnQuickMode((btn as HTMLElement).dataset.annquickMode ?? "manual");
    });
  });
  input("#ann-quick-plate").addEventListener("input", (event) => {
    const el = event.target as HTMLInputElement;
    const pos = el.selectionStart ?? el.value.length;
    // Госномер всегда в верхнем регистре (автокапитализация).
    el.value = el.value.toUpperCase();
    el.setSelectionRange(pos, pos);
  });
  $("#ann-quick-cancel").addEventListener("click", () => annQuick().close());
  $("#ann-quick-show").addEventListener("click", () => void showAnnQuick());

  // Обновление приложения: кнопка в «О нас» и три действия в диалоге.
  document
    .getElementById("check-updates")
    ?.addEventListener("click", () => void checkForUpdates(true));
  $("#update-install").addEventListener("click", () => void installUpdate());
  $("#update-skip").addEventListener("click", () => void skipCurrentUpdate());
  $("#update-later").addEventListener("click", () => {
    const latest = pendingUpdate?.latestVersion;
    if (latest) {
      setAboutUpdateStatus(`Обновление до версии ${latest} отложено — напомним при следующем запуске.`);
    }
    updateDialog().close();
  });
  updateDialog().addEventListener("cancel", (event) => {
    // Обязательное обновление нельзя отложить клавишей Esc.
    if (pendingUpdate?.mandatory) {
      event.preventDefault();
    }
  });

  select("#overview-top").addEventListener("change", () => void loadOverview());

  const persistentToggle = document.getElementById("persistent-display") as HTMLInputElement | null;
  persistentToggle?.addEventListener("change", async () => {
    if (isSelectedMonitorPrimary()) {
      persistentToggle.checked = false;
      setPersistentDisplayEnabled(false);
      syncPersistentDisplayUi();
      window.alert("Постоянный фон можно включить только для внешнего (не основного) монитора.");
      return;
    }
    setPersistentDisplayEnabled(persistentToggle.checked);
    if (persistentToggle.checked) {
      await ensureDisplayWindow();
    } else {
      await closeDisplayWindow();
    }
    refreshActiveStylePreviews();
  });
  $("#backup-database").addEventListener("click", () => void backupDatabase());
  $("#restore-database").addEventListener("click", () => void restoreDatabase());
  $("#yandex-settings-save").addEventListener("click", () => void saveYandexSettings());
  $("#yandex-token-check").addEventListener("click", () => void checkYandexToken());
  $("#yandex-token-page").addEventListener("click", () => void openYandexTokenPage());
  $("#yandex-backup").addEventListener("click", () => void backupToYandex());
  $("#yandex-open-folder").addEventListener("click", () => void openYandexFolder());
  void refreshYandexStatus();
  // Вывод слов в OBS: настройки источника «Браузер».
  bindObsUi();
  document
    .getElementById("open-logs-folder")
    ?.addEventListener("click", () => void openJournalFolder());
  document.querySelectorAll("[data-settings-tab]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const tab = (btn as HTMLElement).dataset.settingsTab;
      if (tab === "translation") {
        // Состояние сервера вывода в OBS обновляем при каждом открытии вкладки.
        void refreshObsStatus();
      }
      if (tab === "logs") {
        void refreshLogsHint();
      }
      if (tab === "backups") {
        // Состояние токена обновляем при каждом открытии вкладки.
        void refreshYandexStatus();
      }
      if (tab === "about") {
        // Версию и сведения о сборке обновляем при каждом открытии вкладки.
        void refreshAppInfo();
      }
    });
  });
}

async function boot() {
  installGlobalLogging("controller");
  // Внешние ссылки уходят в системный браузер, а не в окно приложения.
  installExternalLinkHandler();
  logInfo("boot", "запуск интерфейса");
  applyTheme((localStorage.getItem(STORAGE.theme) as "system" | "dark" | "light") || "system");
  refreshIcons();
  // Хоткеи загружаем из БД до регистрации слушателей.
  await bootHotkeys().catch((error) => logError("boot", "не удалось загрузить горячие клавиши", error));
  try {
    bind();
  } catch (error) {
    // Один не найденный элемент не должен оставлять пустыми обзор и списки:
    // пишем ошибку в журнал и продолжаем запуск.
    logError("boot", "часть обработчиков не привязана — интерфейс может работать неполно", error);
  }
  requestAnimationFrame(placeNavStripe);
  logInfo("boot", "интерфейс привязан, горячие клавиши загружены");

  loadAnnouncements();
  setBiblePreview(null);
  setAnnPreview(null);
  setSongsPreview(null);

  await bootStyles();
  logInfo("boot", "стили загружены");
  // Ширины левых колонок («Песни», «Библия», «Объявления», «Трансляция») восстанавливаем из БД.
  await bootColumnSplitters().catch((error) => {
    logError("boot", "не удалось восстановить ширины колонок", error);
  });
  // Активный стиль сразу уходит и в OBS: оверлей, открытый до начала показа,
  // должен получить цвет, шрифт и переходы, а не оформление по умолчанию.
  void pushObsStyle(getActiveStyleConfig());

  await resolvePreviewAspect();
  refreshBroadcastPreviewAspect(previewAspect.width, previewAspect.height);
  const storedMonitor = localStorage.getItem(STORAGE.monitor);
  if (storedMonitor != null) {
    selectedMonitorIndex = Number(storedMonitor);
    setDisplayMonitorIndex(selectedMonitorIndex);
    await invoke("set_display_monitor", { index: selectedMonitorIndex }).catch(() => undefined);
  } else {
    setDisplayMonitorIndex(selectedMonitorIndex);
  }
  syncPersistentDisplayUi();
  if (persistentDisplayEnabled() && !isSelectedMonitorPrimary()) {
    await ensureDisplayWindow().catch(() => undefined);
  }

  await loadCollections();
  logInfo("boot", "коллекции загружены", { count: collections.length });
  // Песни грузим сразу — не ждём Библию, иначе вкладка «Песни» может открыться с пустым списком.
  const songsReady = loadSongs();
  await loadBooks();
  await songsReady;
  await loadOverview();
  if (books[0]) {
    await selectBook(books[0]);
  }
  refreshIcons();
  requestAnimationFrame(placeNavStripe);
  logInfo("boot", "запуск завершён", {
    books: books.length,
    announcements: announcements.length,
  });
  void logJournalLocation();
  void refreshLogsHint();
  void refreshAppInfo();
  // Проверку обновления запускаем после отрисовки интерфейса, чтобы не тормозить старт.
  window.setTimeout(() => void checkForUpdates(false), 3000);
}

void boot().catch((error) => {
  logError("boot", "критическая ошибка запуска", error);
});
