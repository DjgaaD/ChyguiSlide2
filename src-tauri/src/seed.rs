//! Первичная установка рабочей базы из эталонной.
//!
//! Приложение поставляется с уже готовой базой `resources/template.sqlite`
//! (Синодальный перевод и «Песнь возрождения 3300»), поэтому разбор `songs.sps`
//! и `bible.json` при запуске не выполняется: на новом компьютере рабочий файл
//! базы появляется копированием эталона в каталог данных приложения.
//!
//! Вместе с базой новый компьютер получает готовые стили оформления и наши обои
//! (см. `ensure_default_styles`).

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::{commands, db, logger};

/// Имя рабочего файла базы в каталоге данных приложения.
const DATABASE_FILE: &str = "chyguislide.sqlite";
/// Имя эталонной базы в ресурсах приложения.
const TEMPLATE_FILE: &str = "template.sqlite";
/// Служебные файлы SQLite: лежат рядом с базой под её полным именем.
const SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// Возвращает путь к рабочей базе, создавая её копией эталона при первом запуске.
pub fn ensure_database(app: &AppHandle) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Не удалось определить каталог данных: {error}"))?;
    let db_path = data_dir.join(DATABASE_FILE);

    if is_ready(&db_path) {
        logger::info("db", &format!("Файл базы данных: {}", db_path.display()));
        return Ok(db_path);
    }

    let template = resolve_template(app)?;
    logger::info(
        "db",
        &format!(
            "первый запуск: копирую эталонную базу {} → {}",
            template.display(),
            db_path.display()
        ),
    );
    install(&template, &db_path)?;
    logger::info("db", &format!("Файл базы данных: {}", db_path.display()));
    Ok(db_path)
}

// ——— Готовые стили и обои ———

/// Флаг в `app_settings`: готовые стили этому файлу базы уже поставлены.
const STYLES_SEEDED_KEY: &str = "styles_seeded";

/// Стиль, который включается активным при первой установке.
///
/// Показ по умолчанию выглядит как раньше — белый текст на чёрном фоне; светлый
/// и анимированный варианты пользователь выбирает сам в настройках.
const DEFAULT_ACTIVE_STYLE: &str = "На чёрном фоне";

/// Ставит готовые стили и поддерживает в них пути к поставляемым обоям.
///
/// Эталонная база приходит без стилей, поэтому три варианта — «На белом фоне»,
/// «На чёрном фоне» и «Анимированный фон» с нашими обоями — добавляются здесь,
/// при первом запуске рабочей базы. Флаг в `app_settings` не даёт поставить их
/// повторно, если пользователь удалил стили сам.
pub fn ensure_default_styles(conn: &Connection, app: &AppHandle) -> rusqlite::Result<()> {
    let wallpapers = commands::shipped_wallpaper_files(app);
    if wallpapers.is_empty() {
        logger::warn(
            "style",
            "поставляемые обои не найдены — анимированный фон останется без обоев",
        );
    }
    let installed = install_default_styles_once(conn, &wallpapers)?;
    if installed > 0 {
        logger::info("style", &format!("поставлено готовых стилей: {installed}"));
    }
    let repaired = repair_shipped_wallpaper_paths(conn, &wallpapers)?;
    if repaired > 0 {
        logger::info("style", &format!("обновлены пути к обоям в стилях: {repaired}"));
    }
    Ok(())
}

/// Ставит готовые стили, если этому файлу базы их ещё не ставили.
///
/// Готовые стили нужны только пустой базе: на обновлении существующей установки
/// свои стили пользователя уже есть, и добавлять к ним наши не нужно. Флаг в
/// `app_settings` при этом всё равно ставим — чтобы удалённые пользователем
/// стили не вернулись при следующем запуске.
fn install_default_styles_once(
    conn: &Connection,
    wallpapers: &[(String, PathBuf)],
) -> rusqlite::Result<usize> {
    let seeded = db::get_setting(conn, STYLES_SEEDED_KEY)?.is_some();
    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM styles", [], |row| row.get(0))?;
    if seeded || existing > 0 {
        if !seeded {
            db::set_setting(conn, STYLES_SEEDED_KEY, "1")?;
        }
        return Ok(0);
    }
    let styles = default_styles(wallpapers);
    let tx = conn.unchecked_transaction()?;
    for (name, config) in &styles {
        tx.execute(
            "INSERT INTO styles (name, config_json, is_active) VALUES (?1, ?2, ?3)",
            params![
                name,
                config.to_string(),
                i64::from(*name == DEFAULT_ACTIVE_STYLE)
            ],
        )?;
    }
    db::set_setting(&tx, STYLES_SEEDED_KEY, "1")?;
    tx.commit()?;
    Ok(styles.len())
}

/// Готовые стили: имя и конфигурация (`StyleConfig` из `src/shared/style.ts`).
fn default_styles(wallpapers: &[(String, PathBuf)]) -> Vec<(&'static str, Value)> {
    let media: Vec<String> = wallpapers
        .iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect();
    let selected = media.first().cloned();
    vec![
        ("На белом фоне", solid_style("#1a2230", "#f4f6fa")),
        ("На чёрном фоне", solid_style("#ffffff", "#000000")),
        ("Анимированный фон", animated_style(media, selected)),
    ]
}

/// Однотонный стиль: варианты отличаются только цветом текста и фона.
fn solid_style(text_color: &str, background_color: &str) -> Value {
    json!({
        "textColor": text_color,
        "fontFamily": "Segoe UI",
        "bold": true,
        "align": "center",
        "strokeWidth": 0,
        "strokeColor": "#000000",
        "strokeOpacity": 0.65,
        "transitionType": "fade",
        "transitionMs": 280,
        "backgroundMode": "color",
        "backgroundColor": background_color,
        "mediaPaths": [],
        "selectedMediaPath": null,
        "bibleCaptionEnabled": true,
        "bibleCaptionPosition": "above",
    })
}

/// Стиль с анимированным фоном: наши обои и случайный выбор на каждый показ.
///
/// Обводка нужна для читаемости белого текста поверх светлых обоев («Небо»,
/// «Поле»). Если обои не нашлись, стиль остаётся однотонным — так он хотя бы
/// работает.
fn animated_style(media: Vec<String>, selected: Option<String>) -> Value {
    let has_media = !media.is_empty();
    json!({
        "textColor": "#ffffff",
        "fontFamily": "Segoe UI",
        "bold": true,
        "align": "center",
        "strokeWidth": if has_media { 4 } else { 0 },
        "strokeColor": "#000000",
        "strokeOpacity": 0.65,
        "transitionType": "fade",
        "transitionMs": 280,
        "backgroundMode": if has_media { "random" } else { "color" },
        "backgroundColor": "#000000",
        "mediaPaths": media,
        "selectedMediaPath": selected,
        "bibleCaptionEnabled": true,
        "bibleCaptionPosition": "above",
    })
}

/// Обновляет в стилях ссылки на поставляемые обои.
///
/// Путь к ресурсам приложения зависит от машины: перенос базы на другой
/// компьютер (резервная копия, второй ПК) оставляет в стилях старые пути. Ссылки
/// на поставляемые файлы переписываем на актуальные; файлы пользователя не
/// трогаем — сверяем только имена наших обоев.
fn repair_shipped_wallpaper_paths(
    conn: &Connection,
    wallpapers: &[(String, PathBuf)],
) -> rusqlite::Result<usize> {
    if wallpapers.is_empty() {
        return Ok(0);
    }
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, config_json FROM styles")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut repaired = 0;
    for (id, config_json) in rows {
        // Битый конфиг не наш: его починит интерфейс при сохранении стиля.
        let Ok(mut config) = serde_json::from_str::<Value>(&config_json) else {
            continue;
        };
        let changed = repair_media_list(&mut config, wallpapers)
            | repair_selected_media(&mut config, wallpapers);
        if !changed {
            continue;
        }
        conn.execute(
            "UPDATE styles SET config_json = ?1 WHERE id = ?2",
            params![config.to_string(), id],
        )?;
        repaired += 1;
    }
    Ok(repaired)
}

/// Переписывает мёртвые пути в `mediaPaths`; `true` — если что-то изменилось.
fn repair_media_list(config: &mut Value, wallpapers: &[(String, PathBuf)]) -> bool {
    let Some(paths) = config.get_mut("mediaPaths").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for entry in paths.iter_mut() {
        let Some(current) = entry.as_str().map(str::to_string) else {
            continue;
        };
        if let Some(resolved) = resolve_dead_wallpaper(&current, wallpapers) {
            *entry = Value::String(resolved);
            changed = true;
        }
    }
    changed
}

/// То же для выбранного файла `selectedMediaPath`.
fn repair_selected_media(config: &mut Value, wallpapers: &[(String, PathBuf)]) -> bool {
    let Some(current) = config
        .get("selectedMediaPath")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return false;
    };
    let Some(resolved) = resolve_dead_wallpaper(&current, wallpapers) else {
        return false;
    };
    config["selectedMediaPath"] = Value::String(resolved);
    true
}

/// Путь, которого больше нет на диске, но чьё имя совпадает с поставляемыми
/// обоями → актуальный путь. Иначе `None` (файл жив или он не наш).
fn resolve_dead_wallpaper(path: &str, wallpapers: &[(String, PathBuf)]) -> Option<String> {
    if Path::new(path).is_file() {
        return None;
    }
    let file_name = Path::new(path).file_name()?.to_str()?;
    wallpapers
        .iter()
        .find(|(name, _)| name == file_name)
        .map(|(_, resolved)| resolved.to_string_lossy().into_owned())
}

/// Ищет эталонную базу в ресурсах приложения.
///
/// Сборка кладёт ресурсы в подкаталог `resources` каталога ресурсов; при запуске
/// из исходников файл может лежать ещё и в каталоге проекта, поэтому кандидатов
/// несколько — как в поиске боковых бинарников (`commands.rs::sidecar_path`).
fn resolve_template(app: &AppHandle) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("resources").join(TEMPLATE_FILE));
        candidates.push(dir.join(TEMPLATE_FILE));
    }
    if let Ok(dir) = std::env::current_dir() {
        candidates.push(dir.join("src-tauri").join("resources").join(TEMPLATE_FILE));
        candidates.push(dir.join("resources").join(TEMPLATE_FILE));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| format!("Не найдена эталонная база {TEMPLATE_FILE} в ресурсах приложения."))
}

/// Ставит копию эталона на место рабочей базы.
///
/// Копия появляется под временным именем и только потом переименовывается:
/// прерванный первый запуск не оставит под рабочим именем половину базы.
fn install(template: &Path, db_path: &Path) -> Result<(), String> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!("Не удалось создать каталог {}: {error}", parent.display())
        })?;
    }

    let staged = with_suffix(db_path, ".seed");
    remove_if_exists(&staged)?;
    for suffix in SIDECARS {
        // Журнал прежней базы не должен смешаться со свежей копией.
        remove_if_exists(&with_suffix(db_path, suffix))?;
    }

    copy_writable(template, &staged)?;
    for suffix in SIDECARS {
        let journal = with_suffix(template, suffix);
        if journal.is_file() {
            copy_writable(&journal, &with_suffix(&staged, suffix))?;
        }
    }

    std::fs::rename(&staged, db_path).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        format!("Не удалось установить базу {}: {error}", db_path.display())
    })?;
    // Переименование не трогает служебные файлы: SQLite ищет журнал рядом с базой.
    for suffix in SIDECARS {
        let journal = with_suffix(&staged, suffix);
        if journal.is_file() {
            std::fs::rename(&journal, &with_suffix(db_path, suffix))
                .map_err(|error| format!("Не удалось перенести журнал базы: {error}"))?;
        }
    }

    if let Err(error) = verify(db_path) {
        // Испорченную копию убираем: следующий запуск скопирует эталон заново.
        let _ = std::fs::remove_file(db_path);
        return Err(error);
    }
    Ok(())
}

/// Проверяет, что копия открывается на запись и содержит данные: иначе пустая
/// или битая база обнаружилась бы только в интерфейсе.
fn verify(path: &Path) -> Result<(), String> {
    let connection = Connection::open(path)
        .map_err(|error| format!("Скопированная база не открывается: {error}"))?;
    let count = |table: &str| -> Result<i64, String> {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .map_err(|error| format!("В базе нет таблицы {table}: {error}"))
    };
    let songs = count("songs")?;
    let verses = count("bible_verses")?;
    logger::info("db", &format!("в базе {songs} песен и {verses} стихов"));
    if songs == 0 || verses == 0 {
        return Err("Эталонная база пуста: нет песен или стихов.".into());
    }
    Ok(())
}

/// Готовой считается существующая непустая база: пустой файл — след неудачной
/// попытки копирования, его нужно заменить эталоном.
fn is_ready(path: &Path) -> bool {
    std::fs::metadata(path).map(|meta| meta.len() > 0).unwrap_or(false)
}

/// Копирует файл и разрешает запись в копию.
fn copy_writable(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::copy(source, destination).map_err(|error| {
        format!(
            "Не удалось скопировать {} в {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    make_writable(destination)
}

/// Снимает признак «только для чтения»: `fs::copy` переносит права источника,
/// а ресурсы приложения доступны только для чтения.
fn make_writable(path: &Path) -> Result<(), String> {
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| format!("Нет доступа к {}: {error}", path.display()))?
        .permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).map_err(|error| {
            format!("Не удалось разрешить запись в {}: {error}", path.display())
        })?;
    }
    Ok(())
}

/// Добавляет суффикс к полному имени файла: так SQLite называет журналы базы
/// (`chyguislide.sqlite-wal`), так же называется и временная копия.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Удаляет файл, если он есть.
fn remove_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Не удалось удалить {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Отдельный каталог на каждый тест: параллельные тесты не мешают друг другу.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chyguislide-seed-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Упрощённая эталонная база: те же таблицы, что проверяет `verify`.
    fn write_template(path: &Path, with_rows: bool) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE songs (id INTEGER PRIMARY KEY, title TEXT NOT NULL);
                 CREATE TABLE bible_verses (
                     book TEXT NOT NULL,
                     chapter INTEGER NOT NULL,
                     verse INTEGER NOT NULL,
                     text TEXT NOT NULL,
                     PRIMARY KEY (book, chapter, verse)
                 );",
            )
            .unwrap();
        if with_rows {
            connection
                .execute_batch(
                    "INSERT INTO songs (id, title) VALUES (1, 'Песня');
                     INSERT INTO bible_verses (book, chapter, verse, text)
                         VALUES ('Бытие', 1, 1, 'В начале сотворил Бог небо и землю.');",
                )
                .unwrap();
        }
    }

    /// База со схемой стилей и настроек — как в рабочей базе (`db::open`).
    fn write_styles_schema(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE styles (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     name TEXT NOT NULL,
                     config_json TEXT NOT NULL DEFAULT '{}',
                     is_active INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE app_settings (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );",
            )
            .unwrap();
    }

    /// Обои для тестов: файлы могут не существовать — важен только набор имён.
    fn test_wallpapers() -> Vec<(String, PathBuf)> {
        vec![
            (
                "Небо.mp4".to_string(),
                PathBuf::from("C:/app/resources/wallpapers/Небо.mp4"),
            ),
            (
                "Поле.mp4".to_string(),
                PathBuf::from("C:/app/resources/wallpapers/Поле.mp4"),
            ),
        ]
    }

    /// Стили базы: имя, конфигурация и признак активности — по порядку id.
    fn styles_of(conn: &Connection) -> Vec<(String, Value, i64)> {
        let mut stmt = conn
            .prepare("SELECT name, config_json, is_active FROM styles ORDER BY id ASC")
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let config: String = row.get(1)?;
                Ok((name, serde_json::from_str(&config).unwrap(), row.get(2)?))
            })
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn installs_template_as_working_database() {
        let dir = temp_dir("install");
        let template = dir.join(TEMPLATE_FILE);
        write_template(&template, true);
        let db_path = dir.join("data").join(DATABASE_FILE);

        install(&template, &db_path).unwrap();

        let connection = Connection::open(&db_path).unwrap();
        let songs: i64 = connection
            .query_row("SELECT COUNT(*) FROM songs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(songs, 1);
        drop(connection);
        // Временная копия не остаётся рядом с рабочей базой.
        assert!(!with_suffix(&db_path, ".seed").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_of_read_only_template_is_writable() {
        let dir = temp_dir("read-only");
        let template = dir.join(TEMPLATE_FILE);
        write_template(&template, true);
        let mut permissions = std::fs::metadata(&template).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&template, permissions).unwrap();

        let db_path = dir.join("data").join(DATABASE_FILE);
        install(&template, &db_path).unwrap();

        assert!(!std::fs::metadata(&db_path).unwrap().permissions().readonly());
        // Запись должна быть возможна: рядом SQLite держит журнал.
        std::fs::OpenOptions::new().append(true).open(&db_path).unwrap();

        // Уборка: файл только для чтения мешает удалению каталога.
        let mut permissions = std::fs::metadata(&template).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&template, permissions).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_empty_template() {
        let dir = temp_dir("empty");
        let template = dir.join(TEMPLATE_FILE);
        write_template(&template, false);
        let db_path = dir.join(DATABASE_FILE);

        assert!(install(&template, &db_path).is_err());
        // Плохая копия не остаётся: следующий запуск повторит попытку.
        assert!(!db_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Пустой файл от прерванного копирования не мешает установке базы.
    #[test]
    fn replaces_empty_file_left_by_failed_copy() {
        let dir = temp_dir("empty-file");
        let template = dir.join(TEMPLATE_FILE);
        write_template(&template, true);
        let db_path = dir.join(DATABASE_FILE);
        std::fs::write(&db_path, b"").unwrap();

        install(&template, &db_path).unwrap();

        assert!(is_ready(&db_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ready_only_for_existing_non_empty_file() {
        let dir = temp_dir("ready");
        let db_path = dir.join(DATABASE_FILE);

        assert!(!is_ready(&db_path));
        std::fs::write(&db_path, b"").unwrap();
        assert!(!is_ready(&db_path));
        std::fs::write(&db_path, b"sqlite").unwrap();
        assert!(is_ready(&db_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_names_follow_working_database() {
        let db_path = Path::new("chyguislide.sqlite");
        assert_eq!(
            with_suffix(db_path, "-wal"),
            PathBuf::from("chyguislide.sqlite-wal")
        );
        assert_eq!(
            with_suffix(db_path, ".seed"),
            PathBuf::from("chyguislide.sqlite.seed")
        );
    }

    /// Настоящий эталон из `resources`: проверяем, что сборка поставляет готовую
    /// базу (3300 песен и 31227 стихов), а не только её схему.
    #[test]
    fn real_template_from_resources_is_installed() {
        let template = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(TEMPLATE_FILE);
        if !template.is_file() {
            // Эталон не подготовлен (сборка без ресурсов) — проверять нечего.
            return;
        }
        let dir = temp_dir("real-template");
        let db_path = dir.join("data").join(DATABASE_FILE);

        install(&template, &db_path).unwrap();

        let connection = Connection::open(&db_path).unwrap();
        let count = |table: &str| -> i64 {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count("songs"), 3300);
        assert_eq!(count("bible_verses"), 31227);
        drop(connection);

        // Копия доступна для записи: иначе первый запуск закончился бы ошибкой.
        assert!(!std::fs::metadata(&db_path).unwrap().permissions().readonly());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Новый компьютер: в базе нет стилей — должны появиться три готовых.
    #[test]
    fn installs_three_ready_styles_on_empty_database() {
        let dir = temp_dir("styles");
        let db_path = dir.join(DATABASE_FILE);
        write_styles_schema(&db_path);
        let conn = Connection::open(&db_path).unwrap();

        let installed = install_default_styles_once(&conn, &test_wallpapers()).unwrap();

        assert_eq!(installed, 3);
        let styles = styles_of(&conn);
        let names: Vec<&str> = styles.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["На белом фоне", "На чёрном фоне", "Анимированный фон"]
        );
        // Активен ровно один стиль — чёрный: показ по умолчанию как раньше.
        let active: Vec<&str> = styles
            .iter()
            .filter(|(_, _, is_active)| *is_active == 1)
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert_eq!(active, vec!["На чёрном фоне"]);
        // Однотонные варианты отличаются цветом фона и текста.
        assert_eq!(styles[0].1["backgroundMode"], "color");
        assert_eq!(styles[0].1["backgroundColor"], "#f4f6fa");
        assert_eq!(styles[0].1["textColor"], "#1a2230");
        assert_eq!(styles[1].1["backgroundColor"], "#000000");
        assert_eq!(styles[1].1["textColor"], "#ffffff");
        // В анимированном фоне уже лежат наши обои, режим — случайный выбор.
        let animated = &styles[2].1;
        assert_eq!(animated["backgroundMode"], "random");
        assert_eq!(animated["mediaPaths"].as_array().unwrap().len(), 2);
        assert_eq!(
            animated["selectedMediaPath"].as_str().unwrap(),
            "C:/app/resources/wallpapers/Небо.mp4"
        );
        assert!(animated["strokeWidth"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Удалённые пользователем стили не возвращаются: флаг стоит в настройках.
    #[test]
    fn ready_styles_are_installed_only_once() {
        let dir = temp_dir("styles-once");
        let db_path = dir.join(DATABASE_FILE);
        write_styles_schema(&db_path);
        let conn = Connection::open(&db_path).unwrap();
        assert_eq!(install_default_styles_once(&conn, &test_wallpapers()).unwrap(), 3);

        conn.execute("DELETE FROM styles", []).unwrap();

        assert_eq!(install_default_styles_once(&conn, &test_wallpapers()).unwrap(), 0);
        assert!(styles_of(&conn).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Без обоев в ресурсах стиль всё равно ставится — однотонным.
    #[test]
    fn animated_style_without_wallpapers_stays_solid() {
        let dir = temp_dir("styles-no-wallpapers");
        let db_path = dir.join(DATABASE_FILE);
        write_styles_schema(&db_path);
        let conn = Connection::open(&db_path).unwrap();

        assert_eq!(install_default_styles_once(&conn, &[]).unwrap(), 3);

        let styles = styles_of(&conn);
        assert_eq!(styles[2].1["backgroundMode"], "color");
        assert!(styles[2].1["mediaPaths"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Обновление существующей установки: свои стили не разбавляются готовыми.
    #[test]
    fn keeps_existing_styles_without_adding_ready_ones() {
        let dir = temp_dir("styles-existing");
        let db_path = dir.join(DATABASE_FILE);
        write_styles_schema(&db_path);
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO styles (name, config_json, is_active) VALUES ('Свой стиль', '{}', 1)",
            [],
        )
        .unwrap();

        assert_eq!(install_default_styles_once(&conn, &test_wallpapers()).unwrap(), 0);

        let styles = styles_of(&conn);
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0].0, "Свой стиль");
        // Флаг поставлен: удалённые стили больше не возвращаются.
        assert!(db::get_setting(&conn, STYLES_SEEDED_KEY).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// База со старого компьютера: путь к нашим обоям переписывается на текущий,
    /// файлы пользователя не трогаются.
    #[test]
    fn repairs_paths_to_shipped_wallpapers() {
        let dir = temp_dir("styles-repair");
        let db_path = dir.join(DATABASE_FILE);
        write_styles_schema(&db_path);
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO styles (name, config_json, is_active) VALUES ('Анимированный фон', ?1, 1)",
            params![json!({
                "backgroundMode": "random",
                "mediaPaths": [
                    "D:/old/resources/wallpapers/Небо.mp4",
                    "C:/user/моё видео.mp4",
                ],
                "selectedMediaPath": "D:/old/resources/wallpapers/Небо.mp4",
            })
            .to_string()],
        )
        .unwrap();

        let repaired = repair_shipped_wallpaper_paths(&conn, &test_wallpapers()).unwrap();

        assert_eq!(repaired, 1);
        let config = &styles_of(&conn)[0].1;
        assert_eq!(config["mediaPaths"][0], "C:/app/resources/wallpapers/Небо.mp4");
        assert_eq!(config["mediaPaths"][1], "C:/user/моё видео.mp4");
        assert_eq!(
            config["selectedMediaPath"],
            "C:/app/resources/wallpapers/Небо.mp4"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Живые файлы и чужие пути остаются как есть.
    #[test]
    fn keeps_live_and_user_paths_untouched() {
        let dir = temp_dir("styles-live");
        let db_path = dir.join(DATABASE_FILE);
        write_styles_schema(&db_path);
        let conn = Connection::open(&db_path).unwrap();
        let clip = dir.join("Моё видео.mp4");
        std::fs::write(&clip, b"video").unwrap();
        let clip_path = clip.to_string_lossy().into_owned();
        conn.execute(
            "INSERT INTO styles (name, config_json, is_active) VALUES ('Своё', ?1, 1)",
            params![json!({
                "backgroundMode": "media",
                "mediaPaths": [clip_path.clone(), "C:/user/нет такого.mp4"],
                "selectedMediaPath": clip_path.clone(),
            })
            .to_string()],
        )
        .unwrap();

        let repaired = repair_shipped_wallpaper_paths(&conn, &test_wallpapers()).unwrap();

        assert_eq!(repaired, 0);
        let config = &styles_of(&conn)[0].1;
        assert_eq!(config["mediaPaths"][0].as_str().unwrap(), clip_path);
        assert_eq!(config["mediaPaths"][1], "C:/user/нет такого.mp4");
        assert_eq!(config["selectedMediaPath"].as_str().unwrap(), clip_path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Настоящие ресурсы: эталонная база и наши обои → три готовых стиля.
    ///
    /// Это сценарий нового компьютера: база ставится из эталона (стилей в нём
    /// нет), а пути к обоям берутся из `resources/wallpapers`.
    #[test]
    fn real_template_gets_ready_styles_with_bundled_wallpapers() {
        let template = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(TEMPLATE_FILE);
        if !template.is_file() {
            return; // сборка без ресурсов — проверять нечего
        }
        let dir = temp_dir("styles-real");
        let db_path = dir.join(DATABASE_FILE);
        install(&template, &db_path).unwrap();
        let conn = db::open(&db_path).unwrap();

        // Те же пять обоев, что ищет `commands::shipped_wallpaper_files`.
        let wallpapers: Vec<(String, PathBuf)> = commands::WALLPAPER_NAMES
            .iter()
            .filter_map(|name| commands::wallpaper_file_name(name))
            .map(|file| {
                (
                    file.to_string(),
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("resources")
                        .join("wallpapers")
                        .join(file),
                )
            })
            .filter(|(_, path)| path.is_file())
            .collect();
        assert_eq!(wallpapers.len(), 5, "в ресурсах лежат все пять обоев");

        assert_eq!(install_default_styles_once(&conn, &wallpapers).unwrap(), 3);

        let styles = styles_of(&conn);
        assert_eq!(styles.len(), 3);
        let animated = &styles[2].1;
        assert_eq!(animated["backgroundMode"], "random");
        let media = animated["mediaPaths"].as_array().unwrap();
        assert_eq!(media.len(), 5);
        for path in media {
            let path = path.as_str().unwrap();
            assert!(Path::new(path).is_file(), "обои на месте: {path}");
        }
        // Активный стиль читается так же, как его читает интерфейс.
        let active = db::get_active_style(&conn).unwrap().unwrap();
        assert_eq!(active.name, DEFAULT_ACTIVE_STYLE);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

