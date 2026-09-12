/**
 * Логика оверлея OBS: показывает слова слайда и оформление активного стиля.
 *
 * Состояние приходит потоком событий (SSE) с сервера вывода: страница не
 * опрашивает сервер, а сразу получает снимок при подключении и каждое изменение
 * после него. Текст всегда приходит одной строкой (её собирает интерфейс) —
 * оверлей переносит её по ширине источника и подбирает размер шрифта так, чтобы
 * слова занимали источник целиком, как в прежней версии программы.
 *
 * Проявление и угасание кадра — через прозрачность `#stage` (см. obs-overlay.html),
 * переключение слайдов — кроссфейдом между слоями, подложка появляется только под
 * текстом.
 */
"use strict";

/** Длительность проявления/угасания кадра — синхронно с `#stage` в obs-overlay.html. */
const FADE_MS = 400;
/** Границы длительности перехода между слайдами (защита от «0» и «9999» в стиле). */
const TRANSITION_MIN_MS = 80;
const TRANSITION_MAX_MS = 3000;
/** Границы автоподбора размера шрифта: потолок — как в окне вывода. */
const AUTOFIT_MIN_PX = 10;
const AUTOFIT_MAX_PX = 480;
/** Отступ слов от краёв источника, доля от меньшей стороны. */
const AUTOFIT_PAD_RATIO = 0.02;

const stage = document.getElementById("stage");
const backdrop = document.getElementById("backdrop");
const layers = [document.getElementById("layer-a"), document.getElementById("layer-b")];

/** Какой слой сейчас на экране. */
let frontIndex = 0;
/** Последний полученный снимок: текст слайда, подложка и оформление. */
let snapshot = { slide: { text: "", mode: "" }, backdrop: null, style: null };
/** Ключ показанного слайда — по нему видно, нужно ли перерисовывать текст. */
let shownKey = "";
/** Есть ли сейчас слова на экране: подложка нужна только под текстом. */
let hasText = false;
/** Непрозрачность подложки из настроек вывода. */
let backdropEnabled = false;
let backdropOpacity = 0.9;
/** Непрозрачность из адреса страницы (`?backdrop=`) — сильнее настройки. */
const backdropOverride = backdropFromUrl();

applyStageSize();
applyBackdrop();
connect();

window.addEventListener("resize", refitAll);
// Метрики шрифта могли измениться после его загрузки — пересчитываем размер.
if (document.fonts?.ready) {
  void document.fonts.ready.then(refitAll).catch(() => undefined);
}

/* ——— Параметры страницы ——— */

function param(name) {
  return new URLSearchParams(window.location.search).get(name);
}

/** Фиксированный размер сцены: `?w=1920&h=1080`. */
function applyStageSize() {
  const width = Number.parseInt(param("w") || "", 10);
  const height = Number.parseInt(param("h") || "", 10);
  if (width > 0 && height > 0) {
    stage.style.width = `${width}px`;
    stage.style.height = `${height}px`;
    stage.style.position = "relative";
    stage.style.margin = "0 auto";
  }
}

/** Подложка из адреса страницы: `?backdrop=0.4` (доля) или `?backdrop=40` (проценты). */
function backdropFromUrl() {
  const raw = param("backdrop");
  if (raw == null || raw === "") {
    return null;
  }
  const value = Number.parseFloat(raw);
  if (!Number.isFinite(value)) {
    return null;
  }
  return clampOpacity(value > 1 ? value / 100 : value);
}

function clampOpacity(value) {
  return Math.min(1, Math.max(0, value));
}

/** Непрозрачность подложки: параметр адреса сильнее настройки вывода. */
function backdropValue() {
  if (backdropOverride != null) {
    return backdropOverride;
  }
  return backdropEnabled ? backdropOpacity : 0;
}

/** Подложка показывается только под словами: без текста фон остаётся прозрачным. */
function applyBackdrop() {
  const opacity = hasText ? backdropValue() : 0;
  backdrop.style.setProperty("--backdrop-opacity", String(opacity));
  backdrop.classList.toggle("on", opacity > 0);
}

/** Подложка из снимка состояния — настройки вкладки «Трансляция». */
function applyBackdropConfig(config) {
  if (!config || typeof config !== "object") {
    return;
  }
  backdropEnabled = Boolean(config.enabled);
  const opacity = Number(config.opacity);
  if (Number.isFinite(opacity)) {
    backdropOpacity = clampOpacity(opacity);
  }
  applyBackdrop();
}

/* ——— Приём состояния ——— */

function connect() {
  const events = new EventSource("/events");
  events.onmessage = (event) => {
    try {
      applySnapshot(JSON.parse(event.data));
    } catch {
      // Повреждённое сообщение пропускаем: следующее состояние всё исправит.
    }
  };
  events.onerror = () => {
    // Поток оборвался (перезапуск приложения или смена порта) — подключаемся снова.
    events.close();
    window.setTimeout(connect, 2000);
  };
}

function applySnapshot(next) {
  snapshot = next && typeof next === "object" ? next : snapshot;
  const slide = snapshot.slide || { text: "", mode: "" };
  applyTheme(snapshot.style);
  applyBackdropConfig(snapshot.backdrop);

  const key = String(slide.text || "");
  if (key === shownKey) {
    // Изменилось только оформление — пересчитываем размер шрифта.
    refitAll();
    return;
  }
  shownKey = key;
  applySlide(slide);
}

/* ——— Оформление ——— */

function applyTheme(style) {
  if (!style || typeof style !== "object") {
    return;
  }
  const root = document.documentElement;
  const strokeWidth = Math.max(0, Number(style.strokeWidth) || 0);
  root.style.setProperty("--text-color", style.textColor || "#f5f2ea");
  root.style.setProperty("--font-family", `"${style.fontFamily || "Segoe UI"}", sans-serif`);
  root.style.setProperty("--font-weight", style.bold ? "700" : "400");
  root.style.setProperty("--align", style.align || "center");
  // Без обводки текст терялся бы на светлом видео — оставляем мягкую тень.
  root.style.setProperty(
    "--text-shadow",
    strokeWidth > 0 ? strokeShadows(style, strokeWidth) : "0 2px 12px rgba(0, 0, 0, 0.65)",
  );
  backdrop.style.background = style.backgroundColor || "#000000";
}

function hexToRgba(hex, alpha) {
  const match = /^#?([0-9a-f]{6})$/i.exec(String(hex || "").trim());
  const value = Math.min(1, Math.max(0, alpha));
  if (!match) {
    return `rgba(0, 0, 0, ${value})`;
  }
  const number = Number.parseInt(match[1], 16);
  return `rgba(${(number >> 16) & 255}, ${(number >> 8) & 255}, ${number & 255}, ${value})`;
}

/** Обводка текста: та же сетка теней, что и в окне вывода (display.css). */
function strokeShadows(style, width) {
  const color = hexToRgba(style.strokeColor || "#000000", Number(style.strokeOpacity ?? 0.65));
  const step = Math.max(1, Math.round(width / 2));
  const size = Math.max(1, Math.round(width));
  const shadows = [];
  for (let dx = -size; dx <= size; dx += 1) {
    for (let dy = -size; dy <= size; dy += 1) {
      if (dx === 0 && dy === 0) {
        continue;
      }
      shadows.push(`${dx * step}px ${dy * step}px 0 ${color}`);
    }
  }
  return shadows.join(", ");
}

/* ——— Показ слайда ——— */

function applySlide(slide) {
  const text = String(slide.text || "").trim();
  hasText = text.length > 0;
  applyBackdrop();

  if (!hasText) {
    // Очистка: кадр гаснет, DOM убираем после перехода.
    stage.classList.add("fade-out");
    window.setTimeout(() => {
      if (!stage.classList.contains("fade-out")) {
        return;
      }
      for (const layer of layers) {
        layer.replaceChildren();
        layer.classList.remove("visible");
      }
    }, FADE_MS);
    return;
  }

  const hidden = layers[1 - frontIndex];
  const visible = layers[frontIndex];
  renderSlide(hidden, text);

  if (stage.classList.contains("fade-out") || !visible.classList.contains("visible")) {
    // Экран пуст: слайд подготовлен невидимым и проявляется следующим кадром.
    hidden.classList.add("visible");
    frontIndex = 1 - frontIndex;
    window.requestAnimationFrame(() => stage.classList.remove("fade-out"));
    return;
  }

  // Переключение слайдов внутри показа: короткий кроссфейд между слоями.
  const duration = transitionMs();
  hidden.classList.add("visible");
  hidden.style.transition = `opacity ${duration}ms ease-in-out`;
  visible.style.transition = `opacity ${duration}ms ease-in-out`;
  void hidden.offsetHeight;
  visible.classList.remove("visible");
  frontIndex = 1 - frontIndex;
  window.setTimeout(() => {
    hidden.style.transition = "";
    visible.style.transition = "";
  }, duration);
}

/** Длительность перехода между слайдами из активного стиля. */
function transitionMs() {
  const value = Number((snapshot.style || {}).transitionMs);
  if (!Number.isFinite(value) || value <= 0) {
    return FADE_MS;
  }
  return Math.min(Math.max(value, TRANSITION_MIN_MS), TRANSITION_MAX_MS);
}

/** Рисует слова слайда: текст всегда одна строка, перенос делает браузер. */
function renderSlide(layer, text) {
  layer.replaceChildren();
  const body = document.createElement("div");
  body.className = "slide-text";
  body.textContent = text;
  layer.appendChild(body);
  fitLayer(layer, body);
}

/* ——— Автоподбор размера текста ——— */

/** Пересчитывает размер шрифта во всех слоях (смена оформления, размер окна). */
function refitAll() {
  for (const layer of layers) {
    const body = layer.querySelector(".slide-text");
    if (body) {
      fitLayer(layer, body);
    }
  }
}

/**
 * Подбирает максимальный размер шрифта, при котором слова занимают источник
 * целиком и не вылезают за его края. Перебор размера по фактической раскладке —
 * та же логика, что была в прежней версии программы: текст заполняет источник и
 * не оставляет пустого места.
 */
function fitLayer(layer, body) {
  const width = layer.clientWidth;
  const height = layer.clientHeight;
  if (width <= 0 || height <= 0) {
    return;
  }

  // Небольшой отступ от краёв источника: буквы не липнут к рамке кадра.
  const pad = Math.max(4, Math.round(Math.min(width, height) * AUTOFIT_PAD_RATIO));
  const availableWidth = Math.max(40, width - pad * 2);
  const availableHeight = Math.max(24, height - pad * 2);

  body.style.width = `${availableWidth}px`;
  body.style.maxWidth = `${availableWidth}px`;

  let low = AUTOFIT_MIN_PX;
  let high = AUTOFIT_MAX_PX;
  let best = low;
  while (low <= high) {
    const middle = Math.floor((low + high) / 2);
    body.style.fontSize = `${middle}px`;
    const fits = body.scrollHeight <= availableHeight + 1 && body.scrollWidth <= availableWidth + 1;
    if (fits) {
      best = middle;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }
  body.style.fontSize = `${best}px`;
}

