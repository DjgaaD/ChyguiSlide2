use crate::db::{self, Collection, HotkeyRow, PlaylistDetail, PlaylistItemRow, PlaylistSummary, SongDetail, SongHit, StyleRow, Verse};
use crate::logger;
use crate::windows::{self, MonitorInfo};
use crate::AppState;
use rusqlite::Connection;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
pub fn search_songs(
    query: String,
    sort: Option<String>,
    collection_id: Option<i64>,
    state: State<AppState>,
) -> Result<Vec<SongHit>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let sort_by = match sort.as_deref() {
        Some("id") | Some("number") => "id",
        _ => "title",
    };
    db::search_songs(&db, &query, sort_by, collection_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_song(id: i64, state: State<AppState>) -> Result<Option<SongDetail>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::get_song(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resolve_wallpaper(name: String, app: AppHandle) -> Result<String, String> {
    let file_name = match name.as_str() {
        "Звёзды" => "Звёзды.mp4",
        "Камни" => "Камни.mp4",
        "Крест" => "Крест.mp4",
        "Небо" => "Небо.mp4",
        "Поле" => "Поле.mp4",
        _ => return Err("Unknown wallpaper.".into()),
    };
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let path = resource_dir.join("wallpapers").join(file_name);
    if !path.is_file() {
        return Err(format!("Wallpaper resource not found: {file_name}"));
    }
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Clone, serde::Serialize)]
pub struct FfmpegProgress {
    pub path: String,
    pub percent: f64,
    pub completed: bool,
    pub error: Option<String>,
}

fn sidecar_path(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join(format!("{name}.exe")));
        candidates.push(resource_dir.join(format!("{name}-x86_64-pc-windows-msvc.exe")));
        candidates.push(resource_dir.join(format!("{name}-x86_64-unknown-linux-gnu")));
    }
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("src-tauri").join("binaries").join(format!("{name}.exe")));
        candidates.push(current_dir.join("src-tauri").join("binaries").join(name));
    }
    candidates.into_iter().find(|path| path.is_file()).or_else(|| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).map(|dir| dir.join(format!("{name}.exe"))).find(|path| path.is_file())
        })
    }).ok_or_else(|| format!("{name} was not found. Add it to src-tauri/binaries or install it on PATH."))
}

/// Прячет консольное окно дочернего процесса.
///
/// Приложение собрано как GUI (windows subsystem), поэтому каждый запуск
/// консольного ffmpeg/ffprobe иначе открывает мелькающее чёрное окно.
fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// Контейнер видеофайла, определённый по сигнатуре (первые байты файла).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ContainerKind {
    /// MP4/ISO-BMFF (включая .mov, .m4v) — единственный контейнер, в котором
    /// `moov` может лежать после `mdat`: тогда браузер не начнёт воспроизведение.
    Mp4,
    /// WebM/Matroska — Chromium воспроизводит как есть.
    Webm,
    /// MPEG-TS (обычно .ts, но встречается и под именем .mp4 — экспорт из
    /// стриминговых и телевизионных систем).
    MpegTs,
    Avi,
    Flv,
    MpegPs,
    Unknown,
}

impl ContainerKind {
    fn label(self) -> &'static str {
        match self {
            ContainerKind::Mp4 => "mp4",
            ContainerKind::Webm => "webm/matroska",
            ContainerKind::MpegTs => "mpeg-ts",
            ContainerKind::Avi => "avi",
            ContainerKind::Flv => "flv",
            ContainerKind::MpegPs => "mpeg-ps",
            ContainerKind::Unknown => "неизвестный",
        }
    }

    /// Может ли Chromium (WebView2) открыть такой контейнер в теге <video>.
    fn is_web_playable(self) -> bool {
        matches!(self, ContainerKind::Mp4 | ContainerKind::Webm)
    }
}

/// Первые байты корневого атома MP4: по ним узнаём ISO-BMFF, даже если файл
/// начинается не с `ftyp` (например, сразу с `moov` или `free`).
fn is_mp4_box_type(kind: &[u8]) -> bool {
    const MP4_BOXES: [&[u8; 4]; 12] = [
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"uuid", b"styp", b"sidx", b"moof", b"junk",
    ];
    MP4_BOXES.iter().any(|known| kind == known.as_slice())
}

/// Сигнатура контейнера по первым байтам — мгновенно, без запуска ffprobe.
fn sniff_container(path: &Path) -> std::io::Result<ContainerKind> {
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 512];
    let read = file.read(&mut buffer)?;
    let head = &buffer[..read];
    if head.len() >= 8 && (&head[4..8] == b"ftyp" || is_mp4_box_type(&head[4..8])) {
        return Ok(ContainerKind::Mp4);
    }
    if head.len() >= 4 && head[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Ok(ContainerKind::Webm);
    }
    // MPEG-TS: пакеты по 188 байт, каждый начинается с синхробайта 0x47.
    if head.len() > 188 && head[0] == 0x47 && head[188] == 0x47 {
        return Ok(ContainerKind::MpegTs);
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return Ok(ContainerKind::Avi);
    }
    if head.len() >= 3 && &head[0..3] == b"FLV" {
        return Ok(ContainerKind::Flv);
    }
    if head.len() >= 4 && head[0..4] == [0x00, 0x00, 0x01, 0xBA] {
        return Ok(ContainerKind::MpegPs);
    }
    Ok(ContainerKind::Unknown)
}

/// Обход корневых атомов MP4: возвращает смещения `moov` и первого `mdat`.
/// Быстрая замена `ffprobe -v trace -i`, который читал файл целиком.
fn mp4_box_order(path: &Path) -> std::io::Result<(Option<u64>, Option<u64>)> {
    let mut file = File::open(path)?;
    let total = file.metadata()?.len();
    let mut moov: Option<u64> = None;
    let mut mdat: Option<u64> = None;
    let mut offset: u64 = 0;
    let mut header = [0u8; 16];
    while offset + 8 <= total {
        file.seek(SeekFrom::Start(offset))?;
        if file.read_exact(&mut header[..8]).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let kind = [header[4], header[5], header[6], header[7]];
        let mut header_len: u64 = 8;
        if size == 1 {
            // 64-битный размер: largesize лежит сразу за типом атома.
            if file.read_exact(&mut header[8..16]).is_err() {
                break;
            }
            let mut big = [0u8; 8];
            big.copy_from_slice(&header[8..16]);
            size = u64::from_be_bytes(big);
            header_len = 16;
        } else if size == 0 {
            // Атом занимает файл до конца.
            size = total - offset;
        }
        if size < header_len {
            break;
        }
        if kind == *b"moov" && moov.is_none() {
            moov = Some(offset);
        }
        if kind == *b"mdat" && mdat.is_none() {
            mdat = Some(offset);
        }
        if moov.is_some() && mdat.is_some() {
            break;
        }
        match offset.checked_add(size) {
            Some(next) if next > offset => offset = next,
            _ => break,
        }
    }
    Ok((moov, mdat))
}

/// Кодек видеодорожки через ffprobe: одна дешёвая проверка, окно консоли скрыто.
fn probe_video_codec(app: &AppHandle, path: &str) -> Option<String> {
    let ffprobe = sidecar_path(app, "ffprobe").ok()?;
    let mut command = Command::new(ffprobe);
    command
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stdin(Stdio::null());
    hide_console_window(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let codec = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
    if codec.is_empty() { None } else { Some(codec) }
}

/// Кодеки, которые Chromium/WebView2 декодирует в <video> без конвертации.
fn is_web_playable_codec(codec: &str) -> bool {
    matches!(codec, "h264" | "avc1" | "vp8" | "vp9" | "av1" | "theora")
}

/// Длительность файла через ffprobe — нужна конвертации для расчёта прогресса.
fn probe_duration(app: &AppHandle, path: &str) -> Result<Option<f64>, String> {
    let input = Path::new(path);
    let ffprobe = sidecar_path(app, "ffprobe")?;
    let mut command = Command::new(ffprobe);
    command
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(input)
        .stdin(Stdio::null());
    hide_console_window(&mut command);
    let output = command
        .output()
        .map_err(|e| format!("Could not run ffprobe: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().parse::<f64>().ok())
}

/// Нужна ли конвертация, чтобы файл игрался в окне вывода.
///
/// Проверяем три вещи: контейнер (MPEG-TS/AVI/FLV браузер не откроет вообще,
/// даже если файл назван `.mp4`), кодек видеодорожки (HEVC и MPEG-4 Part 2
/// не декодируются) и порядок атомов MP4 — `moov` обязан идти до `mdat`
/// (faststart), иначе воспроизведение не начнётся и зритель увидит чёрный экран.
///
/// `async` здесь обязателен: для проверки кодека запускается ffprobe, а запуск
/// процесса занимает ~90 мс. Синхронные команды Tauri исполняются в главном
/// потоке, поэтому шесть проверок при старте выстраивались в очередь на ~550 мс
/// и держали за собой `list_monitors` и `search_songs`. С `async` (sync_threadpool)
/// они уходят в пул потоков и выполняются параллельно.
#[tauri::command(async)]
pub fn check_video_optimization(path: String, app: AppHandle) -> Result<bool, String> {
    let input = PathBuf::from(&path);
    if !input.is_file() {
        return Err("Video file does not exist.".into());
    }
    let container = sniff_container(&input).map_err(|e| {
        logger::error("video", &format!("не удалось прочитать файл: {path} ({e})"));
        format!("Could not read the video file: {e}")
    })?;
    let mut reason = String::new();
    let mut needs_conversion: Option<String> = None;
    if !container.is_web_playable() {
        needs_conversion = Some(format!(
            "контейнер {} браузер не воспроизводит",
            container.label()
        ));
    } else {
        if container == ContainerKind::Mp4 {
            match mp4_box_order(&input).unwrap_or((None, None)) {
                (Some(moov), Some(mdat)) if moov > mdat => {
                    needs_conversion = Some("moov после mdat (нет faststart)".into());
                }
                (None, _) => needs_conversion = Some("не найден атом moov".into()),
                _ => {}
            }
        }
        if needs_conversion.is_none() {
            match probe_video_codec(&app, &path) {
                Some(codec) if !is_web_playable_codec(&codec) => {
                    needs_conversion = Some(format!("кодек {codec} браузер не декодирует"));
                }
                Some(codec) => reason = format!("{}, кодек {codec}", container.label()),
                None => reason = format!("{}, кодек не определён", container.label()),
            }
        }
    }
    let web_optimized = needs_conversion.is_none();
    let detail = needs_conversion.unwrap_or(reason);
    logger::info(
        "video",
        &format!(
            "проверка оптимизации: {path} → {} ({detail})",
            if web_optimized { "играется как есть" } else { "нужна конвертация" }
        ),
    );
    Ok(web_optimized)
}

/// `async` — до запуска потока конвертации здесь выполняется ffprobe (длительность
/// файла, ~90 мс); в главном потоке это задерживало остальные IPC-вызовы.
#[tauri::command(async)]
pub fn optimize_video(path: String, app: AppHandle) -> Result<(), String> {
    let input = PathBuf::from(&path);
    if !input.is_file() {
        return Err("Video file does not exist.".into());
    }
    let ffmpeg = sidecar_path(&app, "ffmpeg")?;
    let output = input.with_extension("faststart.tmp.mp4");
    let duration = probe_duration(&app, &path).ok().flatten().unwrap_or(0.0);
    let source = sniff_container(&input).map(|kind| kind.label()).unwrap_or("неизвестный");
    logger::info(
        "video",
        &format!(
            "конвертация в faststart: {path} (исходный контейнер {source}, ffmpeg: {}, длительность {duration:.1} с)",
            ffmpeg.display()
        ),
    );
    let event_app = app.clone();
    // Конвертация асинхронная: ffmpeg пишет прогресс в stderr (-progress pipe:2),
    // а мы транслируем его в UI событием ffmpeg-progress через tauri::Emitter.
    thread::spawn(move || {
        let mut command = Command::new(ffmpeg);
        command
            .args(["-y", "-i"]).arg(&input)
            .args(["-c", "copy", "-movflags", "+faststart", "-progress", "pipe:2"])
            .arg(&output)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::null());
        hide_console_window(&mut command);
        let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => { emit_ffmpeg(&event_app, &path, 0.0, false, Some(error.to_string())); return; }
            };
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let seconds = line
                    .strip_prefix("out_time_us=")
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|us| us / 1_000_000.0)
                    .or_else(|| {
                        line.strip_prefix("out_time_ms=")
                            .and_then(|v| v.parse::<f64>().ok())
                            .map(|ms| ms / 1_000.0)
                    });
                if let Some(seconds) = seconds {
                    let percent = if duration > 0.0 { (seconds / duration * 100.0).clamp(0.0, 100.0) } else { 0.0 };
                    emit_ffmpeg(&event_app, &path, percent, false, None);
                }
            }
        }
        match child.wait() {
            Ok(status) if status.success() => match fs::rename(&output, &input) {
                Ok(()) => emit_ffmpeg(&event_app, &path, 100.0, true, None),
                Err(error) => {
                    // Источник занят (файл воспроизводится) — копируем оптимизированный поверх.
                    if let Ok(_) = fs::copy(&output, &input) {
                        let _ = fs::remove_file(&output);
                        emit_ffmpeg(&event_app, &path, 100.0, true, None);
                    } else {
                        let _ = fs::remove_file(&output);
                        emit_ffmpeg(&event_app, &path, 0.0, false, Some(format!("Could not replace the source file: {error}")));
                    }
                }
            },
            Ok(status) => { let _ = fs::remove_file(&output); emit_ffmpeg(&event_app, &path, 0.0, false, Some(format!("ffmpeg exited with status {status}"))); }
            Err(error) => emit_ffmpeg(&event_app, &path, 0.0, false, Some(error.to_string())),
        }
    });
    Ok(())
}

fn emit_ffmpeg(app: &AppHandle, path: &str, percent: f64, completed: bool, error: Option<String>) {
    if let Some(message) = error.as_ref() {
        logger::error("ffmpeg", &format!("{path}: {message}"));
    } else if completed {
        logger::info("ffmpeg", &format!("{path}: конвертация завершена"));
    }
    let _ = app.emit("ffmpeg-progress", FfmpegProgress { path: path.to_string(), percent, completed, error });
}

#[tauri::command]
pub fn set_close_confirmation(enabled: bool, state: State<AppState>) -> Result<(), String> {
    *state.confirm_close.lock().map_err(|e| e.to_string())? = enabled;
    Ok(())
}

#[tauri::command]
pub fn confirm_application_close(
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    *state.allow_close.lock().map_err(|e| e.to_string())? = true;
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn backup_database(destination_path: String, state: State<AppState>) -> Result<(), String> {
    let destination = Path::new(&destination_path);
    if destination.as_os_str().is_empty() {
        return Err("Backup destination was not selected.".into());
    }
    let database = state.db.lock().map_err(|e| e.to_string())?;
    logger::info(
        "db",
        &format!(
            "резервная копия: {} → {}",
            state.db_path.display(),
            destination.display()
        ),
    );
    database
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| format!("Could not prepare database backup: {e}"))?;
    fs::copy(&state.db_path, destination).map_err(|e| {
        logger::error("db", &format!("не удалось создать резервную копию: {e}"));
        format!("Could not create database backup: {e}")
    })?;
    logger::info("db", "резервная копия создана");
    Ok(())
}

#[tauri::command]
pub fn restore_database(source_path: String, state: State<AppState>) -> Result<(), String> {
    let source = Path::new(&source_path);
    logger::info("db", &format!("восстановление базы из {}", source.display()));
    if !source.is_file() {
        return Err("The selected backup file does not exist.".into());
    }
    if source == state.db_path {
        return Err("The backup file must be different from the active database.".into());
    }

    let validation = Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("The selected file is not a valid SQLite database: {e}"))?;
    validation
        .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get::<_, i64>(0))
        .map_err(|e| format!("The selected file is not a valid SQLite database: {e}"))?;
    drop(validation);

    let mut database = state.db.lock().map_err(|e| e.to_string())?;
    database
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| format!("Could not close the active database cleanly: {e}"))?;
    let restore_backup = state.db_path.with_extension("sqlite.before-restore");
    fs::copy(&state.db_path, &restore_backup)
        .map_err(|e| format!("Could not protect the current database: {e}"))?;
    *database = Connection::open_in_memory()
        .map_err(|e| format!("Could not release the active database: {e}"))?;
    drop(database);

    if let Err(error) = fs::copy(source, &state.db_path) {
        let _ = fs::copy(&restore_backup, &state.db_path);
        let restored = db::open(&state.db_path).map_err(|restore_error| {
            format!("Could not restore the original database: {restore_error}")
        })?;
        *state.db.lock().map_err(|e| e.to_string())? = restored;
        return Err(format!("Could not restore the database: {error}"));
    }

    let reopened = match db::open(&state.db_path) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = fs::copy(&restore_backup, &state.db_path);
            let restored = db::open(&state.db_path).map_err(|restore_error| {
                format!("Could not restore the original database: {restore_error}")
            })?;
            *state.db.lock().map_err(|e| e.to_string())? = restored;
            return Err(format!("The backup could not be opened: {error}"));
        }
    };
    let mut database = state.db.lock().map_err(|e| e.to_string())?;
    *database = reopened;
    let _ = fs::remove_file(restore_backup);
    logger::info("db", "база восстановлена из резервной копии");
    Ok(())
}

#[tauri::command]
pub fn list_collections(state: State<AppState>) -> Result<Vec<Collection>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::list_collections(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_collection(name: String, state: State<AppState>) -> Result<Collection, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Введите название сборника.".into());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::create_collection(&db, &name).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("UNIQUE") {
            "Сборник с таким названием уже есть.".into()
        } else {
            msg
        }
    })
}

#[tauri::command]
pub fn rename_collection(
    id: i64,
    name: String,
    state: State<AppState>,
) -> Result<Collection, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Введите название сборника.".into());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::rename_collection(&db, id, &name).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("UNIQUE") {
            "Сборник с таким названием уже есть.".into()
        } else if msg.contains("QueryReturnedNoRows") || msg.contains("no rows") {
            "Сборник не найден.".into()
        } else {
            msg
        }
    })
}

#[tauri::command]
pub fn collection_song_count(id: i64, state: State<AppState>) -> Result<i64, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::collection_song_count(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_collection(
    id: i64,
    delete_songs: bool,
    move_to: Option<i64>,
    state: State<AppState>,
) -> Result<(), String> {
    if !delete_songs && move_to.is_none() {
        return Err("Выберите, что сделать с песнями.".into());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::delete_collection(&db, id, delete_songs, move_to).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("QueryReturnedNoRows") || msg.contains("no rows") {
            "Сборник не найден.".into()
        } else if msg.contains("cannot move to the same") {
            "Нельзя переместить песни в удаляемый сборник.".into()
        } else if msg.contains("target collection not found") {
            "Целевой сборник не найден.".into()
        } else if msg.contains("move target required") {
            "Выберите сборник для переноса песен.".into()
        } else {
            msg
        }
    })
}

#[tauri::command]
pub fn save_song(
    edit_id: Option<i64>,
    number: Option<i64>,
    title: String,
    collection_id: i64,
    slides: Vec<String>,
    state: State<AppState>,
) -> Result<SongDetail, String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Введите название песни.".into());
    }
    if collection_id <= 0 {
        return Err("Сборник не выбран.".into());
    }
    if slides.is_empty() {
        return Err("Добавьте хотя бы один слайд.".into());
    }
    let text = db::join_slides(&slides);
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::save_song(&db, edit_id, number, &title, collection_id, &text).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("song number already exists") {
            "Песня с таким номером уже существует.".into()
        } else if msg.contains("collection not found") {
            "Сборник не выбран.".into()
        } else {
            msg
        }
    })
}

#[tauri::command]
pub fn import_legacy_chorus_json(
    raw_json: String,
    state: State<AppState>,
) -> Result<db::LegacyImportResult, String> {
    let mut database = state.db.lock().map_err(|e| e.to_string())?;
    db::import_legacy_chorus_json(&mut database, &raw_json)
}

#[tauri::command]
pub fn delete_song(id: i64, state: State<AppState>) -> Result<bool, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::delete_song(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn next_song_id(state: State<AppState>) -> Result<i64, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::next_song_id(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_bible_books(state: State<AppState>) -> Result<Vec<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::list_bible_books(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_bible(
    book: String,
    chapter: i32,
    verse_from: i32,
    verse_to: Option<i32>,
    state: State<AppState>,
) -> Result<Vec<Verse>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::search_bible(&db, &book, chapter, verse_from, verse_to).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_bible_query(query: String, state: State<AppState>) -> Result<Vec<Verse>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::search_bible_query(&db, &query).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn bible_chapter_count(book: String, state: State<AppState>) -> Result<i32, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::bible_chapter_count(&db, &book).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_next_bible_chapter(
    book: String,
    chapter: i64,
    state: State<AppState>,
) -> Result<Option<(String, i64, Vec<db::BibleVerseRow>)>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::get_next_bible_chapter(&db, &book, chapter).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_bible_chapter(
    book: String,
    chapter: i32,
    state: State<AppState>,
) -> Result<Vec<Verse>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::get_bible_chapter(&db, &book, chapter).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Result<Vec<MonitorInfo>, String> {
    windows::list_monitors(&app)
}

#[tauri::command]
pub async fn set_display_monitor(
    index: usize,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut preferred = state.preferred_monitor.lock().map_err(|e| e.to_string())?;
        *preferred = Some(index);
    }
    // Window ops must not run in a sync command on Windows (WebView2 deadlock).
    windows::place_display(&app, Some(index)).map_err(|e| e.to_string())
}

/// Must be async on Windows — sync WebviewWindowBuilder::build deadlocks (WebView2).
#[tauri::command]
pub async fn ensure_display(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let preferred = *state
        .preferred_monitor
        .lock()
        .map_err(|e| e.to_string())?;
    eprintln!("[display] command ensure_display preferred={preferred:?}");
    logger::info("display", &format!("команда ensure_display (монитор {preferred:?})"));
    windows::ensure_display(&app, preferred).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn close_display(app: AppHandle) -> Result<(), String> {
    eprintln!("[display] command close_display");
    logger::info("display", "команда close_display");
    windows::close_display(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_playlists(state: State<AppState>) -> Result<Vec<PlaylistSummary>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::list_playlists(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_playlist(id: i64, state: State<AppState>) -> Result<Option<PlaylistDetail>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::get_playlist(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_playlist(
    name: String,
    items: Vec<PlaylistItemRow>,
    state: State<AppState>,
) -> Result<PlaylistDetail, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Введите название плейлиста.".into());
    }
    if items.is_empty() {
        return Err("Быстрый плейлист пуст.".into());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::create_playlist(&db, &name, &items).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_playlist(id: i64, state: State<AppState>) -> Result<bool, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::delete_playlist(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_styles(state: State<AppState>) -> Result<Vec<StyleRow>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::list_styles(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_active_style(state: State<AppState>) -> Result<Option<StyleRow>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::get_active_style(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_style(
    id: Option<i64>,
    name: String,
    config_json: String,
    state: State<AppState>,
) -> Result<StyleRow, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Введите название стиля.".into());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::save_style(&db, id, &name, &config_json).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("QueryReturnedNoRows") || msg.contains("no rows") {
            "Стиль не найден.".into()
        } else {
            msg
        }
    })
}

#[tauri::command]
pub fn set_active_style(id: i64, state: State<AppState>) -> Result<Option<StyleRow>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::set_active_style(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_style(id: i64, state: State<AppState>) -> Result<bool, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::delete_style(&db, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_hotkeys(state: State<AppState>) -> Result<Vec<HotkeyRow>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::get_hotkeys(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_hotkeys(bindings: Vec<HotkeyRow>, state: State<AppState>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db::save_hotkeys(&db, &bindings).map_err(|e| e.to_string())
}

/* ——— Журнал работы приложения ——— */

/// Пакетная запись событий фронтенда в файл журнала текущего сеанса.
#[tauri::command]
pub fn log_events(entries: Vec<logger::LogEntry>) {
    for entry in entries {
        logger::log_with_data(&entry.level, &entry.scope, &entry.message, entry.data);
    }
}

/// Сводка о журнале (каталог, текущий файл, список файлов) для вкладки «Журнал».
#[tauri::command]
pub fn get_log_info() -> logger::LogInfo {
    logger::info_snapshot()
}

/// Открывает каталог с журналами в системном файловом менеджере.
#[tauri::command]
pub fn open_logs_folder() -> Result<String, String> {
    let dir = logger::current_dir().ok_or_else(|| "Журнал ещё не инициализирован.".to_string())?;
    let dir_string = dir.to_string_lossy().into_owned();
    logger::info("app", &format!("Открытие каталога журналов: {dir_string}"));

    #[cfg(windows)]
    let opened = Command::new("explorer").arg(&dir).spawn();
    #[cfg(not(windows))]
    let opened = Command::new("xdg-open").arg(&dir).spawn();

    opened.map_err(|e| format!("Не удалось открыть каталог журналов: {e}"))?;
    Ok(dir_string)
}

/// Первые два байта файла в hex — проверка PE-подписи `MZ` (4d5a).
fn read_header(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = [0u8; 2];
    file.read_exact(&mut buf).ok()?;
    Some(format!("{:02x}{:02x}", buf[0], buf[1]))
}

/// Диагностика внешних бинарников ffmpeg/ffprobe при старте приложения.
///
/// Проверяет наличие файла, размер и PE-подпись: именно так выявляется
/// подложенный не-Windows (например, Mach-O от macOS) файл с расширением `.exe`.
pub fn log_sidecars(app: &AppHandle) {
    for name in ["ffmpeg", "ffprobe"] {
        match sidecar_path(app, name) {
            Ok(path) => match fs::metadata(&path) {
                Ok(meta) => {
                    let header = read_header(&path);
                    let is_pe = header.as_deref() == Some("4d5a");
                    let message = format!(
                        "{name}: {} ({} байт, заголовок {})",
                        path.display(),
                        meta.len(),
                        header.unwrap_or_else(|| "?".to_string())
                    );
                    if is_pe {
                        logger::info("sidecar", &message);
                    } else {
                        logger::error(
                            "sidecar",
                            &format!("{message} — это не Windows PE (MZ), запуск невозможен!"),
                        );
                    }
                }
                Err(error) => logger::error(
                    "sidecar",
                    &format!("{name}: нет доступа к {}: {error}", path.display()),
                ),
            },
            Err(error) => logger::error("sidecar", &format!("{name}: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chyguislide-video-{tag}-{}", std::process::id()))
    }

    /// Собирает файл из атомов MP4: (тип, размер полезной части).
    fn write_mp4(path: &Path, boxes: &[(&[u8; 4], usize)]) {
        let mut data = Vec::new();
        for (kind, payload) in boxes {
            data.extend_from_slice(&((payload + 8) as u32).to_be_bytes());
            data.extend_from_slice(*kind);
            data.extend(std::iter::repeat(0u8).take(*payload));
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn sniffs_mpeg_ts_hidden_behind_mp4_extension() {
        // Реальный случай: «Цифровой рубль.mp4» — на самом деле контейнер MPEG-TS,
        // поэтому браузер показывал чёрный экран вместо видео.
        let dir = temp_dir("ts");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.mp4");
        let mut data = vec![0u8; 188 * 4];
        for packet in 0..4 {
            data[packet * 188] = 0x47;
        }
        fs::write(&path, &data).unwrap();

        let kind = sniff_container(&path).unwrap();
        assert_eq!(kind, ContainerKind::MpegTs);
        assert!(!kind.is_web_playable());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sniffs_webm_and_avi_signatures() {
        let dir = temp_dir("containers");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let webm = dir.join("clip.webm");
        let mut data = vec![0x1A, 0x45, 0xDF, 0xA3];
        data.extend_from_slice(&[0u8; 64]);
        fs::write(&webm, &data).unwrap();
        assert_eq!(sniff_container(&webm).unwrap(), ContainerKind::Webm);

        let avi = dir.join("clip.avi");
        let mut data = b"RIFF".to_vec();
        data.extend_from_slice(&[0u8; 4]);
        data.extend_from_slice(b"AVI ");
        data.extend_from_slice(&[0u8; 32]);
        fs::write(&avi, &data).unwrap();
        assert_eq!(sniff_container(&avi).unwrap(), ContainerKind::Avi);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_moov_after_mdat() {
        let dir = temp_dir("no-faststart");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.mp4");
        write_mp4(&path, &[(b"ftyp", 16), (b"mdat", 64), (b"moov", 16)]);

        assert_eq!(sniff_container(&path).unwrap(), ContainerKind::Mp4);
        let (moov, mdat) = mp4_box_order(&path).unwrap();
        assert!(moov.unwrap() > mdat.unwrap(), "moov должен идти после mdat");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_faststart_moov_before_mdat() {
        let dir = temp_dir("faststart");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.mp4");
        write_mp4(&path, &[(b"ftyp", 16), (b"moov", 16), (b"mdat", 64)]);

        assert_eq!(sniff_container(&path).unwrap(), ContainerKind::Mp4);
        let (moov, mdat) = mp4_box_order(&path).unwrap();
        assert!(moov.unwrap() < mdat.unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recognises_mp4_without_ftyp_first() {
        // Файл может начинаться с `moov` (faststart) или `free` — это всё ещё MP4.
        let dir = temp_dir("no-ftyp");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.mp4");
        write_mp4(&path, &[(b"moov", 16), (b"mdat", 64)]);
        assert_eq!(sniff_container(&path).unwrap(), ContainerKind::Mp4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_codecs_that_webview_cannot_decode() {
        assert!(is_web_playable_codec("h264"));
        assert!(is_web_playable_codec("vp9"));
        assert!(!is_web_playable_codec("hevc"));
        assert!(!is_web_playable_codec("mpeg4"));
    }
}


