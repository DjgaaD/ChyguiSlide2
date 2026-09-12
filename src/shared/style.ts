/** Shared presentation style (applied to Display + Preview). */

export type TextAlign = "left" | "center" | "right" | "justify";
export type TransitionType =
  | "fade"
  | "crossfade"
  | "fade-slide"
  | "blur"
  | "stagger"
  | "none";
export type BackgroundMode = "color" | "media" | "random";

/** Where the Bible reference caption is placed on the screen. */
export type BibleCaptionPosition =
  | "above" // Над текстом
  | "below" // Под текстом
  | "inline-start" // В начале текста (в строку)
  | "inline-end" // В конце текста (в строку)
  | "screen-top" // Сверху экрана, по центру
  | "corner-left" // В углу слева
  | "corner-right" // В углу справа
  | "screen-bottom-left" // Снизу экрана слева
  | "screen-bottom-right"; // Снизу экрана справа

export type StyleConfig = {
  textColor: string;
  fontFamily: string;
  bold: boolean;
  align: TextAlign;
  strokeWidth: number;
  strokeColor: string;
  strokeOpacity: number;
  transitionType: TransitionType;
  transitionMs: number;
  backgroundMode: BackgroundMode;
  backgroundColor: string;
  mediaPaths: string[];
  /** Selected media when backgroundMode === "media". */
  selectedMediaPath: string | null;
  /** Show the book/chapter/verse reference for Bible verses. */
  bibleCaptionEnabled: boolean;
  bibleCaptionPosition: BibleCaptionPosition;
};

export type StyleRecord = {
  id: number;
  name: string;
  config: StyleConfig;
  isActive: boolean;
};

export const STYLE_FONTS = [
  "Segoe UI",
  "Arial",
  "Calibri",
  "Cambria",
  "Georgia",
  "Times New Roman",
  "Verdana",
  "Tahoma",
  "Trebuchet MS",
  "Consolas",
  "Courier New",
  "Impact",
] as const;

export function defaultStyleConfig(): StyleConfig {
  return {
    textColor: "#ffffff",
    fontFamily: "Segoe UI",
    bold: true,
    align: "center",
    strokeWidth: 0,
    strokeColor: "#000000",
    strokeOpacity: 0.65,
    transitionType: "fade",
    transitionMs: 280,
    backgroundMode: "color",
    backgroundColor: "#000000",
    mediaPaths: [],
    selectedMediaPath: null,
    bibleCaptionEnabled: true,
    bibleCaptionPosition: "above",
  };
}

export function lightStyleConfig(): StyleConfig {
  return {
    ...defaultStyleConfig(),
    textColor: "#1a2230",
    bold: true,
    backgroundMode: "color",
    backgroundColor: "#f4f6fa",
    strokeWidth: 0,
  };
}

export function normalizeStyleConfig(raw: Partial<StyleConfig> | null | undefined): StyleConfig {
  const base = defaultStyleConfig();
  if (!raw || typeof raw !== "object") {
    return base;
  }
  return {
    textColor: String(raw.textColor || base.textColor),
    fontFamily: String(raw.fontFamily || base.fontFamily),
    bold: Boolean(raw.bold),
    align:
      raw.align === "left" ||
      raw.align === "right" ||
      raw.align === "justify" ||
      raw.align === "center"
        ? raw.align
        : base.align,
    strokeWidth: Math.max(0, Number(raw.strokeWidth) || 0),
    strokeColor: String(raw.strokeColor || base.strokeColor),
    strokeOpacity: Math.min(1, Math.max(0, Number(raw.strokeOpacity ?? base.strokeOpacity))),
    transitionType:
      raw.transitionType === "fade" ||
      raw.transitionType === "crossfade" ||
      raw.transitionType === "fade-slide" ||
      raw.transitionType === "blur" ||
      raw.transitionType === "stagger" ||
      raw.transitionType === "none"
        ? raw.transitionType
        : // Старый тип «slide» (сдвиг) заменён на «fade-slide» (появление со сдвигом).
        raw.transitionType === "slide"
          ? "fade-slide"
          : base.transitionType,
    transitionMs: Math.max(0, Number(raw.transitionMs) || base.transitionMs),
    backgroundMode:
      raw.backgroundMode === "media" ||
      raw.backgroundMode === "random" ||
      raw.backgroundMode === "color"
        ? raw.backgroundMode
        : base.backgroundMode,
    backgroundColor: String(raw.backgroundColor || base.backgroundColor),
    mediaPaths: Array.isArray(raw.mediaPaths)
      ? raw.mediaPaths.map(String).filter(Boolean)
      : [],
    selectedMediaPath: raw.selectedMediaPath ? String(raw.selectedMediaPath) : null,
    bibleCaptionEnabled:
      typeof raw.bibleCaptionEnabled === "boolean"
        ? raw.bibleCaptionEnabled
        : base.bibleCaptionEnabled,
    bibleCaptionPosition: isBibleCaptionPosition(raw.bibleCaptionPosition)
      ? raw.bibleCaptionPosition
      : base.bibleCaptionPosition,
  };
}

function isBibleCaptionPosition(value: unknown): value is BibleCaptionPosition {
  return (
    value === "above" ||
    value === "below" ||
    value === "inline-start" ||
    value === "inline-end" ||
    value === "screen-top" ||
    value === "corner-left" ||
    value === "corner-right" ||
    value === "screen-bottom-left" ||
    value === "screen-bottom-right"
  );
}

/** Pick media path for style background (null → solid color). */
export function resolveStyleMediaPath(config: StyleConfig): string | null {
  if (config.backgroundMode === "color") {
    return null;
  }
  const paths = config.mediaPaths.filter(Boolean);
  if (paths.length === 0) {
    return null;
  }
  if (config.backgroundMode === "random") {
    return paths[Math.floor(Math.random() * paths.length)] || null;
  }
  if (config.selectedMediaPath && paths.includes(config.selectedMediaPath)) {
    return config.selectedMediaPath;
  }
  return paths[0] || null;
}

export function mediaKindFromPath(path: string): "image" | "video" {
  if (/\.(mp4|m4v|webm|mkv|avi|mov|ogg|ogv|ts|m2ts|mpg|mpeg|flv|wmv|3gp)$/i.test(path)) {
    return "video";
  }
  return "image";
}

// ——— Чистка текста песни для экрана ———

/**
 * Служебные слова заголовков слайдов. Проверяются с границей слова,
 * поэтому обычные слова («хорошо», «припевающий») под фильтр не попадают.
 */
const SONG_HEADING_WORD_RE =
  /(?:инструментал|проигрыш|припев|куплет|бридж|bridge|chorus|хоры|хор)(?![a-zа-яё])/i;

/**
 * Полный заголовок слайда: служебное слово + номер / «(2 раза)» / знаки —
 * и ничего больше: «Куплет 2», «Припев (x2)», «Хор:», «Bridge», «Проигрыш».
 */
const SONG_HEADING_LINE_RE =
  /^(?:инструментал|проигрыш|припев|куплет|бридж|bridge|chorus|хоры|хор)(?![a-zа-яё])[.!:;x×*—–\-\s\d(]*(?:раза?|times?|repeat)?[).!:;x×*—–\-\s\d]*$/i;

/** Метка в начале строки текста: «Припев: Аллилуйя…», «Куплет 2. Текст…» — снимаем метку. */
export const SONG_LABEL_PREFIX_RE =
  /^(?:куплет\s*\d*|припев|хоры?|бридж|bridge|инструментал|проигрыш)\s*[.:;—–-]\s+/i;

/** Строка является полноценным заголовком слайда («Куплет 1», «Припев», «(Хор)», «Bridge x2»…). */
export function isSongHeadingLine(line: string): boolean {
  const raw = line.trim().toLowerCase();
  if (!raw || !SONG_HEADING_WORD_RE.test(raw)) {
    return false;
  }
  // Варианты обрамления: «(Припев)», «**Куплет 2**», «1. Припев», «— Хор —».
  const unwrapped = raw
    .replace(/^[([{«"'*·•—–\-\s\d]+/, "")
    .replace(/[)\]}»"'*·•—–\-\s\d]+$/, "")
    .trim();
  for (const candidate of [raw, unwrapped]) {
    if (candidate && SONG_HEADING_LINE_RE.test(candidate)) {
      return true;
    }
  }
  return false;
}

/**
 * Жёсткая фильтрация для режима «Песни»: на экран попадают ТОЛЬКО строки
 * текста песни. Отсекаются: любые строки-заголовки («Куплет N», «Припев»,
 * «Хор», «Bridge»… в любой части списка) и служебные строки без букв.
 * Строки-метки («Припев: …») остаются без метки.
 *
 * НЕ сравниваем строки с названием песни: первая строка куплета часто
 * является названием песни («Взойдем на Голгофу, мой брат!»), и такой
 * текст обязан попасть на экран.
 */
export function cleanSongLines(lines: string[], title?: string | null): string[] {
  void title;
  const out: string[] = [];
  for (const raw of lines) {
    let s = raw.trim();
    if (!s) {
      continue;
    }
    if (isSongHeadingLine(s)) {
      continue;
    }
    s = s.replace(SONG_LABEL_PREFIX_RE, "").trim();
    if (!s || isSongHeadingLine(s) || !/\p{L}/u.test(s)) {
      continue; // пустые/служебные строки без текста
    }
    out.push(s);
  }
  return out;
}
