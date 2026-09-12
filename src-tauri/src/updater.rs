//! Автообновление приложения «по воздуху» через публичный репозиторий GitHub.
//!
//! Описание новой версии читается как обычный файл репозитория по анонимной
//! ссылке
//! `https://raw.githubusercontent.com/<владелец>/<репозиторий>/<ветка>/<файл>`
//! — ни токен, ни авторизация не нужны.
//!
//! Источник описания — `Update.md` с машинночитаемым блоком JSON. Порядок
//! перебора: `Update.md`, затем `update.json` (чистый JSON, если когда-нибудь
//! появится) и `README.md` — последний оставлен для совместимости с описанием,
//! которое раньше лежало прямо в README. Схема блока:
//!
//! ```json
//! {
//!   "version": "0.2.0",
//!   "publishedAt": "11.09.2026",
//!   "mandatory": false,
//!   "minSupported": "0.1.0",
//!   "notes": "Что нового:\n- ...",
//!   "downloadUrl": "https://github.com/DjgaaD/ChyguiSlide2/releases/download/v0.2.0/ChyguiSlide_0.2.0_x64-setup.exe",
//!   "sha256": "…",
//!   "sizeBytes": 278000000
//! }
//! ```
//!
//! GitHub принимает в ассеты релиза `.exe` (до 2 ГБ на файл), поэтому
//! установщик публикуется одним файлом и в манифесте достаточно `downloadUrl`.
//! Поле `parts` (несколько ссылок по порядку) поддерживается для совместимости:
//! части скачиваются по порядку, распаковываются (если это gzip), склеиваются в
//! один архив, проверяются по SHA-256 и распаковываются; установщик запускается
//! в тихом режиме (`/S /UPDATE /R` — флаги установщика Tauri NSIS).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::db;
use crate::logger;
use crate::AppState;

/// Репозиторий с релизами приложения.
const REPO: &str = "DjgaaD/ChyguiSlide2";
/// Адрес файлов репозитория в обход веб-интерфейса GitHub: отдаёт ровно
/// содержимое файла и не требует токена.
const RAW_BASE: &str = "https://raw.githubusercontent.com";
/// Ветки, в которых ищется описание обновления (по порядку).
const BRANCHES: [&str; 2] = ["master", "main"];
/// Файлы-источники описания обновления (по порядку приоритета): основной —
/// машинночитаемый `Update.md`, запасные — `update.json` и описание в README.
const SOURCES: [&str; 3] = ["Update.md", "update.json", "README.md"];
/// Строка-маркер машинночитаемого блока (сравнивается целиком).
const MANIFEST_MARKER: &str = "<!-- CHYGUISLIDE-UPDATE -->";
/// Настройка в базе: версия, обновление до которой пользователь пропустил.
const SKIPPED_VERSION_KEY: &str = "update.skipped_version";
/// Таймаут запроса описания релиза.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Таймаут скачивания архива обновления.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Событие с прогрессом скачивания для интерфейса.
const PROGRESS_EVENT: &str = "update-progress";

/// Описание доступной версии (блок JSON из `Update.md` или `update.json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub min_supported: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    /// Части обновления по порядку — запасной путь для хостингов, которые не
    /// принимают крупный установщик одним файлом. На GitHub файл отдаётся
    /// целиком, поэтому обычно используется `downloadUrl`.
    #[serde(default)]
    pub parts: Option<Vec<String>>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

/// Результат проверки обновления для интерфейса.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub available: bool,
    pub skipped: bool,
    pub mandatory: bool,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    pub download_url: Option<String>,
    pub parts: Option<Vec<String>>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub error: Option<String>,
}

impl UpdateStatus {
    /// Статус без данных о версии — для сбоев самой проверки.
    fn failed(current_version: String, error: String) -> Self {
        Self {
            current_version,
            latest_version: None,
            available: false,
            skipped: false,
            mandatory: false,
            notes: None,
            published_at: None,
            download_url: None,
            parts: None,
            sha256: None,
            size_bytes: None,
            error: Some(error),
        }
    }
}

/// Прогресс скачивания архива обновления.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    downloaded: u64,
    total: Option<u64>,
    percent: Option<f64>,
}

/// Достаёт описание обновления из текста файла: либо это чистый JSON
/// (`update.json`), либо блок в ограде ```` ``` ```` сразу после строки-маркера.
pub fn parse_manifest(text: &str) -> Option<UpdateManifest> {
    if let Ok(manifest) = serde_json::from_str::<UpdateManifest>(text.trim()) {
        return Some(manifest);
    }

    let lines: Vec<&str> = text.lines().collect();
    let marker = lines.iter().position(|line| line.trim() == MANIFEST_MARKER)?;
    let open = marker
        + lines[marker..]
            .iter()
            .position(|line| line.trim_start().starts_with("```"))?;
    let body_start = open + 1;
    let close = body_start
        + lines[body_start..]
            .iter()
            .position(|line| line.trim_start().starts_with("```"))?;
    serde_json::from_str::<UpdateManifest>(&lines[body_start..close].join("\n")).ok()
}

/// Разбирает версию вида `1.2.3`, `v1.2.3` или `1.2.3-beta.1`.
/// Последний элемент — признак стабильного релиза: предварительная сборка
/// (`1.2.3-beta.1`) считается младше релиза `1.2.3`.
fn parse_version(value: &str) -> Option<(u64, u64, u64, bool)> {
    let trimmed = value.trim().trim_start_matches('v').trim_start_matches('V');
    let (core, stable) = match trimmed.split_once('-') {
        Some((core, pre)) => (core, pre.trim().is_empty()),
        None => (trimmed, true),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").trim().parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").trim().parse::<u64>().ok()?;
    Some((major, minor, patch, stable))
}

/// `true`, если версия `candidate` новее установленной `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

// ——— Команды для интерфейса ———

/// Сведения о приложении для блока «О нас» в настройках.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Версия установленного приложения (та же, что у установщика).
    version: String,
    /// Разрядность сборки: «x64» или «ARM64».
    arch: String,
    /// Операционная система: «Windows», «Linux» или «macOS».
    os: String,
    /// Идентификатор приложения из `tauri.conf.json`.
    identifier: String,
}

/// Версия, разрядность и система — то, что показывается в блоке «О нас».
#[tauri::command]
pub fn get_app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        arch: match std::env::consts::ARCH {
            "x86_64" => "x64".to_string(),
            "aarch64" => "ARM64".to_string(),
            other => other.to_string(),
        },
        os: match std::env::consts::OS {
            "windows" => "Windows".to_string(),
            "linux" => "Linux".to_string(),
            "macos" => "macOS".to_string(),
            other => other.to_string(),
        },
        identifier: app.config().identifier.clone(),
    }
}

/// Проверяет, есть ли в репозитории версия новее установленной.
///
/// `force = true` — ручная проверка из настроек: пропущенная версия
/// предлагается снова, а ошибки сети возвращаются в поле `error`.
#[tauri::command(async)]
pub fn check_app_update(app: AppHandle, force: Option<bool>) -> UpdateStatus {
    let force = force.unwrap_or(false);
    let current_version = app.package_info().version.to_string();
    let job_version = current_version.clone();
    match run_blocking(move || build_update_status(&app, force, &job_version)) {
        Ok(status) => status,
        Err(error) => UpdateStatus::failed(current_version, error),
    }
}

/// Тело проверки: сеть и база — в отдельном потоке, вне рантайма Tauri.
fn build_update_status(app: &AppHandle, force: bool, current_version: &str) -> UpdateStatus {
    let mut status = UpdateStatus {
        current_version: current_version.to_string(),
        latest_version: None,
        available: false,
        skipped: false,
        mandatory: false,
        notes: None,
        published_at: None,
        download_url: None,
        parts: None,
        sha256: None,
        size_bytes: None,
        error: None,
    };

    let manifest = match fetch_manifest() {
        Ok(manifest) => manifest,
        Err(error) => {
            logger::warn("update", &format!("проверка обновления не удалась: {error}"));
            status.error = Some(error);
            return status;
        }
    };

    let download_url = manifest
        .download_url
        .clone()
        .filter(|value| !value.trim().is_empty());
    let parts: Vec<String> = manifest
        .parts
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    let mandatory = manifest.mandatory
        || manifest
            .min_supported
            .as_deref()
            .map(|min| is_newer(min, current_version))
            .unwrap_or(false);

    status.latest_version = Some(manifest.version.clone());
    status.mandatory = mandatory;
    status.notes = manifest.notes.clone();
    status.published_at = manifest.published_at.clone();
    status.download_url = download_url.clone();
    status.parts = if parts.is_empty() {
        None
    } else {
        Some(parts.clone())
    };
    status.sha256 = manifest.sha256.clone();
    status.size_bytes = manifest.size_bytes;

    if !is_newer(&manifest.version, current_version) {
        logger::info(
            "update",
            &format!("обновлений нет: установлена {current_version}, в репозитории {}", manifest.version),
        );
        return status;
    }

    if download_url.is_none() && parts.is_empty() {
        let error = format!("для версии {} не указаны ссылки на файлы", manifest.version);
        logger::warn("update", &error);
        status.error = Some(error);
        return status;
    }

    let skipped = skipped_version(app).as_deref() == Some(manifest.version.as_str());
    status.skipped = skipped && !force;
    status.available = !status.skipped;
    logger::info(
        "update",
        &format!(
            "доступна версия {} (установлена {current_version}, пропущена: {skipped}, обязательная: {mandatory})",
            manifest.version
        ),
    );
    status
}

/// Запоминает версию, которую пользователь попросил не предлагать.
#[tauri::command]
pub fn skip_app_update(app: AppHandle, version: String) -> Result<(), String> {
    let version = version.trim().to_string();
    if version.is_empty() {
        return Err("не указана версия обновления".to_string());
    }
    let state = app.state::<AppState>();
    let db = state.db.lock().map_err(|error| error.to_string())?;
    db::set_setting(&db, SKIPPED_VERSION_KEY, &version).map_err(|error| error.to_string())?;
    logger::info("update", &format!("версия {version} отмечена как пропущенная"));
    Ok(())
}

/// Скачивает файлы обновления (один архив или несколько частей), собирает их
/// в один файл, проверяет контрольную сумму, распаковывает установщик и
/// запускает его. После запуска приложение закрывается само: установщик
/// заменяет файлы и перезапускает программу.
#[tauri::command(async)]
pub fn install_app_update(
    app: AppHandle,
    url: Option<String>,
    parts: Option<Vec<String>>,
    sha256: Option<String>,
) -> Result<String, String> {
    run_blocking(move || install_update(&app, url, parts, sha256))?
}

/// Тело установки: скачивание и распаковка — в отдельном потоке.
fn install_update(
    app: &AppHandle,
    url: Option<String>,
    parts: Option<Vec<String>>,
    sha256: Option<String>,
) -> Result<String, String> {
    let mut sources: Vec<String> = parts
        .unwrap_or_default()
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    if sources.is_empty() {
        if let Some(url) = url
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            sources.push(url);
        }
    }
    if sources.is_empty() {
        return Err("в описании обновления нет ссылок на файлы".to_string());
    }
    if let Some(bad) = sources.iter().find(|source| !source.starts_with("https://")) {
        return Err(format!("ссылка должна начинаться с https://: {bad}"));
    }

    logger::info(
        "update",
        &format!("скачивание обновления: файлов — {}", sources.len()),
    );
    let dir = staging_dir(app)?;
    let downloaded = download_all(app, &sources, &dir)?;

    let archive = dir.join("update.bin");
    let actual = assemble_archive(&downloaded, &archive)?;
    logger::info("update", &format!("обновление собрано: {}", archive.display()));
    if let Some(expected) = sha256.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        if !expected.eq_ignore_ascii_case(&actual) {
            let _ = std::fs::remove_file(&archive);
            return Err(format!(
                "контрольная сумма не совпала: ожидалось {expected}, получено {actual}"
            ));
        }
        logger::info("update", "контрольная сумма архива совпала");
    } else {
        logger::warn("update", "в описании обновления нет sha256 — проверка пропущена");
    }

    let installer = resolve_installer(&archive, &dir)?;
    launch_installer(&installer)?;
    logger::info("update", &format!("установщик запущен: {}", installer.display()));

    // Даём IPC-ответу дойти до интерфейса и выходим: иначе установщик
    // не сможет заменить файлы запущенного приложения.
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        logger::info("update", "выход из приложения для установки обновления");
        handle.exit(0);
    });

    Ok(installer.display().to_string())
}

// ——— Вспомогательные функции ———

/// Выполняет блокирующую работу в обычном потоке и возвращает её результат.
///
/// `reqwest::blocking` создаёт собственный tokio-runtime, а вложенный runtime
/// нельзя ронять в контексте асинхронного рантайма Tauri: паника
/// «Cannot drop a runtime in a context where blocking is not allowed»
/// обрывает команду, и интерфейс не получает ответ. Обычный поток такого
/// контекста не имеет, поэтому вся сеть и распаковка идут здесь.
fn run_blocking<T, F>(job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::spawn(job)
        .join()
        .map_err(|_| "операция прервана внутренней ошибкой приложения".to_string())
}

/// Версия, обновление до которой пользователь просил не предлагать.
fn skipped_version(app: &AppHandle) -> Option<String> {
    let state = app.state::<AppState>();
    let db = state.db.lock().ok()?;
    db::get_setting(&db, SKIPPED_VERSION_KEY)
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
}

/// HTTP-клиент с общим User-Agent и таймаутом.
fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("ChyguiSlide/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("не удалось создать HTTP-клиент: {error}"))
}

/// Читает описание обновления из репозитория GitHub (анонимно, без токена).
///
/// Порядок перебора: ветка → файл. Ошибка соединения прерывает перебор —
/// дальше пробовать бессмысленно, а каждый запрос ждёт таймаут.
fn fetch_manifest() -> Result<UpdateManifest, String> {
    let client = http_client(REQUEST_TIMEOUT)?;
    let mut last_error = String::from("описание обновления не найдено");

    for branch in BRANCHES {
        for source in SOURCES {
            let url = format!("{RAW_BASE}/{REPO}/{branch}/{source}");
            let response = match client.get(&url).send() {
                Ok(response) => response,
                Err(error) => return Err(format!("нет связи с github.com: {error}")),
            };
            if !response.status().is_success() {
                last_error = format!("{url}: сервер вернул {}", response.status());
                continue;
            }
            let text = match response.text() {
                Ok(text) => text,
                Err(error) => {
                    last_error = format!("{url}: не удалось прочитать ответ ({error})");
                    continue;
                }
            };
            match parse_manifest(&text) {
                Some(manifest) => {
                    logger::info(
                        "update",
                        &format!(
                            "описание обновления получено из {source} (версия {})",
                            manifest.version
                        ),
                    );
                    return Ok(manifest);
                }
                None => last_error = format!("{url}: не найден блок {MANIFEST_MARKER}"),
            }
        }
    }

    Err(last_error)
}

/// Каталог для скачанного архива и распакованного установщика.
fn staging_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("не удалось определить каталог кэша: {error}"))?
        .join("updates");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("не удалось создать {}: {error}", dir.display()))?;
    Ok(dir)
}

/// Скачивает все файлы обновления во временный каталог, сообщая общий прогресс.
/// Возвращает пути скачанных файлов в порядке частей.
fn download_all(app: &AppHandle, sources: &[String], dir: &Path) -> Result<Vec<PathBuf>, String> {
    let client = http_client(DOWNLOAD_TIMEOUT)?;
    let mut files = Vec::new();
    let mut downloaded: u64 = 0;
    let mut total: Option<u64> = Some(0);
    let mut buffer = vec![0u8; 128 * 1024];

    for (index, source) in sources.iter().enumerate() {
        logger::info(
            "update",
            &format!("часть {} из {}: {source}", index + 1, sources.len()),
        );
        let mut response = client
            .get(source)
            .send()
            .map_err(|error| format!("не удалось скачать обновление: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("сервер вернул {status} при скачивании {source}"));
        }
        total = match (total, response.content_length()) {
            (Some(sum), Some(length)) => Some(sum + length),
            _ => None,
        };

        let target = dir.join(format!("part{}.download", index + 1));
        let mut file = std::fs::File::create(&target)
            .map_err(|error| format!("не удалось создать {}: {error}", target.display()))?;
        let mut last_report = Instant::now() - Duration::from_secs(1);
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("ошибка при скачивании обновления: {error}"))?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|error| format!("не удалось записать файл обновления: {error}"))?;
            downloaded += read as u64;
            if last_report.elapsed() >= Duration::from_millis(150) {
                report_progress(app, downloaded, total);
                last_report = Instant::now();
            }
        }
        file.flush()
            .map_err(|error| format!("не удалось сохранить файл обновления: {error}"))?;
        report_progress(app, downloaded, total);
        files.push(target);
    }

    logger::info("update", &format!("скачано {downloaded} байт"));
    Ok(files)
}

/// Собирает скачанные части в один файл и возвращает его SHA-256.
/// Части в gzip (`.gz`) распаковываются, остальные копируются как есть.
fn assemble_archive(parts: &[PathBuf], target: &Path) -> Result<String, String> {
    let mut out = std::fs::File::create(target)
        .map_err(|error| format!("не удалось создать {}: {error}", target.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];

    for part in parts {
        let file = std::fs::File::open(part)
            .map_err(|error| format!("не удалось открыть {}: {error}", part.display()))?;
        let mut reader = std::io::BufReader::new(file);
        if is_gzip(&mut reader)? {
            let mut decoder = flate2::read::GzDecoder::new(reader);
            copy_stream(&mut decoder, &mut out, &mut hasher, &mut buffer)?;
        } else {
            copy_stream(&mut reader, &mut out, &mut hasher, &mut buffer)?;
        }
    }

    out.flush()
        .map_err(|error| format!("не удалось сохранить обновление: {error}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Копирует поток в файл, попутно считая SHA-256.
fn copy_stream<R: Read>(
    reader: &mut R,
    out: &mut std::fs::File,
    hasher: &mut Sha256,
    buffer: &mut [u8],
) -> Result<(), String> {
    loop {
        let read = reader
            .read(buffer)
            .map_err(|error| format!("ошибка при чтении части обновления: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])
            .map_err(|error| format!("не удалось записать обновление: {error}"))?;
    }
}

/// Проверяет gzip-заголовок (`\x1f\x8b`) у части обновления.
fn is_gzip(reader: &mut std::io::BufReader<std::fs::File>) -> Result<bool, String> {
    use std::io::BufRead;
    let head = reader
        .fill_buf()
        .map_err(|error| format!("не удалось прочитать часть обновления: {error}"))?;
    Ok(head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b)
}

/// Читает начало файла — формат определяется по сигнатуре, а не по имени.
fn read_head(path: &Path, length: usize) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("не удалось открыть {}: {error}", path.display()))?;
    let mut head = vec![0u8; length];
    let read = file
        .read(&mut head)
        .map_err(|error| format!("не удалось прочитать {}: {error}", path.display()))?;
    head.truncate(read);
    Ok(head)
}

/// Отправляет в интерфейс прогресс скачивания (событие `update-progress`).
fn report_progress(app: &AppHandle, downloaded: u64, total: Option<u64>) {
    let percent = total
        .filter(|value| *value > 0)
        .map(|value| (downloaded as f64 / value as f64 * 100.0).min(100.0));
    let payload = DownloadProgress {
        downloaded,
        total,
        percent,
    };
    if let Err(error) = app.emit(PROGRESS_EVENT, payload) {
        logger::debug("update", &format!("не удалось отправить прогресс: {error}"));
    }
}

/// Достаёт установщик из собранного файла. Обычно файл обновления — сам
/// установщик `.exe`; архив `.zip` с установщиком внутри тоже поддерживается.
/// Формат определяется по содержимому, а не по расширению.
fn resolve_installer(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    if looks_like_exe(archive)? {
        // Исполняемый файл должен иметь расширение `.exe`, иначе Windows его не запустит.
        let renamed = archive.with_file_name("update.exe");
        std::fs::rename(archive, &renamed)
            .map_err(|error| format!("не удалось подготовить установщик: {error}"))?;
        return Ok(renamed);
    }
    if !looks_like_zip(archive)? {
        return Err(format!("неизвестный формат обновления: {}", archive.display()));
    }

    let unpacked = dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked)
        .map_err(|error| format!("не удалось создать {}: {error}", unpacked.display()))?;

    let file = std::fs::File::open(archive)
        .map_err(|error| format!("не удалось открыть архив обновления: {error}"))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|error| format!("не удалось прочитать архив обновления: {error}"))?;
    zip.extract(&unpacked)
        .map_err(|error| format!("не удалось распаковать архив обновления: {error}"))?;

    find_installer(&unpacked, 0)
        .ok_or_else(|| "в архиве обновления не найден установщик .exe".to_string())
}

/// Проверяет сигнатуру ZIP-архива (`PK\x03\x04`).
fn looks_like_zip(path: &Path) -> Result<bool, String> {
    Ok(read_head(path, 4)?.starts_with(b"PK\x03\x04"))
}

/// Проверяет сигнатуру исполняемого файла Windows (`MZ`).
fn looks_like_exe(path: &Path) -> Result<bool, String> {
    Ok(read_head(path, 2)?.starts_with(b"MZ"))
}

/// Ищет `.exe` в распакованном архиве, заглядывая во вложенные папки.
fn find_installer(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 3 {
        return None;
    }
    let mut nested = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            nested.push(path);
            continue;
        }
        let is_exe = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if is_exe {
            return Some(path);
        }
    }
    nested.iter().find_map(|path| find_installer(path, depth + 1))
}

/// Запускает установщик Tauri NSIS в тихом режиме.
///
/// Флаги установщика: `/S` — без окон и вопросов, `/UPDATE` — обновление
/// поверх текущей версии (данные пользователя сохраняются), `/R` —
/// перезапустить приложение после установки.
fn launch_installer(installer: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new(installer);
    command.args(["/S", "/UPDATE", "/R"]);
    if let Some(parent) = installer.parent() {
        command.current_dir(parent);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("не удалось запустить установщик: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_works() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("1.0.0-beta.1", "1.0.0"));
        assert!(!is_newer("мусор", "0.1.0"));
        assert!(!is_newer("0.2.0", "мусор"));
    }

    /// Файл `Update.md` из корня проекта — ровно его публикует
    /// `publish_github.ps1` и читает программа.
    const UPDATE_FILE: &str = include_str!("../../Update.md");

    #[test]
    fn published_update_file_is_parsed() {
        let manifest = parse_manifest(UPDATE_FILE).expect("Update.md должен читаться как манифест");
        assert!(!manifest.version.is_empty(), "в манифесте нет версии");
        assert!(
            manifest.download_url.is_some()
                || manifest
                    .parts
                    .as_ref()
                    .map(|parts| !parts.is_empty())
                    .unwrap_or(false),
            "в манифесте нет ссылок на файлы обновления"
        );
    }

    #[test]
    fn manifest_is_read_from_update_block() {
        let update = concat!(
            "<!--\n  Комментарий к файлу.\n-->\n\n",
            "# Обновление приложения\n\n",
            "Описание программы обновления.\n\n",
            "<!-- CHYGUISLIDE-UPDATE -->\n",
            "```json\n",
            "{\n  \"version\": \"0.2.0\",\n  \"notes\": \"Что нового\",\n",
            "  \"downloadUrl\": \"https://example.com/app.zip\",\n  \"sha256\": \"abc\"\n}\n",
            "```\n\n",
            "Дальше обычный текст про маркер CHYGUISLIDE-UPDATE.\n",
        );
        let manifest = parse_manifest(update).expect("манифест должен читаться из Update.md");
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.notes.as_deref(), Some("Что нового"));
        assert_eq!(
            manifest.download_url.as_deref(),
            Some("https://example.com/app.zip")
        );
        assert_eq!(manifest.sha256.as_deref(), Some("abc"));
        assert!(!manifest.mandatory);
    }

    #[test]
    fn manifest_is_read_from_plain_json() {
        let manifest = parse_manifest("{\"version\":\"1.2.3\"}").expect("чистый JSON тоже подходит");
        assert_eq!(manifest.version, "1.2.3");
        assert!(manifest.download_url.is_none());
    }

    #[test]
    fn file_without_marker_has_no_manifest() {
        let readme = "# Просто README\n\n```json\n{\"version\":\"9.9.9\"}\n```\n";
        assert!(parse_manifest(readme).is_none());
    }

    #[test]
    fn installer_is_found_in_nested_archive_folder() {
        let root = std::env::temp_dir().join(format!("chyguislide-updater-{}", std::process::id()));
        let nested = root.join("вложенная папка");
        std::fs::create_dir_all(&nested).expect("каталог теста создаётся");
        let installer = nested.join("ChyguiSlide_0.2.0_x64-setup.exe");
        std::fs::write(&installer, b"MZ").expect("файл теста создаётся");

        let found = find_installer(&root, 0);
        assert_eq!(found.as_deref(), Some(installer.as_path()));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parts_are_assembled_into_one_archive() {
        let root = std::env::temp_dir().join(format!("chyguislide-parts-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("каталог теста создаётся");
        let payload: Vec<u8> = (0..5000u32).map(|value| (value % 251) as u8).collect();

        // Первая часть — как есть, вторая — в gzip: должны собраться обе.
        let plain = root.join("part1.download");
        let packed = root.join("part2.download");
        std::fs::write(&plain, &payload[..3000]).expect("часть записывается");
        write_gzip(&packed, &payload[3000..]);

        let target = root.join("update.bin");
        let hash = assemble_archive(&[plain, packed], &target).expect("части собираются");
        let assembled = std::fs::read(&target).expect("архив читается");
        assert_eq!(assembled, payload);
        assert_eq!(hash, format!("{:x}", Sha256::digest(&payload)));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn update_format_is_detected_by_content() {
        let root = std::env::temp_dir().join(format!("chyguislide-format-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("каталог теста создаётся");

        let zip = root.join("update.bin");
        std::fs::write(&zip, b"PK\x03\x04data").expect("файл записывается");
        assert!(looks_like_zip(&zip).expect("файл читается"));
        assert!(!looks_like_exe(&zip).expect("файл читается"));

        let exe = root.join("update.exe");
        std::fs::write(&exe, b"MZ\x90\x00").expect("файл записывается");
        assert!(looks_like_exe(&exe).expect("файл читается"));
        assert!(!looks_like_zip(&exe).expect("файл читается"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// Пишет gzip-файл — в таком виде публикуются части обновления, когда
    /// файл релиза разбит на несколько ссылок.
    fn write_gzip(path: &Path, data: &[u8]) {
        let file = std::fs::File::create(path).expect("gzip-часть создаётся");
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        encoder.write_all(data).expect("данные записываются");
        encoder.finish().expect("gzip закрывается");
    }
}
