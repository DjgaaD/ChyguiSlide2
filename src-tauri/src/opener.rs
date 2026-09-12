//! Открытие ссылок в браузере по умолчанию.
//!
//! Ссылка из интерфейса не должна уводить окно приложения: WebView2 переходит по
//! `<a href="https://…">` внутри себя — интерфейс исчезает, а вернуться назад
//! нечем. Поэтому фронтенд перехватывает клики по внешним ссылкам и зовёт
//! команду `open_external_link`, а она отдаёт ссылку системе (браузер, почтовый
//! клиент) тем же способом, что и кнопки настроек Яндекс.Диска.

use crate::logger;

/// Открывает внешнюю ссылку в браузере по умолчанию.
///
/// Разрешены только `http`/`https` и `mailto`: команда вызывается из интерфейса,
/// а `ShellExecuteW` запустил бы и любой другой протокол (`file:`, `ms-settings:`).
#[tauri::command]
pub fn open_external_link(url: String) -> Result<(), String> {
    let url = url.trim();
    if !is_external_url(url) {
        return Err(format!(
            "Открыть можно только ссылку http/https/mailto: {url}"
        ));
    }
    logger::info("app", &format!("открытие ссылки в системе: {url}"));
    open_url(url)
}

/// Ссылка, которую разрешено отдавать системе: сайт (`http`/`https`) или письмо
/// (`mailto`).
///
/// Регистр протокола не важен, окружающие пробелы отбрасываются.
pub fn is_external_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

/// Открывает ссылку средствами системы: браузер для сайта, почтовый клиент для
/// `mailto:`.
///
/// На Windows ссылка передаётся прямо в `ShellExecuteW`, а не в `cmd /C start`:
/// `cmd.exe` считает `&` разделителем команд и обрезал ссылку авторизации до
/// первого параметра — до Яндекса доходило только `response_type=code`, и
/// страница отвечала «Отсутствует обязательный параметр 'client_id'».
pub(crate) fn open_url(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let operation = wide("open");
        let file = wide(url);
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW сообщает об успехе значением больше 32 (документация Win32).
        if result as isize > 32 {
            Ok(())
        } else {
            Err(format!(
                "Не удалось открыть ссылку: ShellExecuteW вернул {}.",
                result as isize
            ))
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("Не удалось открыть ссылку: {error}"))
    }
}

/// Строка UTF-16 с завершающим нулём — в таком виде Win32-функции принимают текст.
#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Внешними считаются только адреса http/https и mailto: остальные протоколы
    /// запускают внешние программы, а не открывают страницу.
    #[test]
    fn external_url_requires_http_scheme() {
        assert!(is_external_url("https://oauth.yandex.ru/"));
        assert!(is_external_url("http://localhost:1420"));
        assert!(is_external_url("mailto:DjgaaD@ya.ru"));
        // Регистр и пробелы вокруг ссылки значения не имеют.
        assert!(is_external_url("  HTTPS://OAuth.Yandex.Ru/ "));
        assert!(is_external_url(" MAILTO:DjgaaD@ya.ru "));
        assert!(!is_external_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_external_url("ms-settings:privacy"));
        assert!(!is_external_url("javascript:alert(1)"));
        assert!(!is_external_url("oauth.yandex.ru"));
        assert!(!is_external_url(""));
    }

    /// Строка для Win32-вызова: UTF-16 с завершающим нулём и без потерь.
    ///
    /// Проверка сторожит ту самую ошибку: ссылка должна доходить до браузера
    /// целиком, вместе с `&` между параметрами.
    #[cfg(windows)]
    #[test]
    fn wide_string_is_utf16_with_nul() {
        let url = "https://oauth.yandex.ru/authorize?response_type=token&client_id=client-id";
        let value = wide(url);
        assert_eq!(value.len(), url.chars().count() + 1);
        assert_eq!(value.last(), Some(&0));
        assert_eq!(value[0], b'h' as u16);
        let round_trip = String::from_utf16(&value[..value.len() - 1]).expect("UTF-16");
        assert_eq!(round_trip, url);
        assert!(round_trip.contains("&client_id=client-id"));
    }
}
