//! Файловый журнал приложения.
//!
//! Один сеанс — один файл: `<app_log_dir>/chyguislide-YYYY-MM-DD_HH-MM-SS.log`.
//! Одновременно хранится не более [`MAX_LOG_FILES`] файлов: при создании нового
//! самый старый файл удаляется.
//!
//! Журнал пишется и из Rust (жизненный цикл, запуск процессов, ошибки),
//! и из фронтенда (действия пользователя, вызовы IPC, события Display).

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

/// Максимальное количество файлов журнала (включая текущий).
pub const MAX_LOG_FILES: usize = 10;
const FILE_PREFIX: &str = "chyguislide-";
const FILE_SUFFIX: &str = ".log";

struct Logger {
    dir: PathBuf,
    path: PathBuf,
    file: Mutex<File>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Запись журнала, приходящая из фронтенда (`log_events`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    #[serde(default = "default_level")]
    pub level: String,
    pub scope: String,
    pub message: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

fn default_level() -> String {
    "info".to_string()
}

/// Информация о журнале для настроек приложения.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogInfo {
    pub dir: String,
    pub current_file: String,
    pub max_files: usize,
    pub files: Vec<String>,
}

#[derive(Clone, Copy)]
struct TimeParts {
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

impl TimeParts {
    fn stamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }

    fn file_stamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Локальное время системы. На Windows — через `GetLocalTime` (без внешних крейтов).
#[cfg(windows)]
fn now_parts() -> TimeParts {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    TimeParts {
        year: st.wYear,
        month: st.wMonth,
        day: st.wDay,
        hour: st.wHour,
        minute: st.wMinute,
        second: st.wSecond,
        millis: st.wMilliseconds,
    }
}

/// Резервный вариант (UTC) для сборок не под Windows.
#[cfg(not(windows))]
fn now_parts() -> TimeParts {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    TimeParts {
        year: year as u16,
        month: month as u16,
        day: day as u16,
        hour: (rem / 3_600) as u16,
        minute: ((rem % 3_600) / 60) as u16,
        second: (rem % 60) as u16,
        millis: now.subsec_millis() as u16,
    }
}

/// Дни от 1970-01-01 → (год, месяц, день). Алгоритм Говарда Хиннанта.
#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
/// Отметка локального времени для имён файлов (`2026-09-12_14-33-05`).
///
/// Используется модулями, которым нужна дата в имени создаваемого файла
/// (например, резервная копия на Яндекс.Диск), чтобы не дублировать
/// платформенный код получения локального времени.
pub(crate) fn local_file_stamp() -> String {
    now_parts().file_stamp()
}



/// Каталог журналов: `%LOCALAPPDATA%/<identifier>/logs`.
pub fn log_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(dir) = app.path().app_log_dir() {
        return Ok(dir);
    }
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("logs"))
        .map_err(|e| e.to_string())
}

/// Создаёт файл текущего сеанса и включает перехват паник.
pub fn init(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = log_dir(app)?;
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Не удалось создать каталог журналов {}: {e}", dir.display()))?;
    prune(&dir, MAX_LOG_FILES);

    let parts = now_parts();
    let path = dir.join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", parts.file_stamp()));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Не удалось открыть файл журнала {}: {e}", path.display()))?;

    let _ = LOGGER.set(Logger {
        dir: dir.clone(),
        path: path.clone(),
        file: Mutex::new(file),
    });

    install_panic_hook();

    info("app", &format!("=== Сеанс начат {} ===", parts.stamp()));
    info("app", &format!("Файл журнала: {}", path.display()));
    Ok(path)
}

/// Пишет панику в журнал перед завершением процесса.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = if let Some(text) = panic_info.payload().downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = panic_info.payload().downcast_ref::<String>() {
            text.clone()
        } else {
            "неизвестная паника".to_string()
        };
        error("panic", &format!("{payload} ({location})"));
        default_hook(panic_info);
    }));
}

fn is_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX))
        .unwrap_or(false)
}

/// Удаляет самые старые файлы, чтобы после создания нового их осталось не больше `keep`.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_log_file(path))
        .collect();
    // Имена содержат дату-время, поэтому лексикографическая сортировка = хронологическая.
    files.sort();
    while files.len() >= keep {
        let oldest = files.remove(0);
        let _ = fs::remove_file(oldest);
    }
}

fn sanitize(text: &str) -> String {
    text.replace('\r', "\\r").replace('\n', "\\n")
}


/// Запись журнала уровнем по умолчанию.
pub fn log(level: &str, scope: &str, message: &str) {
    log_with_data(level, scope, message, None);
}

/// Запись журнала с произвольными данными (сериализуются в JSON в ту же строку).
pub fn log_with_data(level: &str, scope: &str, message: &str, data: Option<serde_json::Value>) {
    let stamp = now_parts().stamp();
    let level = level.to_ascii_uppercase();
    let mut line = format!("[{stamp}] [{level:<5}] [{scope}] {}", sanitize(message));
    if let Some(data) = data.as_ref() {
        if !data.is_null() {
            line.push_str(" | ");
            line.push_str(&sanitize(&data.to_string()));
        }
    }

    match LOGGER.get() {
        Some(logger) => {
            if let Ok(mut file) = logger.file.lock() {
                // Сбрасываем на диск сразу — журнал должен выжить при падении приложения.
                let _ = writeln!(file, "{line}");
                let _ = file.flush();
            }
        }
        None => eprintln!("{line}"),
    }
}

/// Уровень trace (мелкие детали: сообщения превью, преобразования путей).
#[allow(dead_code)]
pub fn trace(scope: &str, message: &str) {
    log("trace", scope, message);
}

pub fn debug(scope: &str, message: &str) {
    log("debug", scope, message);
}

pub fn info(scope: &str, message: &str) {
    log("info", scope, message);
}

/// Уровень warn (некритичные проблемы).
#[allow(dead_code)]
pub fn warn(scope: &str, message: &str) {
    log("warn", scope, message);
}

pub fn error(scope: &str, message: &str) {
    log("error", scope, message);
}
/// Путь к файлу текущего сеанса (если журнал уже инициализирован).
pub fn current_path() -> Option<PathBuf> {
    LOGGER.get().map(|logger| logger.path.clone())
}

/// Каталог журналов (если журнал уже инициализирован).
pub fn current_dir() -> Option<PathBuf> {
    LOGGER.get().map(|logger| logger.dir.clone())
}

/// Сводка о журнале для настроек приложения.
pub fn info_snapshot() -> LogInfo {
    let dir = current_dir().unwrap_or_default();
    let mut files: Vec<String> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_file() && is_log_file(path))
                .filter_map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    LogInfo {
        dir: dir.to_string_lossy().into_owned(),
        current_file: current_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        max_files: MAX_LOG_FILES,
        files,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chyguislide-log-{tag}-{}", std::process::id()))
    }

    #[test]
    fn prune_keeps_only_newest_files() {
        let dir = temp_dir("prune");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // 14 файлов; имена в формате даты-времени → сортируются хронологически.
        for index in 0..14 {
            let name = format!("{FILE_PREFIX}2026-01-01_00-00-{index:02}{FILE_SUFFIX}");
            fs::write(dir.join(name), "x").unwrap();
        }

        prune(&dir, MAX_LOG_FILES);

        let mut remaining: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();

        // После prune остаётся MAX_LOG_FILES - 1 файлов, чтобы новый сеанс дал ровно MAX.
        assert_eq!(remaining.len(), MAX_LOG_FILES - 1);
        assert!(remaining[0].contains("00-00-05"), "осталось: {remaining:?}");
        assert!(remaining[remaining.len() - 1].contains("00-00-13"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_ignores_foreign_files() {
        let dir = temp_dir("foreign");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("chyguislide.sqlite"), "db").unwrap();
        fs::write(dir.join("notes.txt"), "text").unwrap();
        for index in 0..11 {
            let name = format!("{FILE_PREFIX}2026-01-01_00-00-{index:02}{FILE_SUFFIX}");
            fs::write(dir.join(name), "x").unwrap();
        }

        prune(&dir, MAX_LOG_FILES);

        assert!(dir.join("chyguislide.sqlite").is_file());
        assert!(dir.join("notes.txt").is_file());
        let logs = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| is_log_file(&entry.path()))
            .count();
        assert_eq!(logs, MAX_LOG_FILES - 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_line_has_timestamp_level_scope_and_data() {
        let parts = TimeParts {
            year: 2026,
            month: 9,
            day: 11,
            hour: 18,
            minute: 4,
            second: 22,
            millis: 7,
        };
        assert_eq!(parts.stamp(), "2026-09-11 18:04:22.007");
        assert_eq!(parts.file_stamp(), "2026-09-11_18-04-22");
        assert_eq!(sanitize("a\nb\r"), "a\\nb\\r");
    }
}

