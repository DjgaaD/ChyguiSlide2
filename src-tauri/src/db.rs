use rusqlite::functions::FunctionFlags;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        ",
    )?;
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS songs (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            text TEXT NOT NULL,
            number INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_songs_title ON songs(title);

        CREATE TABLE IF NOT EXISTS song_sections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
            section_type TEXT NOT NULL DEFAULT '',
            sort_order INTEGER NOT NULL DEFAULT 0,
            heading TEXT NOT NULL DEFAULT '',
            content TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_song_sections_song ON song_sections(song_id, sort_order);

        CREATE TABLE IF NOT EXISTS collections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS bible_books (
            sort_order INTEGER PRIMARY KEY,
            title TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS bible_verses (
            book TEXT NOT NULL,
            chapter INTEGER NOT NULL,
            verse INTEGER NOT NULL,
            text TEXT NOT NULL,
            PRIMARY KEY (book, chapter, verse)
        );

        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;
    migrate(&conn)?;
    register_cyrillic_lower(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    ensure_collections_schema(conn)?;
    if !column_exists(conn, "songs", "collection_id")? {
        conn.execute(
            "ALTER TABLE songs ADD COLUMN collection_id INTEGER REFERENCES collections(id)",
            [],
        )?;
    }
    if !column_exists(conn, "songs", "number")? {
        conn.execute("ALTER TABLE songs ADD COLUMN number INTEGER", [])?;
    }
    conn.execute("UPDATE songs SET number = id WHERE number IS NULL", [])?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_songs_collection_number
         ON songs(collection_id, number) WHERE collection_id IS NOT NULL AND number IS NOT NULL",
        [],
    )?;
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS playlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS playlist_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            sort_order INTEGER NOT NULL,
            kind TEXT NOT NULL,
            song_id INTEGER,
            media_path TEXT,
            media_kind TEXT,
            title TEXT
        );

        CREATE TABLE IF NOT EXISTS styles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            config_json TEXT NOT NULL DEFAULT '{}',
            is_active INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS hotkeys (
            action TEXT PRIMARY KEY,
            key TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Old/broken DBs may have `collections` without a `name` column
/// (`CREATE TABLE IF NOT EXISTS` does not upgrade schema).
fn ensure_collections_schema(conn: &Connection) -> rusqlite::Result<()> {
    let needs_rebuild = if !table_exists(conn, "collections")? {
        true
    } else {
        !column_exists(conn, "collections", "name")?
    };

    if !needs_rebuild {
        return Ok(());
    }

    // Drop FK references first if songs.collection_id already points here.
    if column_exists(conn, "songs", "collection_id")? {
        conn.execute("UPDATE songs SET collection_id = NULL", [])?;
    }
    conn.execute_batch(
        "
        PRAGMA foreign_keys = OFF;
        DROP TABLE IF EXISTS collections;
        CREATE TABLE collections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE
        );
        PRAGMA foreign_keys = ON;
        ",
    )?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn register_cyrillic_lower(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "cyrillic_lower",
        1,
        FunctionFlags::SQLITE_UTF8,
        |ctx| {
            let s: String = ctx.get(0)?;
            Ok(s.to_lowercase())
        },
    )?;
    Ok(())
}

pub fn is_empty(conn: &Connection) -> rusqlite::Result<bool> {
    let songs: i64 = conn.query_row("SELECT COUNT(*) FROM songs", [], |r| r.get(0))?;
    let verses: i64 = conn.query_row("SELECT COUNT(*) FROM bible_verses", [], |r| r.get(0))?;
    Ok(songs == 0 || verses == 0)
}

#[derive(Serialize)]
pub struct SongHit {
    pub id: i64,
    pub number: i64,
    pub title: String,
}

#[derive(Serialize)]
pub struct SongDetail {
    pub id: i64,
    pub title: String,
    pub slides: Vec<String>,
    pub collection_id: Option<i64>,
}

#[derive(Serialize)]
pub struct Collection {
    pub id: i64,
    /// Serialized as `title` for the frontend (avoids clashing with DOM `name`).
    #[serde(rename = "title")]
    pub name: String,
}

#[derive(Serialize)]
pub struct Verse {
    pub book: String,
    pub chapter: i32,
    pub verse: i32,
    pub text: String,
}

pub fn list_collections(conn: &Connection) -> rusqlite::Result<Vec<Collection>> {
    let mut stmt = conn.prepare("SELECT id, name FROM collections ORDER BY id ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok(Collection {
            id: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    rows.collect()
}

pub fn create_collection(conn: &Connection, name: &str) -> rusqlite::Result<Collection> {
    let name = name.trim();
    if name.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "collection name is empty".into(),
        ));
    }
    conn.execute("INSERT INTO collections (name) VALUES (?1)", params![name])?;
    Ok(Collection {
        id: conn.last_insert_rowid(),
        name: name.to_string(),
    })
}

pub fn rename_collection(conn: &Connection, id: i64, name: &str) -> rusqlite::Result<Collection> {
    let name = name.trim();
    if name.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "collection name is empty".into(),
        ));
    }
    let updated = conn.execute(
        "UPDATE collections SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    if updated == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(Collection {
        id,
        name: name.to_string(),
    })
}

pub fn collection_song_count(conn: &Connection, id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM songs WHERE collection_id = ?1",
        params![id],
        |row| row.get(0),
    )
}

/// `move_to`: if `delete_songs` is false, songs are moved to this collection.
pub fn delete_collection(
    conn: &Connection,
    id: i64,
    delete_songs: bool,
    move_to: Option<i64>,
) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM collections WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    if exists == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }

    if delete_songs {
        conn.execute("DELETE FROM songs WHERE collection_id = ?1", params![id])?;
    } else {
        let target = move_to.ok_or(rusqlite::Error::InvalidParameterName(
            "move target required".into(),
        ))?;
        if target == id {
            return Err(rusqlite::Error::InvalidParameterName(
                "cannot move to the same collection".into(),
            ));
        }
        let target_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM collections WHERE id = ?1",
            params![target],
            |row| row.get(0),
        )?;
        if target_exists == 0 {
            return Err(rusqlite::Error::InvalidParameterName(
                "target collection not found".into(),
            ));
        }
        conn.execute(
            "UPDATE songs SET collection_id = ?1 WHERE collection_id = ?2",
            params![target, id],
        )?;
    }

    conn.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn ensure_collection(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM collections WHERE name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute("INSERT INTO collections (name) VALUES (?1)", params![name])?;
    Ok(conn.last_insert_rowid())
}

pub fn assign_songs_without_collection(conn: &Connection, collection_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE songs SET collection_id = ?1 WHERE collection_id IS NULL",
        params![collection_id],
    )?;
    Ok(())
}

pub fn search_songs(
    conn: &Connection,
    query: &str,
    sort_by: &str,
    collection_id: Option<i64>,
) -> rusqlite::Result<Vec<SongHit>> {
    let order_sql = if sort_by == "id" {
        "COALESCE(number, id) ASC, id ASC"
    } else {
        "title COLLATE NOCASE ASC"
    };

    let query = query.to_lowercase();

    if query.trim().is_empty() {
        let (sql, params_owned): (String, Vec<i64>) = if let Some(cid) = collection_id {
            (
                format!(
                    "SELECT id, COALESCE(number, id), title FROM songs WHERE collection_id = ?1 ORDER BY {order_sql}"
                ),
                vec![cid],
            )
        } else {
            (
                format!("SELECT id, COALESCE(number, id), title FROM songs ORDER BY {order_sql}"),
                vec![],
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if params_owned.is_empty() {
            stmt.query_map([], map_hit)?.collect()
        } else {
            stmt.query_map(params![params_owned[0]], map_hit)?.collect()
        };
        return rows;
    }

    let (sql, use_collection) = if collection_id.is_some() {
        (
            format!(
                "SELECT id, COALESCE(number, id), title FROM songs
                 WHERE (
                    cyrillic_lower(title) LIKE cyrillic_lower('%' || ?1 || '%')
                    OR cyrillic_lower(text) LIKE cyrillic_lower('%' || ?1 || '%')
                    OR CAST(COALESCE(number, id) AS TEXT) LIKE '%' || ?1 || '%'
                 ) AND collection_id = ?2
                 ORDER BY {order_sql}"
            ),
            true,
        )
    } else {
        (
            format!(
                "SELECT id, COALESCE(number, id), title FROM songs
                 WHERE cyrillic_lower(title) LIKE cyrillic_lower('%' || ?1 || '%')
                    OR cyrillic_lower(text) LIKE cyrillic_lower('%' || ?1 || '%')
                    OR CAST(COALESCE(number, id) AS TEXT) LIKE '%' || ?1 || '%'
                 ORDER BY {order_sql}"
            ),
            false,
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = if use_collection {
        stmt.query_map(params![query, collection_id.unwrap()], map_hit)?
            .collect()
    } else {
        stmt.query_map(params![query], map_hit)?.collect()
    };
    rows
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BibleVerseRow {
    pub book: String,
    pub chapter: i64,
    pub verse: i64,
    pub text: String,
}

pub fn bible_books(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT title FROM bible_books ORDER BY sort_order ASC")?;
    let rows = stmt.query_map([], |row| {
        let title: String = row.get(0)?;
        // Extract short book name (first word) for matching
        let short = title.split_whitespace().next().unwrap_or(&title).to_string();
        Ok((short, title))
    })?;
    rows.collect()
}

pub fn bible_chapter_verses_by_book_chapter(
    conn: &Connection,
    book: &str,
    chapter: i64,
) -> rusqlite::Result<Vec<BibleVerseRow>> {
    let mut stmt = conn.prepare(
        "SELECT book, chapter, verse, text FROM bible_verses WHERE book = ?1 AND chapter = ?2 ORDER BY verse ASC"
    )?;
    let rows = stmt.query_map(params![book, chapter], |row| {
        Ok(BibleVerseRow {
            book: row.get(0)?,
            chapter: row.get(1)?,
            verse: row.get(2)?,
            text: row.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn get_next_bible_chapter(
    db: &Connection,
    current_book: &str,
    current_chapter: i64,
) -> rusqlite::Result<Option<(String, i64, Vec<BibleVerseRow>)>> {
    // Determine the canonical order of books.
    let book_order: HashMap<String, i64> = bible_books(db)?
        .into_iter()
        .enumerate()
        .map(|(i, (short, _))| (short, i as i64))
        .collect();

    let current_order = match book_order.get(current_book) {
        Some(o) => *o,
        None => return Ok(None),
    };

    // Get the max chapter for the current book.
    let max_chapter: i64 = db.query_row(
        "SELECT MAX(chapter) FROM bible_verses WHERE book = ?1",
        params![current_book],
        |row| row.get::<_, Option<i64>>(0),
    )?.unwrap_or(0);

    if current_chapter < max_chapter {
        // Next chapter in the same book.
        let next_chapter = current_chapter + 1;
        let verses = bible_chapter_verses_by_book_chapter(db, current_book, next_chapter)?;
        Ok(Some((current_book.to_string(), next_chapter, verses)))
    } else {
        // Last chapter of current book → move to next book.
        let next_order = current_order + 1;
        let next_book = bible_books(db)?
            .into_iter()
            .find(|(short, _)| {
                book_order.get(short).map_or(false, |o| *o == next_order)
            })
            .map(|(short, _)| short);

        if let Some(book) = next_book {
            let verses = bible_chapter_verses_by_book_chapter(db, &book, 1)?;
            Ok(Some((book, 1, verses)))
        } else {
            // Revelation (last book) — stop.
            Ok(None)
        }
    }
}

fn map_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<SongHit> {
    Ok(SongHit {
        id: row.get(0)?,
        number: row.get(1)?,
        title: row.get(2)?,
    })
}

pub fn get_song(conn: &Connection, id: i64) -> rusqlite::Result<Option<SongDetail>> {
    conn.query_row(
        "SELECT id, title, text, collection_id FROM songs WHERE id = ?1",
        params![id],
        |row| {
            let text: String = row.get(2)?;
            Ok(SongDetail {
                id: row.get(0)?,
                title: row.get(1)?,
                slides: split_slides(&text),
                collection_id: row.get(3)?,
            })
        },
    )
    .optional()
}

pub fn next_song_id(conn: &Connection) -> rusqlite::Result<i64> {
    let max: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM songs", [], |row| row.get(0))?;
    Ok(max + 1)
}

pub fn song_id_exists(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM songs WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn save_song(
    conn: &Connection,
    edit_id: Option<i64>,
    number: Option<i64>,
    title: &str,
    collection_id: i64,
    text: &str,
) -> rusqlite::Result<SongDetail> {
    let title = title.trim();
    if title.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName("title is empty".into()));
    }

    let coll_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM collections WHERE id = ?1",
        params![collection_id],
        |row| row.get(0),
    )?;
    if coll_exists == 0 {
        return Err(rusqlite::Error::InvalidParameterName(
            "collection not found".into(),
        ));
    }

    let final_id = match number {
        Some(n) if n > 0 => n,
        _ => next_song_id(conn)?,
    };

    if let Some(old_id) = edit_id {
        if final_id != old_id && song_id_exists(conn, final_id)? {
            return Err(rusqlite::Error::InvalidParameterName(
                "song number already exists".into(),
            ));
        }
        if final_id == old_id {
            conn.execute(
                "UPDATE songs SET title = ?1, text = ?2, collection_id = ?3 WHERE id = ?4",
                params![title, text, collection_id, old_id],
            )?;
        } else {
            conn.execute(
                "UPDATE songs SET id = ?1, title = ?2, text = ?3, collection_id = ?4 WHERE id = ?5",
                params![final_id, title, text, collection_id, old_id],
            )?;
        }
    } else {
        if song_id_exists(conn, final_id)? {
            return Err(rusqlite::Error::InvalidParameterName(
                "song number already exists".into(),
            ));
        }
        conn.execute(
            "INSERT INTO songs (id, title, text, collection_id) VALUES (?1, ?2, ?3, ?4)",
            params![final_id, title, text, collection_id],
        )?;
    }

    get_song(conn, final_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

#[derive(Serialize)]
pub struct LegacyImportResult {
    pub imported_songs: usize,
    pub imported_sections: usize,
    pub collection_id: i64,
}

pub fn import_legacy_chorus_json(
    conn: &mut Connection,
    raw_json: &str,
) -> Result<LegacyImportResult, String> {
    let root: Value = serde_json::from_str(raw_json)
        .map_err(|e| format!("Не удалось прочитать JSON-файл: {e}"))?;
    let object = root
        .as_object()
        .ok_or_else(|| "Корень JSON должен быть объектом.".to_string())?;
    let requested_collection_name = object
        .get("collection")
        .and_then(|value| value.as_object())
        .and_then(|collection| string_value(collection.get("name")))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Импортированные песни".to_string());
    let songs = object
        .get("songs")
        .and_then(Value::as_array)
        .ok_or_else(|| "В JSON-файле не найден массив songs.".to_string())?;

    let tx = conn.transaction().map_err(to_db_string)?;
    let collection_name = unique_collection_name(&tx, &requested_collection_name)?;
    tx.execute("INSERT INTO collections (name) VALUES (?1)", params![collection_name])
        .map_err(to_db_string)?;
    let collection_id = tx.last_insert_rowid();

    let mut imported_songs = 0;
    let mut imported_sections = 0;
    let mut generated_id = next_song_id(&tx).map_err(to_db_string)?;
    let mut used_numbers = std::collections::HashSet::new();
    for (song_index, song_value) in songs.iter().enumerate() {
        let song = song_value.as_object().ok_or_else(|| {
            format!("Песня #{} должна быть JSON-объектом.", song_index + 1)
        })?;
        while song_id_exists(&tx, generated_id).map_err(to_db_string)? {
            generated_id += 1;
        }
        let song_id = generated_id;
        generated_id += 1;
        let mut number = i64_value(song.get("number"))
            .filter(|number| *number > 0)
            .unwrap_or((song_index + 1) as i64);
        while !used_numbers.insert(number) {
            number += 1;
        }
        let title = string_value(song.get("title"))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| format!("Песня {song_id}"));
        let sections = song
            .get("sections")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let slides: Vec<String> = sections
            .iter()
            .filter_map(|section| section.as_object()?.get("content"))
            .map(content_to_text)
            .filter(|content| !content.trim().is_empty())
            .collect();
        let text = slides.join("\n\n");
        tx.execute(
            "INSERT INTO songs (id, title, text, collection_id, number) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![song_id, title, text, collection_id, number],
        )
        .map_err(to_db_string)?;
        tx.execute("DELETE FROM song_sections WHERE song_id = ?1", params![song_id])
            .map_err(to_db_string)?;
        for (section_index, section_value) in sections.iter().enumerate() {
            let section = section_value.as_object().ok_or_else(|| {
                format!("Песня '{}' содержит некорректный раздел #{}.", title, section_index + 1)
            })?;
            tx.execute(
                "INSERT INTO song_sections (song_id, section_type, sort_order, heading, content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    song_id,
                    string_value(section.get("type")).unwrap_or_default(),
                    i64_value(section.get("order")).unwrap_or(section_index as i64),
                    string_value(section.get("heading")).unwrap_or_default(),
                    section.get("content").map(content_to_text).unwrap_or_default()
                ],
            )
            .map_err(to_db_string)?;
            imported_sections += 1;
        }
        imported_songs += 1;
    }
    tx.commit().map_err(to_db_string)?;
    Ok(LegacyImportResult { imported_songs, imported_sections, collection_id })
}

fn unique_collection_name(conn: &Connection, requested: &str) -> Result<String, String> {
    let base = if requested.trim().is_empty() {
        "Импортированные песни"
    } else {
        requested.trim()
    };
    let exists = |name: &str| -> Result<bool, String> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM collections WHERE name = ?1)",
            params![name],
            |row| row.get(0),
        )
        .map_err(to_db_string)
    };
    if !exists(base)? {
        return Ok(base.to_string());
    }
    for suffix in 2..=10_000 {
        let candidate = format!("{base} ({suffix})");
        if !exists(&candidate)? {
            return Ok(candidate);
        }
    }
    Err("Не удалось создать уникальное имя сборника.".into())
}

fn string_value(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        _ => None,
    }
}

fn i64_value(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
        .or_else(|| value.as_f64().filter(|number| number.fract() == 0.0).map(|number| number as i64))
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(content_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn to_db_string<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

pub fn delete_song(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let n = conn.execute("DELETE FROM songs WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

pub fn list_bible_books(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT title FROM bible_books ORDER BY sort_order")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

pub fn search_bible(
    conn: &Connection,
    book: &str,
    chapter: i32,
    verse_from: i32,
    verse_to: Option<i32>,
) -> rusqlite::Result<Vec<Verse>> {
    let to = verse_to.unwrap_or(verse_from).max(verse_from);
    let mut stmt = conn.prepare(
        "SELECT book, chapter, verse, text FROM bible_verses
         WHERE book = ?1 AND chapter = ?2 AND verse >= ?3 AND verse <= ?4
         ORDER BY verse",
    )?;
    let rows = stmt.query_map(params![book, chapter, verse_from, to], |row| {
        Ok(Verse {
            book: row.get(0)?,
            chapter: row.get(1)?,
            verse: row.get(2)?,
            text: row.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn search_bible_query(conn: &Connection, query: &str) -> rusqlite::Result<Vec<Verse>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = query.split_whitespace().collect();
    if parts.len() >= 2 {
        let (book_end, chapter, verse) = if let Some((chapter, verse)) = parts
            .last()
            .and_then(|part| part.split_once(':'))
            .and_then(|(chapter, verse)| Some((chapter.parse::<i32>().ok()?, verse.parse::<i32>().ok()?)))
        {
            (parts.len() - 1, Some(chapter), Some(verse))
        } else if parts.len() >= 3 {
            (
                parts.len() - 2,
                parts[parts.len() - 2].parse::<i32>().ok(),
                parts[parts.len() - 1].parse::<i32>().ok(),
            )
        } else {
            (0, None, None)
        };
        if let (Some(chapter), Some(verse)) = (chapter, verse) {
            let book_query = normalize_bible_text(&parts[..book_end].join(""));
            let mut stmt = conn.prepare("SELECT book FROM bible_books ORDER BY sort_order")?;
            let books: Vec<String> = stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
            if let Some(book) = books.into_iter().find(|book| bible_book_matches(book, &book_query)) {
                let mut verse_stmt = conn.prepare(
                    "SELECT book, chapter, verse, text FROM bible_verses
                     WHERE book = ?1 AND chapter = ?2 AND verse = ?3",
                )?;
                return verse_stmt
                    .query_map(params![book, chapter, verse], |row| {
                        Ok(Verse {
                            book: row.get(0)?,
                            chapter: row.get(1)?,
                            verse: row.get(2)?,
                            text: row.get(3)?,
                        })
                    })?
                    .collect();
            }
        }
    }

    let normalized_query = normalize_bible_text(query);
    if normalized_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT book, chapter, verse, text FROM bible_verses ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Verse {
            book: row.get(0)?,
            chapter: row.get(1)?,
            verse: row.get(2)?,
            text: row.get(3)?,
        })
    })?;
    rows.filter_map(|row| match row {
        Ok(verse) if normalize_bible_text(&verse.text).contains(&normalized_query) => Some(Ok(verse)),
        Ok(_) => None,
        Err(error) => Some(Err(error)),
    })
    .collect()
}

fn normalize_bible_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn bible_book_matches(book: &str, query: &str) -> bool {
    let normalized = normalize_bible_text(book);
    let aliases = [
        ("быт", "бытие"), ("исх", "исход"), ("ин", "иоанна"),
        ("1пар", "1япаралипоменон"), ("2пар", "2япаралипоменон"),
        ("мф", "матфея"), ("мк", "марка"), ("лк", "луки"),
        ("рим", "римлянам"), ("кор", "коринфянам"),
    ];
    aliases.iter().any(|(alias, target)| query == *alias && normalized.contains(target))
        || normalized == query
        || normalized.starts_with(query)
}

pub fn bible_chapter_count(conn: &Connection, book: &str) -> rusqlite::Result<i32> {
    conn.query_row(
        "SELECT COALESCE(MAX(chapter), 0) FROM bible_verses WHERE book = ?1",
        params![book],
        |row| row.get(0),
    )
}

pub fn get_bible_chapter(
    conn: &Connection,
    book: &str,
    chapter: i32,
) -> rusqlite::Result<Vec<Verse>> {
    search_bible(conn, book, chapter, 1, Some(9999))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub id: i64,
    pub name: String,
    pub item_count: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistItemRow {
    pub kind: String,
    pub song_id: Option<i64>,
    pub media_path: Option<String>,
    pub media_kind: Option<String>,
    pub title: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub id: i64,
    pub name: String,
    pub items: Vec<PlaylistItemRow>,
}

pub fn list_playlists(conn: &Connection) -> rusqlite::Result<Vec<PlaylistSummary>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name,
                (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id)
         FROM playlists p
         ORDER BY p.created_at DESC, p.id DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PlaylistSummary {
            id: row.get(0)?,
            name: row.get(1)?,
            item_count: row.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn get_playlist(conn: &Connection, id: i64) -> rusqlite::Result<Option<PlaylistDetail>> {
    let name: Option<String> = conn
        .query_row(
            "SELECT name FROM playlists WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(name) = name else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT kind, song_id, media_path, media_kind, COALESCE(title, '')
         FROM playlist_items
         WHERE playlist_id = ?1
         ORDER BY sort_order ASC, id ASC",
    )?;
    let items = stmt
        .query_map(params![id], |row| {
            Ok(PlaylistItemRow {
                kind: row.get(0)?,
                song_id: row.get(1)?,
                media_path: row.get(2)?,
                media_kind: row.get(3)?,
                title: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(PlaylistDetail { id, name, items }))
}

pub fn create_playlist(
    conn: &Connection,
    name: &str,
    items: &[PlaylistItemRow],
) -> rusqlite::Result<PlaylistDetail> {
    let name = name.trim();
    if name.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "playlist name is empty".into(),
        ));
    }
    if items.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "playlist is empty".into(),
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO playlists (name, created_at) VALUES (?1, ?2)",
        params![name, now],
    )?;
    let id = conn.last_insert_rowid();
    for (index, item) in items.iter().enumerate() {
        conn.execute(
            "INSERT INTO playlist_items
             (playlist_id, sort_order, kind, song_id, media_path, media_kind, title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                index as i64,
                item.kind,
                item.song_id,
                item.media_path,
                item.media_kind,
                item.title
            ],
        )?;
    }
    get_playlist(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub fn delete_playlist(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let n = conn.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

// ——— Presentation styles ———

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleRow {
    pub id: i64,
    pub name: String,
    pub config_json: String,
    pub is_active: bool,
}

fn map_style_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StyleRow> {
    Ok(StyleRow {
        id: row.get(0)?,
        name: row.get(1)?,
        config_json: row.get(2)?,
        is_active: row.get(3)?,
    })
}

pub fn get_style_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<StyleRow>> {
    conn.query_row(
        "SELECT id, name, config_json, is_active FROM styles WHERE id = ?1",
        params![id],
        map_style_row,
    )
    .optional()
}

pub fn list_styles(conn: &Connection) -> rusqlite::Result<Vec<StyleRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, config_json, is_active FROM styles ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], map_style_row)?;
    rows.collect()
}

pub fn get_active_style(conn: &Connection) -> rusqlite::Result<Option<StyleRow>> {
    conn.query_row(
        "SELECT id, name, config_json, is_active FROM styles WHERE is_active = 1 ORDER BY id ASC LIMIT 1",
        [],
        map_style_row,
    )
    .optional()
}

pub fn save_style(
    conn: &Connection,
    id: Option<i64>,
    name: &str,
    config_json: &str,
) -> rusqlite::Result<StyleRow> {
    let name = name.trim();
    if name.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName("style name is empty".into()));
    }
    let config = config_json.trim();
    if config.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "style config is empty".into(),
        ));
    }
    let id = match id {
        Some(id) => {
            let updated = conn.execute(
                "UPDATE styles SET name = ?1, config_json = ?2 WHERE id = ?3",
                params![name, config, id],
            )?;
            if updated == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            id
        }
        None => {
            conn.execute(
                "INSERT INTO styles (name, config_json, is_active) VALUES (?1, ?2, 0)",
                params![name, config],
            )?;
            conn.last_insert_rowid()
        }
    };
    get_style_by_id(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub fn set_active_style(conn: &Connection, id: i64) -> rusqlite::Result<Option<StyleRow>> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM styles WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    if exists == 0 {
        return Ok(None);
    }
    conn.execute("UPDATE styles SET is_active = 0", [])?;
    conn.execute("UPDATE styles SET is_active = 1 WHERE id = ?1", params![id])?;
    get_style_by_id(conn, id)
}

pub fn delete_style(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let was_active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM styles WHERE id = ?1 AND is_active = 1",
        params![id],
        |row| row.get(0),
    )?;
    let n = conn.execute("DELETE FROM styles WHERE id = ?1", params![id])?;
    if n > 0 && was_active > 0 {
        let first: Option<i64> = conn
            .query_row("SELECT id FROM styles ORDER BY id ASC LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(first) = first {
            conn.execute("UPDATE styles SET is_active = 1 WHERE id = ?1", params![first])?;
        }
    }
    Ok(n > 0)
}

pub fn split_slides(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn join_slides(slides: &[String]) -> String {
    slides
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ——— Горячие клавиши ———

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyRow {
    pub action: String,
    pub key: String,
}

pub fn get_hotkeys(conn: &Connection) -> rusqlite::Result<Vec<HotkeyRow>> {
    let mut stmt = conn.prepare("SELECT action, key FROM hotkeys ORDER BY action ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok(HotkeyRow {
            action: row.get(0)?,
            key: row.get(1)?,
        })
    })?;
    rows.collect()
}

// ——— Настройки приложения (пропущенные версии обновления и т. п.) ———

/// Читает значение настройки; отсутствие записи — это `None`, а не ошибка.
pub fn get_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
}

/// Записывает настройку, перезаписывая прежнее значение.
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Удаляет настройку (например, забытый токен внешнего сервиса).
pub fn delete_setting(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM app_settings WHERE key = ?1", params![key])?;
    Ok(())
}

/// Полностью синхронизирует привязки: переданный список — источник истины.
pub fn save_hotkeys(conn: &Connection, bindings: &[HotkeyRow]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM hotkeys", [])?;
    for binding in bindings {
        if binding.action.trim().is_empty() {
            continue;
        }
        tx.execute(
            "INSERT INTO hotkeys (action, key) VALUES (?1, ?2)",
            params![binding.action.trim(), binding.key],
        )?;
    }
    tx.commit()?;
    Ok(())
}
