//! Резервное копирование данных приложения на Яндекс.Диск.
//!
//! Порядок работы команды `yandex_backup`:
//! 1. база сбрасывает журнал WAL на диск (`PRAGMA wal_checkpoint`) — копия
//!    снимается с целостного файла;
//! 2. файлы каталога данных складываются в ZIP-архив во временном каталоге;
//! 3. у REST API Яндекс.Диска запрашивается ссылка на загрузку
//!    (`GET /v1/disk/resources/upload`), затем архив отправляется по этой
//!    ссылке методом PUT;
//! 4. выполняется ротация: в папке остаётся не больше заданного в настройках
//!    числа копий, самые старые переносятся в корзину Диска.
//!
//! Авторизация — OAuth приложения: команда `open_yandex_token_page` открывает
//! страницу Яндекса, где токен показан прямо в ответе (`response_type=token`),
//! и его остаётся вставить в поле настроек. Токен и Client ID хранятся в таблице
//! настроек `app_settings` (та же база, что и у остальных данных). В журнал токен
//! не пишется, а в интерфейсе показывается прямо в поле «OAuth-токен»: так его
//! можно поправить или удалить вместе с остальными настройками одной кнопкой.
//!
//! Все сетевые операции идут через `run_blocking`: `reqwest::blocking` создаёт
//! собственный tokio-runtime, который нельзя ронять в асинхронном рантайме
//! Tauri (см. комментарий в `updater.rs`). Разбор ответов и ротация сделаны на
//! стандартной библиотеке — новых зависимостей модуль не требует.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::db;
use crate::logger;
use crate::opener::open_url;
use crate::updater::run_blocking;
use crate::AppState;

/// Базовый адрес REST API Яндекс.Диска.
const API_BASE: &str = "https://cloud-api.yandex.net/v1/disk";
/// Страница авторизации OAuth, куда открывается браузер.
const OAUTH_AUTHORIZE: &str = "https://oauth.yandex.ru/authorize";
/// Адрес возврата, который Яндекс принимает всегда — даже если он не прописан в
/// приложении. На этой странице Яндекс показывает токен: так он и получается.
const OAUTH_VERIFICATION_URI: &str = "https://oauth.yandex.ru/verification_code";
/// Ключ настройки в базе: OAuth-токен Яндекс.Диска.
const TOKEN_KEY: &str = "yandex.disk.token";
/// Ключ настройки: Client ID приложения в Яндексе.
const CLIENT_ID_KEY: &str = "yandex.oauth.client_id";
/// Ключ настройки: имя папки с копиями на Диске.
const FOLDER_KEY: &str = "yandex.disk.folder";
/// Ключ настройки: сколько копий хранить.
const KEEP_KEY: &str = "yandex.disk.keep";
/// Подкаталог с архивами внутри папки приложения на Диске.
const BACKUPS_SUBDIR: &str = "backups";
/// Папка с копиями по умолчанию.
const DEFAULT_FOLDER: &str = "ChyguiSlide";
/// Сколько копий хранить по умолчанию.
const DEFAULT_KEEP: usize = 10;
/// Верхняя граница лимита копий: защита от случайного «99999» в поле ввода.
const KEEP_MAX: usize = 200;
/// Максимальная длина имени папки на Диске.
const FOLDER_MAX: usize = 64;
/// Запасная страница «Полигон»: открывается, когда Client ID ещё не указан.
const TOKEN_PAGE: &str = "https://yandex.ru/dev/disk/poligon/";
/// Веб-интерфейс Яндекс.Диска: из этого адреса собирается ссылка на папку копий.
const WEB_DISK: &str = "https://disk.yandex.ru/client/disk";
/// Таймаут служебных запросов (проверка токена, получение ссылки на загрузку).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Таймаут загрузки архива: копия может быть крупной, а канал медленным.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(900);

/// Настройки и состояние Яндекс.Диска для вкладки «Резервные копии».
///
/// Токен отдаётся в интерфейс целиком: поле «OAuth-токен» всегда показывает
/// сохранённое значение, поэтому токен можно править, заменять и удалять прямо
/// в нём. В журнал токен по-прежнему не пишется.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YandexSettings {
    pub configured: bool,
    /// Сохранённый токен (пустая строка, если его нет).
    pub token: String,
    pub client_id: String,
    pub folder: String,
    pub keep_copies: usize,
    /// Каталог копий на Диске: «app:/<папка>/backups».
    pub cloud_dir: String,
}

/// Учётная запись Яндекс.Диска, которой принадлежит токен.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YandexAccount {
    pub login: String,
    pub total_space: i64,
    pub used_space: i64,
}

/// Итог резервного копирования — для уведомления в интерфейсе.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YandexBackupResult {
    pub file_name: String,
    /// Путь в схеме API (`app:/…`) — им пользуются запросы к Диску.
    pub cloud_path: String,
    /// Каталог на Диске, куда легла копия.
    pub cloud_dir: String,
    /// Тот же путь, как его отдаёт Яндекс (`disk:/Приложения/…`): копии лежат в
    /// папке приложения, поэтому `app:/` пользователю ничего не говорит.
    pub display_path: String,
    /// Ссылка на папку копий в веб-интерфейсе Диска (пусто, если её не удалось собрать).
    pub web_url: String,
    pub size_bytes: u64,
    /// Локальные дата и время создания копии.
    pub uploaded_at: String,
    /// Сколько старых копий убрано ротацией.
    pub removed_count: usize,
    pub removed: Vec<String>,
}

/// Настройки модуля, прочитанные из базы.
struct YandexConfig {
    token: Option<String>,
    client_id: String,
    folder: String,
    keep: usize,
}

impl YandexConfig {
    /// Каталог копий на Диске. Архивы лежат в подпапке `backups`, поэтому
    /// пользователь настраивает только имя папки приложения.
    fn cloud_dir(&self) -> String {
        format!("app:/{}/{BACKUPS_SUBDIR}", self.folder)
    }
}

#[derive(Deserialize)]
struct UploadLink {
    href: String,
}

#[derive(Deserialize)]
struct DiskInfo {
    total_space: i64,
    used_space: i64,
    user: DiskUser,
}

#[derive(Deserialize)]
struct DiskUser {
    login: String,
}

/// Элемент списка файлов в папке на Диске.
#[derive(Deserialize)]
struct CloudItem {
    name: String,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    modified: Option<String>,
    /// «file» или «dir».
    #[serde(rename = "type", default)]
    resource_type: String,
}

impl CloudItem {
    /// Момент создания файла в секундах UTC; если поля нет — берём изменение.
    fn created_at(&self) -> i64 {
        self.created
            .as_deref()
            .and_then(parse_yandex_time)
            .or_else(|| self.modified.as_deref().and_then(parse_yandex_time))
            .unwrap_or_default()
    }
}

/// Ответ `GET /resources` для папки: список вложенных объектов.
#[derive(Deserialize)]
struct ResourceList {
    #[serde(rename = "_embedded", default)]
    embedded: Option<ResourceListItems>,
}

#[derive(Deserialize)]
struct ResourceListItems {
    #[serde(default)]
    items: Vec<CloudItem>,
}

/// Ответ `GET /resources` с одним атрибутом — реальным путём ресурса.
#[derive(Deserialize)]
struct ResourcePath {
    path: String,
}

/// Тело ошибки REST API Яндекс.Диска.
#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Настройки и состояние авторизации Яндекс.Диска для вкладки «Резервные копии».
#[tauri::command]
pub fn yandex_settings(state: State<AppState>) -> Result<YandexSettings, String> {
    load_config(&state).map(|config| settings_view(&config))
}

/// Сохраняет настройки Яндекс.Диска вместе с токеном.
///
/// Токен приходит из того же поля формы, что и остальные настройки: пустая
/// строка означает «удалить сохранённый токен» (раньше это делала отдельная
/// кнопка «Забыть токен»), а `None` — «поле не трогать».
#[tauri::command]
pub fn save_yandex_settings(
    client_id: String,
    folder: String,
    keep_copies: i64,
    token: Option<String>,
    state: State<AppState>,
) -> Result<YandexSettings, String> {
    let client_id = client_id.trim().to_string();
    let folder = sanitize_folder(&folder)?;
    let keep = validate_keep(keep_copies)?;
    let token = token.as_deref().map(str::trim);
    {
        let db = state.db.lock().map_err(|error| error.to_string())?;
        db::set_setting(&db, CLIENT_ID_KEY, &client_id).map_err(|error| error.to_string())?;
        db::set_setting(&db, FOLDER_KEY, &folder).map_err(|error| error.to_string())?;
        db::set_setting(&db, KEEP_KEY, &keep.to_string()).map_err(|error| error.to_string())?;
        match token {
            Some("") => db::delete_setting(&db, TOKEN_KEY).map_err(|error| error.to_string())?,
            Some(value) => {
                db::set_setting(&db, TOKEN_KEY, value).map_err(|error| error.to_string())?
            }
            None => {}
        }
    }
    logger::info(
        "yandex",
        &format!("настройки Яндекс.Диска сохранены: папка «{folder}», копий {keep}"),
    );
    load_config(&state).map(|config| settings_view(&config))
}

/// Проверяет токен: обращается к профилю диска и возвращает логин и объём.
///
/// Если токен не передан, берётся сохранённый — так кнопка «Проверить»
/// работает и с уже настроенным приложением, и с только что вставленным токеном.
#[tauri::command(async)]
pub fn check_yandex_token(
    token: Option<String>,
    state: State<AppState>,
) -> Result<YandexAccount, String> {
    let token = match token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => resolve_token(&state)?,
    };
    run_blocking(move || check_account(&token))?
}

/// Резервная копия базы и данных приложения на Яндекс.Диск.
///
/// `async` — как у обновления приложения: архивация, загрузка и ротация идут в
/// отдельном потоке, поэтому интерфейс не подвисает на время отправки копии.
#[tauri::command(async)]
pub fn yandex_backup(app: AppHandle, state: State<AppState>) -> Result<YandexBackupResult, String> {
    let config = load_config(&state)?;
    let token = config
        .token
        .clone()
        .ok_or_else(|| "Токен Яндекс.Диска не сохранён — выполните авторизацию.".to_string())?;
    let cloud_dir = config.cloud_dir();
    let keep = config.keep;
    let db_path = state.db_path.clone();
    // Копия снимается с целостного файла: WAL сбрасывается на диск до архивации.
    {
        let db = state.db.lock().map_err(|error| error.to_string())?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|error| format!("Не удалось подготовить базу к копированию: {error}"))?;
    }
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Не удалось определить каталог данных: {error}"))?;
    let staging = staging_dir(&app)?;

    logger::info(
        "yandex",
        &format!("начато резервное копирование на Яндекс.Диск: {cloud_dir}"),
    );
    let result_dir = cloud_dir.clone();
    run_blocking(move || {
        let stamp = logger::local_file_stamp();
        let file_name = format!("chyguislide-backup-{stamp}.zip");
        let archive = staging.join(&file_name);
        let size_bytes = build_archive(&data_dir, &db_path, &archive)?;
        let cloud_path = format!("{cloud_dir}/{file_name}");
        // Временный архив удаляем в любом случае — и при успехе, и при ошибке.
        let uploaded = upload_archive(&token, &archive, &cloud_dir, &cloud_path, size_bytes);
        let _ = fs::remove_file(&archive);
        uploaded?;
        logger::info(
            "yandex",
            &format!("резервная копия загружена: {cloud_path} ({size_bytes} байт)"),
        );
        // Копия лежит в папке приложения (системный каталог «Приложения»), поэтому
        // в интерфейс отдаём путь в схеме `disk:/` и ссылку на эту папку.
        let (display_path, web_url) = disk_location(&token, &cloud_path);
        logger::info("yandex", &format!("копия на Диске: {display_path}"));
        // Ротация не должна выглядеть провалом копирования: архив уже на диске,
        // поэтому сбой удаления старых файлов только пишем в журнал.
        let removed = rotate_backups(&token, &cloud_dir, keep).unwrap_or_else(|error| {
            logger::error(
                "yandex",
                &format!("не удалось удалить старые копии: {error}"),
            );
            Vec::new()
        });
        Ok(YandexBackupResult {
            file_name,
            cloud_path,
            cloud_dir: result_dir,
            display_path,
            web_url,
            size_bytes,
            uploaded_at: stamp.replace('_', " "),
            removed_count: removed.len(),
            removed,
        })
    })?
}

/// Открывает в браузере папку с копиями на Яндекс.Диске.
///
/// Копии лежат в папке приложения — в системном каталоге «Приложения», которого
/// нет в списке «Все файлы». Адрес папки берётся из метаданных ресурса: Яндекс
/// отдаёт реальный путь в схеме `disk:/`. Пока копий не было, папки ещё нет —
/// тогда открывается корень Диска.
#[tauri::command(async)]
pub fn open_yandex_backups_folder(state: State<AppState>) -> Result<(), String> {
    let cloud_dir = load_config(&state)?.cloud_dir();
    let token = resolve_token(&state)?;
    // Сбой определения пути не должен мешать: открываем корень Диска и пишем журнал.
    let link = run_blocking(move || match http_client(REQUEST_TIMEOUT) {
        Ok(client) => match resolve_disk_path(&client, &token, &cloud_dir) {
            Ok(path) => web_disk_link(&path).unwrap_or_else(|| WEB_DISK.to_string()),
            Err(error) => {
                logger::warn(
                    "yandex",
                    &format!("папка копий на Диске недоступна ({cloud_dir}): {error}"),
                );
                WEB_DISK.to_string()
            }
        },
        Err(error) => {
            logger::warn(
                "yandex",
                &format!("не удалось создать HTTP-клиент для Диска: {error}"),
            );
            WEB_DISK.to_string()
        }
    })?;
    logger::info("yandex", &format!("открытие папки копий на Диске: {link}"));
    open_url(&link)
}

/// Открывает в браузере страницу, на которой Яндекс показывает OAuth-токен.
///
/// Когда Client ID уже сохранён, открывается страница авторизации самого
/// приложения в неявном режиме (`response_type=token`): Яндекс показывает токен
/// прямо на странице, и его остаётся вставить в поле «OAuth-токен Яндекс.Диска».
/// Адрес возврата служебный (`verification_code`), поэтому способ работает у
/// любого приложения: ни Callback URL в настройках приложения, ни Client secret
/// не нужны. Без Client ID открывается общая страница «Полигон».
#[tauri::command]
pub fn open_yandex_token_page(state: State<AppState>) -> Result<(), String> {
    let client_id = load_config(&state)?.client_id;
    let url = if client_id.is_empty() {
        TOKEN_PAGE.to_string()
    } else {
        token_url(&client_id)
    };
    logger::info("yandex", &format!("открытие страницы токена: {url}"));
    open_url(&url)
}

// ——— Настройки ———

/// Настройки модуля из базы.
///
/// Некорректные значения в базе (например, имя папки из одних пробелов) вкладку
/// настроек не ломают: вместо них подставляются значения по умолчанию.
fn load_config(state: &State<AppState>) -> Result<YandexConfig, String> {
    let db = state.db.lock().map_err(|error| error.to_string())?;
    let read = |key: &str| -> Result<Option<String>, String> {
        db::get_setting(&db, key).map_err(|error| error.to_string())
    };
    Ok(YandexConfig {
        token: read(TOKEN_KEY)?.and_then(non_empty),
        client_id: read(CLIENT_ID_KEY)?.unwrap_or_default().trim().to_string(),
        folder: read(FOLDER_KEY)?
            .as_deref()
            .and_then(|value| sanitize_folder(value).ok())
            .unwrap_or_else(|| DEFAULT_FOLDER.to_string()),
        keep: read(KEEP_KEY)?
            .as_deref()
            .and_then(|value| parse_keep(value).ok())
            .unwrap_or(DEFAULT_KEEP),
    })
}

/// Значение без пробелов по краям (`None`, если после обрезки пусто).
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Настройки для интерфейса: поле «OAuth-токен» заполняется сохранённым токеном.
fn settings_view(config: &YandexConfig) -> YandexSettings {
    YandexSettings {
        configured: config.token.is_some(),
        token: config.token.clone().unwrap_or_default(),
        client_id: config.client_id.clone(),
        folder: config.folder.clone(),
        keep_copies: config.keep,
        cloud_dir: config.cloud_dir(),
    }
}

/// Приводит имя папки на Диске к безопасному виду.
///
/// «/» в пути — разделитель, часть символов в именах недопустима, поэтому они
/// заменяются подчёркиванием; пробелы и точки по краям убираются.
fn sanitize_folder(raw: &str) -> Result<String, String> {
    let replaced: String = raw
        .trim()
        .chars()
        .map(|symbol| match symbol {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            symbol if symbol.is_control() => '_',
            symbol => symbol,
        })
        .collect();
    let cleaned: String = replaced
        .trim_matches(|symbol| symbol == ' ' || symbol == '.')
        .chars()
        .take(FOLDER_MAX)
        .collect();
    if cleaned.is_empty() {
        return Err("Укажите название папки на Яндекс.Диске.".into());
    }
    Ok(cleaned)
}

/// Лимит копий из настроек. `0` — ротация выключена.
fn parse_keep(raw: &str) -> Result<usize, String> {
    let value: i64 = raw
        .trim()
        .parse()
        .map_err(|_| "Количество копий должно быть числом.".to_string())?;
    validate_keep(value)
}

/// Проверка лимита копий, введённого в интерфейсе.
fn validate_keep(value: i64) -> Result<usize, String> {
    if !(0..=KEEP_MAX as i64).contains(&value) {
        return Err(format!(
            "Количество копий должно быть от 0 до {KEEP_MAX} (0 — не удалять старые)."
        ));
    }
    Ok(value as usize)
}

/// Токен для запросов: сохранённый в настройках.
fn resolve_token(state: &State<AppState>) -> Result<String, String> {
    load_config(state)?.token.ok_or_else(|| {
        "Токен Яндекс.Диска не сохранён — нажмите «Получить токен» в настройках.".to_string()
    })
}

/// HTTP-клиент с общим User-Agent и таймаутом (как в модуле обновления).
fn http_client(timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .timeout(timeout)
        .user_agent(concat!("ChyguiSlide/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Не удалось создать HTTP-клиент: {error}"))
}

/// Каталог для временного архива перед отправкой.
fn staging_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("Не удалось определить каталог кэша: {error}"))?
        .join("backups");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Не удалось создать {}: {error}", dir.display()))?;
    Ok(dir)
}

/// Читает профиль диска — так проверяется, что токен рабочий.
fn check_account(token: &str) -> Result<YandexAccount, String> {
    let client = http_client(REQUEST_TIMEOUT)?;
    let response = client
        .get(format!("{API_BASE}/"))
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        .send()
        .map_err(|error| format!("Нет связи с Яндекс.Диском: {error}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let info: DiskInfo = serde_json::from_str(&body)
        .map_err(|error| format!("Неожиданный ответ Яндекс.Диска: {error}"))?;
    logger::info(
        "yandex",
        &format!(
            "токен принят: диск {} ({} из {} байт занято)",
            info.user.login, info.used_space, info.total_space
        ),
    );
    Ok(YandexAccount {
        login: info.user.login,
        total_space: info.total_space,
        used_space: info.used_space,
    })
}

// ——— Расположение копий на Диске ———

/// Путь ресурса так, как его видит пользователь: `disk:/Приложения/…`.
///
/// Запрос к `app:/…` Яндекс выполняет в папке приложения — в системном каталоге
/// «Приложения», которого нет в списке «Все файлы». В ответе API абсолютный путь
/// приходит в схеме `disk:/`, поэтому его и показываем.
fn resolve_disk_path(client: &Client, token: &str, cloud_path: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&format!("{API_BASE}/resources"))
        .map_err(|error| format!("Некорректный адрес API Яндекс.Диска: {error}"))?;
    url.query_pairs_mut()
        .append_pair("path", cloud_path)
        .append_pair("fields", "path");
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        .send()
        .map_err(|error| format!("Нет связи с Яндекс.Диском: {error}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let resource: ResourcePath = serde_json::from_str(&body)
        .map_err(|error| format!("Неожиданный ответ Яндекс.Диска: {error}"))?;
    Ok(resource.path)
}

/// Ссылка на папку в веб-интерфейсе Диска из пути в схеме `disk:/`.
///
/// Сегменты кодируются: системный каталог приложений назван по языку аккаунта
/// («Приложения», «Apps»), а в адресе такие имена должны быть percent-кодированы.
fn web_disk_link(disk_path: &str) -> Option<String> {
    let relative = disk_path.strip_prefix("disk:/")?;
    if relative.is_empty() {
        return None;
    }
    let encoded: Vec<String> = relative.split('/').map(percent_encode).collect();
    Some(format!("{WEB_DISK}/{}", encoded.join("/")))
}

/// Путь копии для интерфейса и ссылка на неё в веб-интерфейсе Диска.
///
/// Метаданные запрашиваются уже после загрузки, поэтому сбой этого запроса не
/// должен выглядеть провалом копирования: показываем исходный `app:/…` без ссылки.
fn disk_location(token: &str, cloud_path: &str) -> (String, String) {
    let resolved = http_client(REQUEST_TIMEOUT)
        .and_then(|client| resolve_disk_path(&client, token, cloud_path));
    match resolved {
        Ok(path) => {
            let link = web_disk_link(&path).unwrap_or_default();
            (path, link)
        }
        Err(error) => {
            logger::warn(
                "yandex",
                &format!("не удалось определить путь копии на Диске: {error}"),
            );
            (cloud_path.to_string(), String::new())
        }
    }
}

// ——— Ссылка для получения токена ———

/// Ссылка, по которой Яндекс показывает токен прямо на странице.
///
/// Это неявный режим (`response_type=token`): Redirect URI берётся служебный —
/// `https://oauth.yandex.ru/verification_code`. Его Яндекс принимает у любого
/// приложения, поэтому ни Callback URL в настройках приложения, ни Client secret
/// не нужны: токен выдаётся сразу, обменивать код не требуется.
fn token_url(client_id: &str) -> String {
    format!(
        "{OAUTH_AUTHORIZE}?response_type=token&client_id={}&redirect_uri={}",
        percent_encode(client_id),
        percent_encode(OAUTH_VERIFICATION_URI)
    )
}

// ——— Кодирование параметров ———

/// Percent-кодирование для параметров URL.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Складывает файлы каталога данных в ZIP-архив и возвращает его размер.
///
/// В копию попадает база данных и остальные файлы каталога данных;
/// подкаталоги (журналы, кэш), служебные `-wal`/`-shm` и сам архив
/// пропускаются — после `wal_checkpoint` служебные файлы пусты и для
/// восстановления не нужны.
fn build_archive(data_dir: &Path, db_path: &Path, archive: &Path) -> Result<u64, String> {
    let file = File::create(archive)
        .map_err(|error| format!("Не удалось создать архив {}: {error}", archive.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut sources: Vec<PathBuf> = vec![db_path.to_path_buf()];
    if let Ok(entries) = fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() || path.as_path() == db_path || path.as_path() == archive {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if name.ends_with("-wal") || name.ends_with("-shm") {
                continue;
            }
            sources.push(path);
        }
    }

    for path in sources {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let mut source = File::open(&path)
            .map_err(|error| format!("Не удалось открыть {}: {error}", path.display()))?;
        zip.start_file(name, options)
            .map_err(|error| format!("Не удалось добавить {name} в архив: {error}"))?;
        std::io::copy(&mut source, &mut zip)
            .map_err(|error| format!("Не удалось записать {name} в архив: {error}"))?;
    }

    let mut file = zip
        .finish()
        .map_err(|error| format!("Не удалось завершить архив: {error}"))?;
    file.flush()
        .map_err(|error| format!("Не удалось сохранить архив: {error}"))?;
    fs::metadata(archive)
        .map(|meta| meta.len())
        .map_err(|error| format!("Не удалось прочитать размер архива: {error}"))
}

/// Загружает архив на Яндекс.Диск: сначала ссылка на загрузку, затем PUT.
fn upload_archive(
    token: &str,
    archive: &Path,
    cloud_dir: &str,
    cloud_path: &str,
    size_bytes: u64,
) -> Result<(), String> {
    let client = http_client(UPLOAD_TIMEOUT)?;
    ensure_cloud_dir(&client, token, cloud_dir)?;
    let href = request_upload_url(&client, token, cloud_path)?;

    let file = File::open(archive)
        .map_err(|error| format!("Не удалось открыть архив для отправки: {error}"))?;
    let response = client
        .put(&href)
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        // `Body::sized` отдаёт точный Content-Length — без chunked-кодирования.
        .body(reqwest::blocking::Body::sized(file, size_bytes))
        .send()
        .map_err(|error| format!("Не удалось отправить архив: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(api_error(status, &body));
    }
    Ok(())
}

/// Создаёт каталог для копий на Диске вместе с родительскими папками.
///
/// Имя папки задаёт пользователь, поэтому путь может быть любой глубины:
/// Яндекс.Диск не создаёт промежуточные папки сам, делаем это по шагам.
fn ensure_cloud_dir(client: &Client, token: &str, cloud_dir: &str) -> Result<(), String> {
    let mut current = String::new();
    for segment in cloud_dir.split('/') {
        if segment.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(segment);
        // «app:» — корень папки приложения, а не создаваемый каталог.
        if segment.ends_with(':') {
            continue;
        }
        create_dir(client, token, &current)?;
    }
    Ok(())
}

/// Создаёт папку на Диске: «уже существует» — не ошибка.
fn create_dir(client: &Client, token: &str, path: &str) -> Result<(), String> {
    let mut url = reqwest::Url::parse(&format!("{API_BASE}/resources"))
        .map_err(|error| format!("Некорректный адрес API Яндекс.Диска: {error}"))?;
    url.query_pairs_mut().append_pair("path", path);
    let response = client
        .put(url)
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        .send()
        .map_err(|error| format!("Нет связи с Яндекс.Диском: {error}"))?;
    let status = response.status();
    // 201 — папка создана, 409 — уже существует: оба варианта означают «есть».
    if status.is_success() || status == reqwest::StatusCode::CONFLICT {
        return Ok(());
    }
    let body = response.text().unwrap_or_default();
    Err(api_error(status, &body))
}

/// Список файлов в папке копий (`GET /resources`); подпапки пропускаются.
fn list_backups(client: &Client, token: &str, cloud_dir: &str) -> Result<Vec<CloudItem>, String> {
    let mut url = reqwest::Url::parse(&format!("{API_BASE}/resources"))
        .map_err(|error| format!("Некорректный адрес API Яндекс.Диска: {error}"))?;
    url.query_pairs_mut()
        .append_pair("path", cloud_dir)
        .append_pair("limit", "1000");
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        .send()
        .map_err(|error| format!("Нет связи с Яндекс.Диском: {error}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let listing: ResourceList = serde_json::from_str(&body)
        .map_err(|error| format!("Неожиданный ответ Яндекс.Диска: {error}"))?;
    Ok(listing
        .embedded
        .map(|embedded| embedded.items)
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item.resource_type != "dir")
        .collect())
}

/// Удаляет файл с Диска. Яндекс переносит его в корзину, поэтому случайно
/// убранную ротацией копию можно вернуть вручную.
fn delete_resource(client: &Client, token: &str, cloud_path: &str) -> Result<(), String> {
    let mut url = reqwest::Url::parse(&format!("{API_BASE}/resources"))
        .map_err(|error| format!("Некорректный адрес API Яндекс.Диска: {error}"))?;
    url.query_pairs_mut().append_pair("path", cloud_path);
    let response = client
        .delete(url)
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        .send()
        .map_err(|error| format!("Нет связи с Яндекс.Диском: {error}"))?;
    let status = response.status();
    // 204 — удалено, 404 — файла уже нет: для ротации это один и тот же итог.
    if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }
    let body = response.text().unwrap_or_default();
    Err(api_error(status, &body))
}

// ——— Ротация копий ———

/// Ротация: в папке остаются `keep` самых свежих файлов, остальные уходят в
/// корзину Диска. `keep == 0` — ротация выключена, список даже не запрашивается.
fn rotate_backups(token: &str, cloud_dir: &str, keep: usize) -> Result<Vec<String>, String> {
    if keep == 0 {
        return Ok(Vec::new());
    }
    let client = http_client(REQUEST_TIMEOUT)?;
    let items = list_backups(&client, token, cloud_dir)?;
    let names = backups_to_remove(items, keep);
    for name in &names {
        let path = format!("{cloud_dir}/{name}");
        delete_resource(&client, token, &path)?;
        logger::info("yandex", &format!("старая копия убрана в корзину: {path}"));
    }
    Ok(names)
}

/// Имена копий, которые нужно удалить: всё, что старше `keep` самых свежих.
///
/// Копии, созданные в одну секунду, различаются по имени — в него входит та же
/// отметка времени, поэтому имя годится запасным ключом сортировки.
fn backups_to_remove(mut items: Vec<CloudItem>, keep: usize) -> Vec<String> {
    if keep == 0 || items.len() <= keep {
        return Vec::new();
    }
    items.sort_by(|left, right| {
        right
            .created_at()
            .cmp(&left.created_at())
            .then_with(|| right.name.cmp(&left.name))
    });
    items.into_iter().skip(keep).map(|item| item.name).collect()
}

/// Время из ответа Яндекс.Диска (`2026-09-12T14:33:05+03:00`) → секунды UTC.
///
/// Своя разборка вместо отдельной зависимости для дат: нужен только порядок
/// копий, а формат фиксирован — ISO-8601 с числовым смещением или `Z`.
fn parse_yandex_time(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = digits(&bytes[0..4])?;
    let month = digits(&bytes[5..7])?;
    let day = digits(&bytes[8..10])?;
    let hour = digits(&bytes[11..13])?;
    let minute = digits(&bytes[14..16])?;
    let second = digits(&bytes[17..19])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut index = 19;
    // Доли секунды на порядок копий не влияют — просто пропускаем их.
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while matches!(bytes.get(index), Some(byte) if byte.is_ascii_digit()) {
            index += 1;
        }
    }
    let offset = match bytes.get(index) {
        None | Some(b'Z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours = digits(bytes.get(index + 1..index + 3)?)?;
            let minutes = bytes
                .get(index + 4..index + 6)
                .and_then(digits)
                .unwrap_or(0);
            let sign = if *sign == b'-' { -1 } else { 1 };
            sign * (hours * 3_600 + minutes * 60)
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

/// Число из последовательности ASCII-цифр.
fn digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0i64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

/// (год, месяц, день) → дни от 1970-01-01. Алгоритм Говарда Хиннанта.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Ссылка на загрузку файла (`href`) из REST API Яндекс.Диска.
fn request_upload_url(client: &Client, token: &str, cloud_path: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&format!("{API_BASE}/resources/upload"))
        .map_err(|error| format!("Некорректный адрес API Яндекс.Диска: {error}"))?;
    url.query_pairs_mut()
        .append_pair("path", cloud_path)
        .append_pair("overwrite", "true");
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"))
        .send()
        .map_err(|error| format!("Нет связи с Яндекс.Диском: {error}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let link: UploadLink = serde_json::from_str(&body)
        .map_err(|error| format!("Неожиданный ответ Яндекс.Диска: {error}"))?;
    Ok(link.href)
}

/// Понятное человеку объяснение ошибки REST API Яндекс.Диска.
fn api_error(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<ApiError>(body)
        .ok()
        .and_then(|error| error.description.or(error.message).or(error.error))
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status.as_u16() {
        401 => format!(
            "Яндекс.Диск не принял токен (401): проверьте токен и повторите попытку. {detail}"
        ),
        403 => format!("Доступ к Яндекс.Диску запрещён (403): проверьте права токена. {detail}"),
        404 => format!("Объект не найден на Яндекс.Диске (404). {detail}"),
        409 => format!("Конфликт на Яндекс.Диске (409): повторите попытку позже. {detail}"),
        413 => format!("Файл слишком большой для Яндекс.Диска (413). {detail}"),
        507 => format!("На Яндекс.Диске закончилось место (507). {detail}"),
        _ => format!("Яндекс.Диск вернул ошибку {} ({detail})", status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chyguislide-yandex-{tag}-{}", std::process::id()))
    }

    /// В архив попадают база и остальные файлы каталога данных, но не служебные
    /// `-wal`/`-shm` и не подкаталоги (журналы).
    #[test]
    fn archive_keeps_database_and_skips_service_files() {
        let dir = temp_dir("archive");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("logs")).expect("каталог данных создаётся");
        let db_path = dir.join("chyguislide.sqlite");
        fs::write(&db_path, b"SQLite format 3\0").expect("база записывается");
        fs::write(dir.join("chyguislide.sqlite-wal"), b"wal").expect("wal записывается");
        fs::write(dir.join("chyguislide.sqlite-shm"), b"shm").expect("shm записывается");
        fs::write(dir.join("extra.dat"), b"extra").expect("файл записывается");
        fs::write(dir.join("logs").join("session.log"), b"log").expect("журнал записывается");
        let archive = dir.join("backup.zip");

        let size = build_archive(&dir, &db_path, &archive).expect("архив собирается");
        assert!(size > 0, "архив должен быть непустым");

        let file = File::open(&archive).expect("архив открывается");
        let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).expect("архив читается");
        let mut names: Vec<String> = (0..zip.len())
            .map(|index| {
                zip.by_index(index)
                    .expect("файл архива читается")
                    .name()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["chyguislide.sqlite", "extra.dat"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Путь, который Яндекс отдаёт в схеме `disk:/`, превращается в адрес
    /// веб-интерфейса: сегменты кодируются, потому что системный каталог
    /// «Приложения» назван по языку аккаунта.
    #[test]
    fn disk_path_becomes_web_link() {
        assert_eq!(
            web_disk_link("disk:/Приложения/ChyguiSlide/backups").as_deref(),
            Some(
                "https://disk.yandex.ru/client/disk/%D0%9F%D1%80%D0%B8%D0%BB%D0%BE%D0%B6%D0%B5%D0%BD%D0%B8%D1%8F/ChyguiSlide/backups"
            )
        );
    }

    /// Ссылка строится только из абсолютного пути: `app:/…` веб-интерфейсу не годится.
    #[test]
    fn web_link_needs_absolute_disk_path() {
        assert_eq!(web_disk_link("app:/ChyguiSlide/backups"), None);
        assert_eq!(web_disk_link("disk:/"), None);
    }

    /// Токен уходит в интерфейс целиком: поле «OAuth-токен» всегда заполнено, а
    /// остальные значения формы берутся из настроек.
    #[test]
    fn settings_view_gives_the_token_to_the_form() {
        let config = YandexConfig {
            token: Some("y0_AgAAAA1234wxyz".into()),
            client_id: "8f1c2d3e".into(),
            folder: "ChyguiSlide".into(),
            keep: 5,
        };
        let view = settings_view(&config);
        assert!(view.configured);
        assert_eq!(view.token, "y0_AgAAAA1234wxyz");
        assert_eq!(view.client_id, "8f1c2d3e");
        assert_eq!(view.keep_copies, 5);
        assert_eq!(view.cloud_dir, "app:/ChyguiSlide/backups");
    }

    /// Без сохранённого токена поле пустое — форма показывает, что авторизации нет.
    #[test]
    fn settings_view_without_token_leaves_the_field_empty() {
        let config = YandexConfig {
            token: None,
            client_id: String::new(),
            folder: "ChyguiSlide".into(),
            keep: DEFAULT_KEEP,
        };
        let view = settings_view(&config);
        assert!(!view.configured);
        assert!(view.token.is_empty());
    }

    /// Ошибка авторизации превращается в понятную подсказку с деталями ответа.
    #[test]
    fn unauthorized_error_explains_the_token() {
        let body = concat!(
            r#"{"message":"Unauthorized","description":"Токен недействителен","#,
            r#""error":"UnauthorizedError"}"#
        );
        let message = api_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(
            message.contains("токен"),
            "в тексте должна быть подсказка про токен: {message}"
        );
        assert!(
            message.contains("Токен недействителен"),
            "детали ответа теряются: {message}"
        );
    }

    /// Элемент списка файлов с датой создания — для проверок ротации.
    fn cloud_item(name: &str, created: &str) -> CloudItem {
        CloudItem {
            name: name.to_string(),
            created: Some(created.to_string()),
            modified: None,
            resource_type: "file".to_string(),
        }
    }

    /// Время из ответа Яндекс.Диска переводится в секунды UTC со смещением.
    #[test]
    fn yandex_time_is_parsed_with_offset() {
        let local = days_from_civil(2026, 9, 12) * 86_400 + 14 * 3_600 + 33 * 60 + 5;
        assert_eq!(
            parse_yandex_time("2026-09-12T14:33:05+03:00"),
            Some(local - 3 * 3_600)
        );
        assert_eq!(parse_yandex_time("2026-09-12T14:33:05Z"), Some(local));
        assert_eq!(
            parse_yandex_time("2026-09-12T14:33:05.123+00:00"),
            Some(local)
        );
        assert_eq!(parse_yandex_time("12.09.2026"), None);
    }

    /// Ротация оставляет самые свежие копии, а старые отдаёт на удаление.
    #[test]
    fn rotation_keeps_the_newest_copies() {
        let items = vec![
            cloud_item("b.zip", "2026-09-02T10:00:00+03:00"),
            cloud_item("d.zip", "2026-09-04T10:00:00+03:00"),
            cloud_item("a.zip", "2026-09-01T10:00:00+03:00"),
            cloud_item("c.zip", "2026-09-03T10:00:00+03:00"),
        ];
        assert_eq!(backups_to_remove(items, 2), vec!["b.zip", "a.zip"]);
        let only = || vec![cloud_item("only.zip", "2026-09-01T10:00:00Z")];
        assert!(backups_to_remove(only(), 3).is_empty());
        // `0` — ротация выключена, старые копии не трогаем.
        assert!(backups_to_remove(only(), 0).is_empty());
    }

    /// Имя папки чистится от разделителей и запрещённых символов.
    #[test]
    fn folder_name_is_sanitised() {
        assert_eq!(
            sanitize_folder("  Мои/копии  ").expect("имя допустимо"),
            "Мои_копии"
        );
        assert_eq!(
            sanitize_folder("ChyguiSlide").expect("имя допустимо"),
            "ChyguiSlide"
        );
        assert!(sanitize_folder("   ").is_err());
        assert!(sanitize_folder("...").is_err());
    }

    /// Лимит копий: 0 — без ротации, больше `KEEP_MAX` — ошибка.
    #[test]
    fn keep_limit_is_validated() {
        assert_eq!(validate_keep(0).expect("0 допустим"), 0);
        assert_eq!(validate_keep(10).expect("10 допустим"), 10);
        assert!(validate_keep(-1).is_err());
        assert!(validate_keep(KEEP_MAX as i64 + 1).is_err());
        assert_eq!(parse_keep(" 7 ").expect("число из настроек"), 7);
        assert!(parse_keep("много").is_err());
    }

    /// Получение токена не зависит от настроек приложения: адрес возврата служебный.
    #[test]
    fn token_url_uses_verification_code_redirect() {
        let url = token_url("client-id");
        assert!(url.starts_with(OAUTH_AUTHORIZE));
        assert!(url.contains("response_type=token"));
        assert!(url.contains("client_id=client-id"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Foauth.yandex.ru%2Fverification_code"));
        // Адрес локального сервера в этой ссылке не участвует.
        assert!(!url.contains("localhost"));
        // Параметры разделены `&`: ссылку нельзя резать по этому символу
        // (см. `open_url` в `opener.rs` — она уходит в браузер целиком).
        assert!(url.contains("&client_id=client-id&redirect_uri="));
    }

    /// Кодирование параметров: кириллица, пробел и «+» экранируются.
    #[test]
    fn percent_coding_escapes_reserved_characters() {
        assert_eq!(
            percent_encode("Мои копии+1"),
            "%D0%9C%D0%BE%D0%B8%20%D0%BA%D0%BE%D0%BF%D0%B8%D0%B8%2B1"
        );
        // Незарезервированные символы остаются как есть — так их и ждёт Яндекс.
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(
            percent_encode(OAUTH_VERIFICATION_URI),
            "https%3A%2F%2Foauth.yandex.ru%2Fverification_code"
        );
    }
}
