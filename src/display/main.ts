import { convertFileSrc, emit, emitTo, invoke, listen } from "../shared/ipc";
import { disableLogging, installGlobalLogging, logError, logInfo } from "../shared/logger";
import {
  EVENTS,
  PREVIEW_CHANNEL,
  type ClearPayload,
  type MediaControlPayload,
  type OverlayPayload,
  type PreviewMessage,
  type SetMediaPayload,
  type SetStylePayload,
  type SetTextPayload,
  type VideoLoopPayload,
  type VideoSeekPayload,
} from "../shared/events";
import {
  mediaKindFromPath,
  cleanSongLines,
  defaultStyleConfig,
  normalizeStyleConfig,
  resolveStyleMediaPath,
  type BibleCaptionPosition,
  type StyleConfig,
  type TransitionType,
} from "../shared/style";
import { captionSizePx } from "../shared/caption";

const PERSISTENT_STORAGE_KEY = "chyguislide.persistentDisplay";

const isPreviewFrame =
  new URLSearchParams(window.location.search).get("preview") === "1";

const root = document.querySelector<HTMLElement>("#display-root")!;
const layerBg = document.querySelector<HTMLElement>("#layer-bg")!;
const layerOverlay = document.querySelector<HTMLElement>("#layer-overlay")!;
const layerText = document.querySelector<HTMLElement>("#layer-text")!;
const paneA = document.querySelector<HTMLElement>("#text-a")!;
const paneB = document.querySelector<HTMLElement>("#text-b")!;

let frontIsA = true;
let currentMedia: HTMLVideoElement | HTMLImageElement | null = null;
let activeStyle: StyleConfig = defaultStyleConfig();
let lastPayload: SetTextPayload | null = null;
let frontContent: HTMLElement | null = null;
let currentBgPath: string | null = null;
let intentionalVideoPause = false;
// Медиа, запущенное из «Трансляции», приоритетнее фона стиля: повторное
// применение стиля (например, ответ на display:ping) не должно его затирать.
let broadcastMediaActive = false;
/**
 * Кадр погашен явной очисткой (`clearAllDisplayContent`). Оформление при этом
 * принимаем как обычно (цвет, шрифт, переходы), но фон и текст сами не
 * возвращаются: иначе после сохранения стиля на пустом (чёрном) экране
 * самопроизвольно появляется фон.
 */
let frameBlanked = false;
/** Фон активного стиля, отложенный до следующего показа (см. `frameBlanked`). */
let deferredStyleBackground = false;

function frontPane(): HTMLElement {
  return frontIsA ? paneA : paneB;
}

function backPane(): HTMLElement {
  return frontIsA ? paneB : paneA;
}

type BibleCaptionSpec =
  | {
      position: BibleCaptionPosition;
      flow: boolean;
      inline: boolean;
      floating: boolean;
    }
  | null;

function bibleCaptionSpec(payload: SetTextPayload): BibleCaptionSpec {
  if (payload.mode !== "bible" || !payload.verseRef) {
    return null;
  }
  if (!activeStyle.bibleCaptionEnabled) {
    return null;
  }
  const position = activeStyle.bibleCaptionPosition;
  const flow = position === "above" || position === "below";
  const inline = position === "inline-start" || position === "inline-end";
  return { position, flow, inline, floating: !flow && !inline };
}

function makeBibleCaption(position: BibleCaptionPosition, text: string): HTMLElement {
  const el = document.createElement("span");
  el.className = `bible-caption pos-${position}`;
  el.textContent = text;
  return el;
}

function pxOf(value: string): number {
  return Number.parseFloat(value) || 0;
}

/**
 * Auto-fit: подбирает максимальный font-size, при котором текст заполняет
 * панель по ширине и высоте без прокрутки. Стартовый размер — от высоты окна
 * (window.innerHeight * 0.15), затем плавная доводка по фактическому контенту.
 * Одна и та же логика работает и в реальном окне Display, и в превью-iframe.
 */
const AUTOFIT_BASE_PX = 100;
const AUTOFIT_START_RATIO = 0.15;

function fitNow(pane: HTMLElement, content: HTMLElement) {
  if (!content.isConnected) {
    return;
  }
  if (pane.clientWidth <= 0 || pane.clientHeight <= 0) {
    // Панель ещё не отрендерена — автофит повторит ResizeObserver/load.
    return;
  }
  const paneStyle = window.getComputedStyle(pane);
  const availW = Math.max(
    1,
    pane.clientWidth - pxOf(paneStyle.paddingLeft) - pxOf(paneStyle.paddingRight),
  );
  const availH = Math.max(
    1,
    pane.clientHeight - pxOf(paneStyle.paddingTop) - pxOf(paneStyle.paddingBottom),
  );
  const overflows = () =>
    content.scrollWidth > availW + 1 || content.scrollHeight > availH + 1;

  // Стартовый размер — от высоты окна (15% высоты).
  let fontSize = Math.max(12, Math.round(window.innerHeight * AUTOFIT_START_RATIO));

  // Линейная доводка: измеряем контент при базовом шрифте и масштабируем.
  content.style.fontSize = `${AUTOFIT_BASE_PX}px`;
  const baseH = content.scrollHeight;
  const baseW = content.scrollWidth;
  if (baseH > 0) {
    const scaleH = (availH * 0.98) / baseH;
    const scaleW = baseW > availW ? (availW * 0.98) / baseW : Number.POSITIVE_INFINITY;
    const scale = Math.min(scaleH, scaleW);
    if (Number.isFinite(scale) && scale > 0) {
      fontSize = AUTOFIT_BASE_PX * scale;
    }
  }
  fontSize = Math.min(Math.max(fontSize, 10), 480);
  content.style.fontSize = `${fontSize}px`;

  // Плавная доводка: вниз — страховка от округлений layout-прохода.
  for (let guard = 0; overflows() && guard < 60; guard += 1) {
    const next = Math.max(9, fontSize * 0.96);
    if (next === fontSize) {
      break;
    }
    fontSize = next;
    content.style.fontSize = `${fontSize}px`;
  }
  // …и вверх — добиваемся максимального заполнения панели.
  for (let guard = 0; guard < 40; guard += 1) {
    const next = fontSize * 1.03;
    if (next > 480) {
      break;
    }
    content.style.fontSize = `${next}px`;
    if (overflows()) {
      content.style.fontSize = `${fontSize}px`;
      break;
    }
    fontSize = next;
  }
  content.style.fontSize = `${fontSize}px`;
  // Подписи, прибитые к краям экрана, лежат вне `.slide-content` и «em» от
  // автофита не наследуют — размер передаём переменной. Считаем его по кадру
  // (`captionSizePx`): на экране коэффициент кадра равен 1, в превью-iframe он
  // меньше — вместе с ним уменьшаются и границы подписи, поэтому пропорция
  // «основной текст : подпись» в превью та же, что в окне вывода.
  pane.style.setProperty(
    "--sl-caption-px",
    `${captionSizePx(fontSize, window.innerHeight)}px`,
  );
}

/** Автофит запускается только после полной загрузки окна (window.load). */
let windowLoaded = document.readyState === "complete";
if (!windowLoaded) {
  window.addEventListener(
    "load",
    () => {
      windowLoaded = true;
      if (frontContent && frontContent.isConnected) {
        fitNow(frontPane(), frontContent);
      }
    },
    { once: true },
  );
}

function scheduleAutofit(pane: HTMLElement, content: HTMLElement) {
  const run = () => {
    if (windowLoaded && content.isConnected) {
      fitNow(pane, content);
    }
  };
  // Двойной rAF + таймер — layout обязан устояться после вставки контента.
  window.requestAnimationFrame(() => window.requestAnimationFrame(run));
  window.setTimeout(run, 60);
  // После загрузки шрифтов — пересчёт (метрики могли измениться).
  if (document.fonts?.ready) {
    void document.fonts.ready
      .then(() => run())
      .catch(() => undefined);
  }
}

// Реакция на любые изменения размера панели (окно, превью-iframe, монитор).
const paneResizeObserver = new ResizeObserver(() => {
  if (windowLoaded && frontContent && frontContent.isConnected) {
    fitNow(frontPane(), frontContent);
  }
});
paneResizeObserver.observe(paneA);
paneResizeObserver.observe(paneB);

function renderText(pane: HTMLElement, payload: SetTextPayload): HTMLElement {
  pane.replaceChildren();

  // Для песен — жёсткая фильтрация: только строчки текста, без заголовков
  // («Куплет N», «Припев», «Хор», «Bridge») и без шапки с названием песни.
  const lines =
    payload.mode === "song" ? cleanSongLines(payload.lines, payload.title) : payload.lines;
  // Заголовки (название песни/объявления) — служебные и на экран не выводятся никогда.
  const bodyPayload: SetTextPayload = { ...payload, title: undefined, lines };

  const caption = bibleCaptionSpec(bodyPayload);
  const captionEl =
    caption && bodyPayload.verseRef
      ? makeBibleCaption(caption.position, bodyPayload.verseRef)
      : null;

  const content = document.createElement("div");
  content.className = "slide-content";

  if (bodyPayload.title) {
    const title = document.createElement("div");
    title.className = "slide-title";
    title.textContent = bodyPayload.title;
    content.appendChild(title);
  } else if (captionEl && caption && caption.flow && caption.position === "above") {
    content.appendChild(captionEl);
  }

  const body = document.createElement("div");
  body.className = "slide-body";
  const rows = bodyPayload.lines
    .map((line) => line.trimEnd())
    .filter((line) => line.length > 0)
    .map((line) => {
      const row = document.createElement("div");
      row.textContent = line;
      return row;
    });

  if (captionEl && caption && caption.inline) {
    if (caption.position === "inline-start" && rows[0]) {
      rows[0].prepend(captionEl);
    } else if (caption.position === "inline-end" && rows[rows.length - 1]) {
      rows[rows.length - 1].appendChild(captionEl);
    }
  }

  for (const row of rows) {
    body.appendChild(row);
  }
  content.appendChild(body);

  if (captionEl && caption && caption.flow && caption.position === "below") {
    content.appendChild(captionEl);
  }

  pane.appendChild(content);

  if (captionEl && caption && caption.floating) {
    // Прибитые к краям экрана подписи — вне потока, поверх текста.
    pane.appendChild(captionEl);
  }

  scheduleAutofit(pane, content);
  return content;
}

/* ——— JS-анимации переходов. Портировано 1:1 из slide-transitions-demo.html. ——— */

let transitionSeq = 0;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

/* ——— Плавное появление и угасание кадра ———
   Старт и финиш трансляции идут строго через прозрачность главного контейнера
   (#display-root, переход задан в display.css). Переключение слайдов внутри
   показа кадр не гасит: там анимируются панели текста. */

/** Длительность перехода прозрачности — синхронно с `#display-root` в display.css. */
const FADE_MS = 400;

/** Номера команд гашения: новая команда отменяет незавершённую предыдущую. */
let rootFadeSeq = 0;
let textFadeSeq = 0;

/** Мгновенно (без анимации) гасит или проявляет элемент. */
function setFadeInstant(el: HTMLElement, hidden: boolean) {
  const previous = el.style.transition;
  el.style.transition = "none";
  el.classList.toggle("fade-out", hidden);
  void el.offsetHeight; // фиксируем состояние до возврата перехода
  el.style.transition = previous;
}

/**
 * Проявляет элемент следующим кадром: класс `.fade-out` снимается уже после
 * отрисовки подготовленного DOM, поэтому проявление идёт через CSS-переход.
 */
function revealNextFrame(el: HTMLElement) {
  window.requestAnimationFrame(() => el.classList.remove("fade-out"));
}

/**
 * Проявляет кадр после явной очистки и снимает признак «экран очищен».
 * Отложенный фон активного стиля возвращается именно здесь: до этого момента
 * показывать его нельзя — пустой экран обязан остаться пустым.
 */
function revealRootFrame() {
  frameBlanked = false;
  revealNextFrame(root);
  if (deferredStyleBackground) {
    deferredStyleBackground = false;
    applyStyleBackground(activeStyle);
  }
}

/** Снять inline-стили анимации на панели И строках — к чистому CSS-состоянию. */
function clearPaneAnimationStyles(pane: HTMLElement) {
  pane.style.removeProperty("opacity");
  pane.style.removeProperty("transform");
  pane.style.removeProperty("filter");
  pane.style.removeProperty("clip-path");
  pane.style.removeProperty("transition");
  // Строки: после построчного перехода уходящие линии остаются с inline
  // opacity:0/transform — сбрасываем, иначе следующий переход «не увидит» слайд.
  for (const line of slideRows(pane)) {
    line.style.removeProperty("opacity");
    line.style.removeProperty("transform");
    line.style.removeProperty("transition");
  }
}

/** Строки текста слайда — в демо это .line, у нас строки .slide-body. */
function slideRows(pane: HTMLElement): HTMLElement[] {
  return Array.from(pane.querySelectorAll<HTMLElement>(".slide-body > div"));
}

/**
 * Двухслойные переходы: next — панель с новым текстом (класс .visible уже
 * выставлен), prev — уходящая панель. seq — номер вызова setText: если пришёл
 * более новый вызов, анимация прекращается и не трогает панели, чтобы не
 * затереть его состояние.
 */
async function animateTransition(
  kind: Exclude<TransitionType, "none">,
  next: HTMLElement,
  prev: HTMLElement,
  ms: number,
  seq: number,
) {
  const dur = Math.max(16, ms);
  const active = () => seq === transitionSeq;

  clearPaneAnimationStyles(next);
  clearPaneAnimationStyles(prev);
  next.style.transition = "none";
  prev.style.transition = "none";
  next.style.opacity = "0";
  next.style.transform = "none";
  next.style.filter = "none";
  next.style.clipPath = "none";
  prev.style.transform = "none";
  prev.style.filter = "none";
  prev.style.clipPath = "none";

  // 01: fade — затемнение, смена, проявление (как runFade из демо).
  if (kind === "fade") {
    prev.style.opacity = "1";
    void next.offsetHeight; // force layout
    const easing = `${dur / 2}ms ease-in-out`;
    prev.style.transition = `opacity ${easing}`;
    prev.style.opacity = "0";
    await sleep(dur / 2);
    if (!active()) return;
    next.style.transition = `opacity ${easing}`;
    next.style.opacity = "1";
    await sleep(dur / 2);
    if (!active()) return;
    clearPaneAnimationStyles(next);
    clearPaneAnimationStyles(prev);
    return;
  }

  // 07: построчное появление — строки уходящего слайда уходят, нового — въезжают.
  if (kind === "stagger") {
    const showLines = slideRows(next);
    const hideLines = slideRows(prev);
    const stagger = 90;
    if (showLines.length > 0 && hideLines.length > 0) {
      next.style.opacity = "1";
      prev.style.opacity = "1";
      const ease = `${dur}ms cubic-bezier(0.22,0.61,0.36,1)`;
      for (const line of showLines) {
        line.style.transition = "none";
        line.style.opacity = "0";
        line.style.transform = "translateY(14px)";
      }
      void next.offsetHeight; // force layout — зафиксировать стартовые состояния строк
      showLines.forEach((l, idx) => {
        l.style.transition = `opacity ${ease}, transform ${ease}`;
        window.setTimeout(() => {
          if (active()) {
            l.style.opacity = "1";
            l.style.transform = "translateY(0px)";
          }
        }, idx * stagger);
      });
      hideLines.forEach((l, idx) => {
        l.style.transition = `opacity ${ease}, transform ${ease}`;
        window.setTimeout(() => {
          if (active()) {
            l.style.opacity = "0";
            l.style.transform = "translateY(-10px)";
          }
        }, idx * stagger);
      });
      await sleep(dur + (showLines.length - 1) * stagger);
      if (!active()) {
        return;
      }
      // Как в демо: возвращаем строки уходящего слайда на место.
      hideLines.forEach((l) => {
        l.style.transform = "translateY(0px)";
      });
      clearPaneAnimationStyles(next);
      clearPaneAnimationStyles(prev);
      return;
    }
    // Строк нет — строчную анимацию делать нечем, работаем как crossfade.
  }

  if (kind === "crossfade" || kind === "stagger") {
    // 02: crossfade — два слоя идут навстречу (как runCrossfade из демо).
    prev.style.opacity = "1";
    void next.offsetHeight; // force layout
    const e = `opacity ${dur}ms cubic-bezier(0.4,0,0.2,1)`;
    next.style.transition = e;
    prev.style.transition = e;
    next.style.opacity = "1";
    prev.style.opacity = "0";
  } else if (kind === "fade-slide") {
    // 03: fade + slide — новый «подъезжает» снизу, старый уходит вверх (как runFadeSlide).
    next.style.transform = "translateY(18px)";
    prev.style.opacity = "1";
    void next.offsetHeight; // force layout
    const e = `opacity ${dur}ms cubic-bezier(0.22,0.61,0.36,1), transform ${dur}ms cubic-bezier(0.22,0.61,0.36,1)`;
    next.style.transition = e;
    prev.style.transition = e;
    next.style.opacity = "1";
    next.style.transform = "translateY(0px)";
    prev.style.opacity = "0";
    prev.style.transform = "translateY(-14px)";
  } else {
    // 05: blur → фокус — новый проявляется из размытия (как runBlur).
    next.style.filter = "blur(10px)";
    prev.style.opacity = "1";
    void next.offsetHeight; // force layout
    const e = `opacity ${dur}ms ease-out, filter ${dur}ms ease-out`;
    next.style.transition = e;
    prev.style.transition = e;
    next.style.opacity = "1";
    next.style.filter = "blur(0px)";
    prev.style.opacity = "0";
    prev.style.filter = "blur(6px)";
  }

  await sleep(dur);
  if (!active()) {
    return;
  }
  clearPaneAnimationStyles(next);
  clearPaneAnimationStyles(prev);
}

/**
 * Показывает ли слой фона что-то прямо сейчас. По этому признаку видно, можно ли
 * гасить весь кадр для плавного появления первого слайда: при постоянном фоне
 * (второй экран) кадр уже виден, и его гашение выглядит как «моргание» фона.
 */
function isBackgroundOnScreen(): boolean {
  return layerBg.classList.contains("visible") && layerBg.style.display !== "none";
}

/**
 * Глубокая проверка «на экране уже этот же слайд». Сравниваем всё, что реально
 * выводится (строки, режим, подпись стиха); `title` не участвует — он служебный
 * и в DOM не попадает.
 *
 * Зачем: повторный клик по активному слайду присылает идентичный payload.
 * Пересборка DOM сбрасывает идущие анимации перехода и сбрасывает автофит,
 * из-за чего текст на экране едва заметно «моргает».
 */
function isSameSlide(previous: SetTextPayload | null, next: SetTextPayload): boolean {
  if (!previous || previous.mode !== next.mode) {
    return false;
  }
  if ((previous.verseRef ?? "") !== (next.verseRef ?? "")) {
    return false;
  }
  const before = previous.lines ?? [];
  const after = next.lines ?? [];
  return before.length === after.length && before.every((line, index) => line === after[index]);
}

function setText(payload: SetTextPayload) {
  if (isSameSlide(lastPayload, payload)) {
    console.log("[display] set-text: тот же слайд — DOM не перестраиваем");
    return;
  }
  // Новый слайд отменяет незавершённую очистку: иначе её таймер сработает уже
  // после отрисовки и сотрёт только что показанный текст.
  rootFadeSeq += 1;
  textFadeSeq += 1;

  const next = backPane();
  const prev = frontPane();
  const hadPrev = prev.classList.contains("visible");
  const rootHidden = root.classList.contains("fade-out");
  const textHidden = layerText.classList.contains("fade-out");
  // Экран пуст (старт трансляции или «Очистить»): слайд готовим в погашенном
  // кадре, а проявляем его снятием `.fade-out` на следующем кадре.
  const freshStart = rootHidden || textHidden || !hadPrev;
  // Кадр с уже проявленным фоном («Постоянный фон на втором экране») целиком гасить
  // нельзя: фон «моргает» чёрным на старте показа. В этом случае проявляем только
  // слой текста, а фон остаётся на экране без перерыва.
  const keepFrame = freshStart && !rootHidden && !textHidden && isBackgroundOnScreen();
  if (keepFrame) {
    setFadeInstant(layerText, true);
  } else if (freshStart && !rootHidden && !textHidden) {
    setFadeInstant(root, true);
  }

  const content = renderText(next, payload);
  const seq = ++transitionSeq;
  const kind = activeStyle.transitionType;
  const ms = Math.max(0, Number(activeStyle.transitionMs) || 0);

  // Снять inline-стили возможной предыдущей JS-анимации.
  clearPaneAnimationStyles(next);
  clearPaneAnimationStyles(prev);

  next.classList.add("visible");
  prev.classList.remove("visible");
  frontIsA = !frontIsA;
  lastPayload = payload;
  frontContent = content;

  if (freshStart) {
    // Плавное появление: панели уже содержат готовый слайд.
    if (keepFrame) {
      revealNextFrame(layerText);
      return;
    }
    revealRootFrame();
    if (textHidden) {
      revealNextFrame(layerText);
    }
    return;
  }

  // Переключение слайдов внутри показа: кадр не гаснет, анимируются панели.
  // Все анимации (включая «fade») выполняются JS-функциями, портированными
  // 1:1 из демо.
  if (kind !== "none" && ms > 0) {
    void animateTransition(kind, next, prev, ms, seq);
    return;
  }
  // Тема «без перехода»: вместо резкой подмены — короткий кроссфейд, иначе
  // смена стиха выглядит как «моргание».
  void animateTransition("crossfade", next, prev, FADE_MS, seq);
}

/** Мгновенно убирает текст с экрана (плавный вариант — `clearTextWithFade`). */
function clearText() {
  const prev = frontPane();
  prev.classList.remove("visible");
  lastPayload = null;
  frontContent = null;
}

/**
 * Очистка только текста: слой текста сначала гаснет, и лишь после перехода
 * убирается DOM — резкое удаление выглядит как «моргание». Медиа на экране при
 * этом продолжает играть (кадр не гаснет).
 */
function clearTextWithFade() {
  const seq = ++textFadeSeq;
  layerText.classList.add("fade-out");
  window.setTimeout(() => {
    if (seq !== textFadeSeq) {
      return;
    }
    clearText();
  }, FADE_MS);
}

function destroyMedia() {
  broadcastMediaActive = false;
  if (!currentMedia) {
    return;
  }
  if (currentMedia instanceof HTMLVideoElement) {
    currentMedia.pause();
    currentMedia.removeAttribute("src");
    currentMedia.load();
  }
  currentMedia.remove();
  currentMedia = null;
  currentBgPath = null;
}

/**
 * Плавное угасание медиа: слой фона гаснет, элементы убираются после перехода.
 * Служебные флаги сбрасываются сразу — фоном снова управляет стиль, — а сам
 * элемент живёт до конца анимации, иначе кадр обрывается рывком.
 */
function fadeOutMedia(then?: () => void) {
  broadcastMediaActive = false;
  currentBgPath = null;
  const node = currentMedia;
  currentMedia = null;
  fadeBg(false);
  if (!node) {
    then?.();
    return;
  }
  if (node instanceof HTMLVideoElement) {
    node.pause();
  }
  window.setTimeout(() => {
    node.remove();
    then?.();
  }, FADE_MS);
}

function hexToRgba(hex: string, alpha: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec((hex || "").trim());
  if (!m) {
    return `rgba(0, 0, 0, ${Math.min(1, Math.max(0, alpha))})`;
  }
  const n = parseInt(m[1], 16);
  const r = (n >> 16) & 255;
  const g = (n >> 8) & 255;
  const b = n & 255;
  return `rgba(${r}, ${g}, ${b}, ${Math.min(1, Math.max(0, alpha))})`;
}

/** Multi-directional text-shadow contour (thin "outline" glow). */
function strokeShadows(cfg: Partial<StyleConfig>): string {
  const width = Math.max(0, Number(cfg.strokeWidth) || 0);
  if (width <= 0) {
    return "none";
  }
  const color = hexToRgba(cfg.strokeColor || "#000000", Number(cfg.strokeOpacity ?? 0.65));
  const step = Math.max(1, Math.round(width / 2));
  const n = Math.max(1, Math.round(width));
  const shadows: string[] = [];
  for (let dx = -n; dx <= n; dx += 1) {
    for (let dy = -n; dy <= n; dy += 1) {
      if (dx === 0 && dy === 0) {
        continue;
      }
      shadows.push(`${dx * step}px ${dy * step}px 0 ${color}`);
    }
  }
  return shadows.join(", ");
}

/** Set text / transition / background from the active presentation style. */
function applyStyleToDisplay(cfgIn: SetStylePayload) {
  const cfg = normalizeStyleConfig(cfgIn);
  activeStyle = cfg;
  const root = document.documentElement;
  const transitionMs = Math.max(0, Number(cfg.transitionMs) || 0);

  root.style.setProperty("--sl-text-color", cfg.textColor);
  root.style.setProperty("--sl-font-family", `"${cfg.fontFamily}", sans-serif`);
  root.style.setProperty("--sl-font-weight", cfg.bold ? "700" : "400");
  root.style.setProperty("--sl-align", cfg.align);
  root.style.setProperty("--sl-ms", `${transitionMs}ms`);

  if (Number(cfg.strokeWidth) > 0) {
    const color = hexToRgba(cfg.strokeColor, Number(cfg.strokeOpacity) || 0);
    root.style.setProperty("--sl-stroke", `${cfg.strokeWidth}px ${color}`);
    root.style.setProperty("--sl-text-shadow", strokeShadows(cfg));
    root.style.setProperty("--sl-caption-shadow", strokeShadows(cfg));
  } else {
    root.style.setProperty("--sl-stroke", "none");
    root.style.setProperty("--sl-text-shadow", "none");
    // Тема без обводки: подписи стиха всё равно нужна мягкая тень, иначе она
    // теряется на фоне — размер у неё почти как у основного текста.
    root.style.setProperty("--sl-caption-shadow", "0 2px 14px rgba(0, 0, 0, 0.85)");
  }

  // Все анимации переходов выполняет JS (см. setText) — CSS-переход панели выключаем.
  root.style.setProperty("--sl-transition", "none");

  applyStyleBackground(cfg);

  // Смена шрифта/выравнивания/подписи — перерисовать текущий слайд с автофитом.
  if (lastPayload) {
    frontContent = renderText(frontPane(), lastPayload);
  }
}

function applyStyleBackground(cfg: SetStylePayload) {
  const mediaMode = cfg.backgroundMode === "media" || cfg.backgroundMode === "random";
  const path = mediaMode ? resolveStyleMediaPath(cfg) : null;

  // Пока на экране медиа из «Трансляции», фон стиля не должен его перебивать.
  // Запоминаем желаемый путь: он применится, когда показ медиа закончится
  // (контроллер пришлёт display:set-media {kind:"none"}, затем стиль заново).
  if (broadcastMediaActive) {
    currentBgPath = path;
    return;
  }

  // Экран очищен: обновление стиля меняет только оформление. Фон запоминаем и
  // отдадим его при следующем показе (см. `revealRootFrame`) — на пустом кадре
  // он появляться не должен.
  if (frameBlanked) {
    deferredStyleBackground = true;
    return;
  }

  // Only update background if it actually changed (prevents flickering)
  if (path === currentBgPath && (path !== null) === (currentBgPath !== null)) {
    return;
  }

  currentBgPath = path;

  if (!mediaMode || !path) {
    // Цвет/градиент стиля: если на экране было медиа, сначала гасим слой фона и
    // только после перехода показываем новый фон — иначе кадр дёргается.
    const showColor = () => {
      layerBg.style.removeProperty("backgroundImage");
      layerBg.style.background = cfg.backgroundColor;
      fadeBg(true);
    };
    if (currentMedia) {
      fadeOutMedia(showColor);
    } else {
      showColor();
    }
    return;
  }
  layerBg.style.removeProperty("background");
  setMedia({ kind: mediaKindFromPath(path), path }, "style");
}

function parseStyleRow(row: unknown): StyleConfig | null {
  if (!row || typeof row !== "object") {
    return null;
  }
  const r = row as Record<string, unknown>;
  const raw = (r.configJson ?? r.config_json ?? "{}") as string;
  try {
    return normalizeStyleConfig(JSON.parse(raw) as Partial<StyleConfig>);
  } catch {
    return null;
  }
}

/** Real Display pulls the active style from the DB on boot and re-announce. */
async function applyActiveStyleFromBackend() {
  try {
    const row: unknown = await invoke("get_active_style");
    const cfg = parseStyleRow(row);
    if (cfg) {
      console.log("[display] applying active style");
      applyStyleToDisplay(cfg);
    }
  } catch (err) {
    console.warn("[display] apply active style failed", err);
  }
}

function reportMediaStatus() {
  if (isPreviewFrame || !(currentMedia instanceof HTMLVideoElement)) {
    return;
  }
  void emit(EVENTS.mediaStatus, {
    currentTime: currentMedia.currentTime,
    duration: Number.isFinite(currentMedia.duration) ? currentMedia.duration : 0,
    paused: currentMedia.paused,
  });
}

/**
 * Показывает или гасит слой фона. Слой не скрывается сразу через `display: none`
 * — сначала идёт переход прозрачности, иначе фон «моргает».
 */
function fadeBg(visible: boolean) {
  if (visible) {
    const wasHidden = layerBg.style.display === "none";
    layerBg.style.display = "block";
    if (wasHidden) {
      void layerBg.offsetHeight; // фиксируем показ до смены прозрачности
    }
    layerBg.classList.add("visible");
    return;
  }
  layerBg.classList.remove("visible");
  window.setTimeout(() => {
    if (!layerBg.classList.contains("visible")) {
      layerBg.style.display = "none";
    }
  }, FADE_MS);
}

/** Источник медиа: явный показ из «Трансляции» или фон активного стиля. */
type MediaOrigin = "style" | "broadcast";

/**
 * Ставит медиа в фоновый слой. Медиа из «Трансляции» (origin: "broadcast")
 * помечается как приоритетное, чтобы последующее применение стиля не вернуло
 * фон вместо него.
 */
function setMedia(payload: SetMediaPayload, origin: MediaOrigin = "broadcast") {
  if (payload.kind === "none" || !payload.path) {
    // Очистка медиа: слой фона гаснет, элементы убираются после перехода
    // (fadeOutMedia сразу сбрасывает broadcastMediaActive — фоном снова
    // управляет стиль).
    fadeOutMedia();
    return;
  }

  // Новый источник: старый убираем сразу — новый проявится по готовности
  // (loadeddata/load → fadeBg(true)).
  destroyMedia();
  fadeBg(false);

  if (origin === "broadcast") {
    broadcastMediaActive = true;
  }

  // Показ медиа отменяет незавершённую очистку и проявляет кадр.
  rootFadeSeq += 1;
  if (root.classList.contains("fade-out")) {
    revealRootFrame();
  }

  // Keep the filesystem path raw; convertFileSrc performs the required URL encoding
  // for spaces and non-ASCII characters before creating the asset URL.
  const src = convertFileSrc(payload.path);
  if (payload.kind === "video") {
    const video = document.createElement("video");
    video.src = src;
    video.autoplay = true;
    video.loop = true;
    video.setAttribute("loop", "");
    video.playsInline = true;
    video.muted = isPreviewFrame;
    layerBg.appendChild(video);
    currentMedia = video;
    video.addEventListener("play", () => {
      console.log("[display] background video play");
    });
    video.addEventListener("ended", () => {
      console.log("[display] background video ended; restarting");
      void video.play().catch(() => undefined);
    });
    video.addEventListener("stalled", () => {
      console.log("[display] background video stalled; restarting");
      void video.play().catch(() => undefined);
    });
    video.addEventListener("pause", () => {
      console.log("[display] background video pause", {
        intentional: intentionalVideoPause,
      });
      if (!intentionalVideoPause) {
        void video.play().catch(() => undefined);
      }
    });
    video.addEventListener("error", (event) => {
      const error = (event.target as HTMLVideoElement | null)?.error;
      console.error("[display] background video error", {
        code: error?.code,
        message: error?.message,
      });
    });
    video.addEventListener("loadeddata", () => fadeBg(true), { once: true });
    if (!isPreviewFrame) {
      video.addEventListener("timeupdate", reportMediaStatus);
    }
    void video.play().catch(() => undefined);
  } else {
    const img = document.createElement("img");
    img.src = src;
    img.alt = "";
    layerBg.appendChild(img);
    currentMedia = img;
    img.addEventListener("load", () => fadeBg(true), { once: true });
  }
}

function seekVideo(payload: VideoSeekPayload) {
  if (currentMedia instanceof HTMLVideoElement && Number.isFinite(payload.time)) {
    currentMedia.currentTime = Math.max(0, payload.time);
  }
}

function setVideoLoop(payload: VideoLoopPayload) {
  if (currentMedia instanceof HTMLVideoElement) {
    currentMedia.loop = Boolean(payload.loop);
  }
}

function controlMedia(payload: MediaControlPayload) {
  const video = currentMedia instanceof HTMLVideoElement ? currentMedia : null;

  switch (payload.action) {
    case "play":
      void video?.play();
      break;
    case "pause":
      if (video) {
        intentionalVideoPause = true;
        video.pause();
        intentionalVideoPause = false;
      }
      break;
    case "seek":
      if (video && payload.value != null) {
        video.currentTime = payload.value;
      }
      break;
    case "volume":
      if (video && payload.value != null) {
        video.volume = Math.min(1, Math.max(0, payload.value));
      }
      break;
    case "fade-out":
      // Плавное угасание: медиа убирается после перехода прозрачности.
      fadeOutMedia();
      break;
  }
}

function setOverlay(payload: OverlayPayload) {
  layerOverlay.style.opacity = String(Math.min(1, Math.max(0, payload.opacity)));
}

function handleClear(payload?: ClearPayload | null) {
  if (payload?.textOnly) {
    clearTextWithFade();
    return;
  }
  void clearAllDisplayContent();
}

function handlePreviewMessage(data: PreviewMessage) {
  if (!data || data.channel !== PREVIEW_CHANNEL) {
    return;
  }
  switch (data.type) {
    case EVENTS.setText:
      setText(data.payload);
      break;
    case EVENTS.clear:
      handleClear(data.payload);
      break;
    case EVENTS.setMedia:
      setMedia(data.payload);
      break;
    case EVENTS.mediaControl:
      controlMedia(data.payload);
      break;
    case EVENTS.videoSeek:
      seekVideo(data.payload);
      break;
    case EVENTS.videoSetLoop:
      setVideoLoop(data.payload);
      break;
    case EVENTS.setOverlay:
      setOverlay(data.payload);
      break;
    case EVENTS.setStyle:
      applyStyleToDisplay(normalizeStyleConfig(data.payload));
      break;
  }
}

/**
 * Полная очистка экрана трансляции. Кадр сначала плавно гаснет, и только после
 * перехода убирается DOM и останавливается медиа. Контейнер остаётся погашенным:
 * следующий показ проявляется сам (см. `setText`/`setMedia`).
 *
 * `instant` — аварийный вариант (Esc): гасим без анимации, чтобы успеть закрыть
 * окно, не дожидаясь перехода.
 */
async function clearAllDisplayContent(instant = false) {
  const seq = ++rootFadeSeq;
  if (instant) {
    setFadeInstant(root, true);
  } else {
    root.classList.add("fade-out");
    await sleep(FADE_MS);
  }
  if (seq !== rootFadeSeq) {
    return;
  }
  clearText();
  destroyMedia();
  layerBg.classList.remove("visible");
  layerBg.style.display = "none";
  layerOverlay.style.opacity = "0";
  // Экран очищен по команде: дальше принимаем только оформление, фон и текст
  // возвращаются исключительно новым показом.
  frameBlanked = true;
  deferredStyleBackground = false;
}

function isPersistentBackground(): boolean {
  try {
    return localStorage.getItem(PERSISTENT_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

async function emergencyEscape() {
  // Аварийное скрытие: Esc убирает кадр немедленно, поэтому здесь мгновенное
  // гашение без ожидания перехода.
  if (isPersistentBackground()) {
    // Постоянный фон: убираем только текст, медиа продолжает играть.
    clearText();
    return;
  }
  await clearAllDisplayContent(true);
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  } catch {
    // ignore
  }
}

async function announceReady(reason: string) {
  console.log(`[display] announce ready (${reason})`);
  try {
    await emit(EVENTS.displayReady, { reason });
    // Явно в controller — на случай если глобальный emit не дойдёт.
    await emitTo("controller", EVENTS.displayReady, { reason });
    console.log("[display] display:ready emitted");
  } catch (err) {
    console.error("[display] failed to emit display:ready", err);
    logError("display", "не удалось отправить display:ready", { error: String(err) });
  }
}

async function boot() {
  if (isPreviewFrame) {
    // Превью-iframe общается через postMessage — в файловый журнал не пишем.
    disableLogging();
  } else {
    installGlobalLogging("display");
    logInfo("display", "запуск окна вывода", { href: window.location.href });
  }

  window.addEventListener("message", (event) => {
    handlePreviewMessage(event.data as PreviewMessage);
  });

  // Автоподбор размера при изменении размеров окна/превью.
  let fitResizeTimer = 0;
  window.addEventListener(
    "resize",
    () => {
      window.clearTimeout(fitResizeTimer);
      fitResizeTimer = window.setTimeout(() => {
        if (frontContent && frontContent.isConnected) {
          fitNow(frontPane(), frontContent);
        }
      }, 150);
    },
    { passive: true },
  );

  // Preview iframe must NOT subscribe to Tauri events — only the real Display window does.
  if (isPreviewFrame) {
    document.documentElement.classList.add("preview-frame");
    console.log("[display] preview frame — skip Tauri listeners");
    return;
  }
  logInfo("display", "регистрация слушателей Tauri");

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    void emergencyEscape();
  });

  console.log("[display] registering Tauri listeners…");
  await listen<SetTextPayload>(EVENTS.setText, (event) => {
    console.log("[display] ← set-text", event.payload);
    setText(event.payload);
  });
  await listen<ClearPayload | null>(EVENTS.clear, (event) => {
    console.log("[display] ← clear", event.payload);
    handleClear(event.payload);
  });
  await listen<SetMediaPayload>(EVENTS.setMedia, (event) => {
    console.log("[display] ← set-media", event.payload);
    setMedia(event.payload);
  });
  await listen<MediaControlPayload>(EVENTS.mediaControl, (event) => {
    console.log("[display] ← media-control", event.payload);
    controlMedia(event.payload);
  });
  await listen<VideoSeekPayload>(EVENTS.videoSeek, (event) => {
    console.log("[display] ← video:seek", event.payload);
    seekVideo(event.payload);
  });
  await listen<VideoLoopPayload>(EVENTS.videoSetLoop, (event) => {
    console.log("[display] ← video:set-loop", event.payload);
    setVideoLoop(event.payload);
  });
  await listen<OverlayPayload>(EVENTS.setOverlay, (event) => {
    console.log("[display] ← set-overlay", event.payload);
    setOverlay(event.payload);
  });
  await listen<SetStylePayload>(EVENTS.setStyle, (event) => {
    console.log("[display] ← set-style", event.payload);
    applyStyleToDisplay(normalizeStyleConfig(event.payload));
  });
  await listen(EVENTS.displayPing, () => {
    console.log("[display] ← ping");
    // Переприменение стиля безопасно: пока идёт показ медиа из «Трансляции»,
    // applyStyleBackground не трогает медиа-слой (см. broadcastMediaActive).
    void applyActiveStyleFromBackend();
    void announceReady("ping");
  });

  // Apply persisted active style before announcing readiness.
  await applyActiveStyleFromBackend();
  await announceReady("boot");
  logInfo("display", "окно вывода готово к приёму команд");
}

void boot().catch((error) => {
  logError("display", "критическая ошибка окна вывода", error);
});
