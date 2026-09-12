/**
 * Внешние ссылки в интерфейсе.
 *
 * Переход по `<a href="https://…">` в WebView2 выполняется внутри окна
 * приложения: интерфейс исчезает, а вернуться назад нечем. Поэтому такие клики
 * (а также `mailto:`) перехватываются, а ссылку открывает система — так же, как
 * это делает кнопка «Получить токен» (Rust-команда `open_external_link`).
 */
import { invoke } from "./ipc";
import { logError, logInfo } from "./logger";

/** Ссылка, которую открывает система: сайт (http/https) или письмо (mailto). */
const EXTERNAL_HREF = /^(https?:\/\/|mailto:)/i;

/** Ближайшая внешняя ссылка вверх по дереву от места клика. */
function externalLink(target: EventTarget | null): HTMLAnchorElement | null {
  const node = target as Element | null;
  if (!node || typeof node.closest !== "function") {
    return null;
  }
  const link = node.closest("a[href]") as HTMLAnchorElement | null;
  const href = link?.getAttribute("href")?.trim() ?? "";
  return link && EXTERNAL_HREF.test(href) ? link : null;
}

/** Открывает ссылку в браузере по умолчанию. */
export async function openExternalLink(url: string): Promise<void> {
  try {
    await invoke("open_external_link", { url });
    logInfo("link", `ссылка открыта в браузере: ${url}`);
  } catch (error) {
    logError("link", `не удалось открыть ссылку: ${url}`, error);
    window.alert(`Не удалось открыть ссылку:\n${String(error)}`);
  }
}

/** Перехватывает клики по внешним ссылкам во всём окне. */
export function installExternalLinkHandler(): void {
  const openLink = (event: MouseEvent) => {
    const link = externalLink(event.target);
    if (!link) {
      return;
    }
    // Без preventDefault WebView2 ушёл бы по ссылке сам и окно стало бы браузером.
    event.preventDefault();
    void openExternalLink(link.href);
  };
  document.addEventListener("click", openLink, true);
  // Средняя кнопка мыши открывает ссылку отдельно и до `click` не доходит.
  document.addEventListener(
    "auxclick",
    (event) => {
      if (event.button === 1) {
        openLink(event);
      }
    },
    true,
  );
}
