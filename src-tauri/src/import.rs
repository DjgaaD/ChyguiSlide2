use crate::db;
use rusqlite::Connection;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Deserialize)]
struct BibleFile {
    books: Vec<BibleBook>,
}

#[derive(Deserialize)]
struct BibleBook {
    title: String,
    chapters: Vec<Vec<BibleVerse>>,
}

#[derive(Deserialize)]
struct BibleVerse {
    verse: i32,
    text: String,
}

pub fn seed_if_needed(app: &AppHandle, conn: &mut Connection) -> Result<(), String> {
    if !db::is_empty(conn).map_err(to_str)? {
        return Ok(());
    }

    let bible_path = resolve_source(app, "bible.json", "Библия синодальный перевод.json")?;
    let songs_path = resolve_source(app, "songs.sps", "Песнь возрождения 3300.sps")?;

    let tx = conn.transaction().map_err(to_str)?;
    import_bible(&tx, &bible_path)?;
    import_songs(&tx, &songs_path)?;
    tx.commit().map_err(to_str)?;
    Ok(())
}

fn resolve_source(app: &AppHandle, resource_name: &str, data_name: &str) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("resources").join(resource_name));
        candidates.push(dir.join(resource_name));
    }

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    candidates.push(project_root.join("src-tauri").join("resources").join(resource_name));
    candidates.push(project_root.join("Данные").join(data_name));

    for path in candidates {
        if path.exists() {
            return Ok(path);
        }
    }

    Err(format!("source file not found: {resource_name}"))
}

fn import_bible(conn: &Connection, path: &Path) -> Result<(), String> {
    let raw = fs::read_to_string(path).map_err(to_str)?;
    let bible: BibleFile = serde_json::from_str(&raw).map_err(to_str)?;

    let mut book_stmt = conn
        .prepare("INSERT OR IGNORE INTO bible_books (sort_order, title) VALUES (?1, ?2)")
        .map_err(to_str)?;
    let mut verse_stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO bible_verses (book, chapter, verse, text) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(to_str)?;

    for (index, book) in bible.books.iter().enumerate() {
        book_stmt
            .execute(rusqlite::params![index as i32, book.title])
            .map_err(to_str)?;
        for (chapter_idx, verses) in book.chapters.iter().enumerate() {
            let chapter = (chapter_idx as i32) + 1;
            for verse in verses {
                verse_stmt
                    .execute(rusqlite::params![
                        book.title,
                        chapter,
                        verse.verse,
                        verse.text
                    ])
                    .map_err(to_str)?;
            }
        }
    }
    Ok(())
}

fn import_songs(conn: &Connection, path: &Path) -> Result<(), String> {
    let collection_id = db::ensure_collection(conn, "Песнь возрождения 3300").map_err(to_str)?;
    let raw = fs::read_to_string(path).map_err(to_str)?;
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO songs (id, title, text, collection_id) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(to_str)?;
    for (id, title, text) in parse_sps(&raw) {
        stmt.execute(rusqlite::params![id, title, text, collection_id])
            .map_err(to_str)?;
    }
    Ok(())
}

/// Existing DBs: create default collection and attach orphan songs.
pub fn ensure_default_collection(conn: &Connection) -> Result<(), String> {
    let songs: i64 = conn
        .query_row("SELECT COUNT(*) FROM songs", [], |r| r.get(0))
        .map_err(to_str)?;
    if songs == 0 {
        return Ok(());
    }
    let collection_id = db::ensure_collection(conn, "Песнь возрождения 3300").map_err(to_str)?;
    db::assign_songs_without_collection(conn, collection_id).map_err(to_str)?;
    Ok(())
}

/// SPS records: id#$#title#$#number#$#key#$#author#$#composer#$#lyrics
/// Lyrics use `@%` for line breaks and `@$` for slide/section breaks.
pub fn parse_sps(raw: &str) -> Vec<(i64, String, String)> {
    let tokens: Vec<&str> = raw.split("#$#").collect();
    let mut songs = Vec::new();
    let mut i = 0;

    while i + 6 < tokens.len() {
        let Some(id) = parse_id_token(tokens[i]) else {
            i += 1;
            continue;
        };
        let title = tokens[i + 1].trim().to_string();
        let lyrics = tokens[i + 6];
        let text = normalize_lyrics(lyrics);
        if !title.is_empty() && !text.is_empty() {
            songs.push((id, title, text));
        }
        i += 7;
        while i < tokens.len() && parse_id_token(tokens[i]).is_none() {
            i += 1;
        }
    }

    songs
}

fn parse_id_token(token: &str) -> Option<i64> {
    token
        .lines()
        .rev()
        .find_map(|line| line.trim().parse::<i64>().ok())
}

fn normalize_lyrics(raw: &str) -> String {
    raw.split("@$")
        .map(|block| {
            block
                .split("@%")
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn to_str<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_sps;

    #[test]
    fn parses_two_songs() {
        let raw = "1431#$#First title#$#33#$#key#$##$##$#Куплет 1 @%line a @%line b@$Припев@%chorus#$#\n831#$#Second#$#16#$#key#$#a#$#b#$#Куплет 1@%hello\n";
        let songs = parse_sps(raw);
        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0].0, 1431);
        assert_eq!(songs[0].1, "First title");
        assert!(songs[0].2.contains("line a"));
        assert_eq!(songs[1].0, 831);
    }
}
