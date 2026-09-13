/**
 * Размер подписи библейского стиха (ссылки «Быт 1:3»).
 *
 * Подпись — часть слайда и лишь немного уступает основному тексту (90% от него),
 * но у краёв экрана (`pos-screen-top`, углы) её размер задаётся отдельно: такая
 * подпись лежит вне блока с автофитом, и «em» основного текста ей не достаётся
 * (см. `.bible-caption` в `src/styles/display.css`).
 *
 * Границы 18…72px подобраны под кадр-эталон 1920×1080 — там подпись аккуратная и
 * не спорит с текстом. Кадр меньше эталона (превью-iframe во вкладках приложения)
 * — границы уменьшаются вместе с ним, поэтому пропорция «основной текст : подпись»
 * в превью ровно такая же, как в окне вывода. Без этого коэффициента подпись в
 * превью упирается в «экранный» максимум 72px и выглядит размером с основной
 * текст: автофит там даёт ~69px, и подпись получается 63px вместо ~26px.
 */

/** Доля от размера основного текста — как `font-size: 0.9em` в `.bible-caption`. */
export const CAPTION_SIZE_RATIO = 0.9;
/** Границы размера подписи на кадре-эталоне — как `clamp(18px, …, 72px)` в CSS. */
export const CAPTION_MIN_PX = 18;
export const CAPTION_MAX_PX = 72;
/** Высота кадра-эталона, под которую подобраны границы подписи. */
export const CAPTION_REFERENCE_HEIGHT = 1080;

/**
 * Коэффициент кадра: 1 — на экране-эталоне 1920×1080, меньше единицы — в
 * превью-iframe (кадр превью меньше настоящего экрана). Он же — коэффициент
 * масштабирования превью.
 */
export function captionFrameScale(frameHeightPx: number): number {
  return frameHeightPx > 0 ? frameHeightPx / CAPTION_REFERENCE_HEIGHT : 1;
}

/**
 * Размер подписи в px для кадра высотой `frameHeightPx` при основном тексте
 * `fontSizePx`: та же логика, что на экране (90% от текста в границах 18…72px),
 * но границы умножены на коэффициент кадра — см. `captionFrameScale`.
 */
export function captionSizePx(fontSizePx: number, frameHeightPx: number): number {
  const scale = captionFrameScale(frameHeightPx);
  return Math.round(
    Math.min(
      Math.max(fontSizePx * CAPTION_SIZE_RATIO, CAPTION_MIN_PX * scale),
      CAPTION_MAX_PX * scale,
    ),
  );
}
