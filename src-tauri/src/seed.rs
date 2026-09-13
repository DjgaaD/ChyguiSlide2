//! Первичная установка рабочей базы из эталонной.
//!
//! Приложение поставляется с уже готовой базой `resources/template.sqlite`
//! (Синодальный перевод и «Песнь возрождения 3300»), поэтому разбор `songs.sps`
//! и `bible.json` при запуске не выполняется: на новом компьютере рабочий файл
//! базы появляется копированием эталона в каталог данных приложения.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::logger;

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
}

