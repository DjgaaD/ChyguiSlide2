mod commands;
mod db;
mod import;
mod logger;
mod obs;
mod opener;
mod updater;
mod windows;
mod yandex;

use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

pub struct AppState {
    pub db: Mutex<Connection>,
    pub db_path: PathBuf,
    pub preferred_monitor: Mutex<Option<usize>>,
    pub confirm_close: Mutex<bool>,
    pub allow_close: Mutex<bool>,
    pub close_prompt_open: Mutex<bool>,
    /// LAN-вывод слов в OBS (источник «Браузер»).
    pub obs: Arc<obs::ObsRuntime>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            let handle = app.handle().clone();
            // Журнал инициализируем первым: дальше логируется каждый шаг запуска.
            match logger::init(&handle) {
                Ok(path) => eprintln!("[log] {}", path.display()),
                Err(error) => eprintln!("[log] не удалось открыть журнал: {error}"),
            }
            logger::info(
                "app",
                &format!(
                    "ChyguiSlide {} ({} {}, Tauri {})",
                    app.package_info().version,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    tauri::VERSION
                ),
            );
            let db_path = app
                .path()
                .app_data_dir()
                .map_err(|e| e.to_string())?
                .join("chyguislide.sqlite");
            logger::info("db", &format!("Файл базы данных: {}", db_path.display()));
            let mut conn = db::open(&db_path).map_err(|e| {
                logger::error("db", &format!("не удалось открыть базу: {e}"));
                e.to_string()
            })?;
            import::seed_if_needed(&handle, &mut conn)?;
            import::ensure_default_collection(&conn)?;
            app.manage(AppState {
                db: Mutex::new(conn),
                db_path,
                preferred_monitor: Mutex::new(None),
                confirm_close: Mutex::new(false),
                allow_close: Mutex::new(false),
                close_prompt_open: Mutex::new(false),
                obs: Arc::new(obs::ObsRuntime::new()),
            });
            // Вывод слов в OBS поднимается вместе с приложением, если включён в настройках.
            obs::start_from_settings(&handle);
            // Диагностика внешних бинарников: наличие, размер и PE-подпись (MZ).
            commands::log_sidecars(&handle);
            // Файлы прошлого обновления (скачанная часть и установщик) в кэше
            // больше не нужны: их удаление идёт в фоне и не задерживает запуск.
            updater::cleanup_staging(&handle);
            logger::info("app", "инициализация завершена");
            // Display opens on demand (show / persistent second-screen background).
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { .. } => {
                    logger::info("window", &format!("{}: запрошено закрытие окна", window.label()));
                }
                tauri::WindowEvent::Destroyed => {
                    logger::info("window", &format!("{}: окно уничтожено", window.label()));
                }
                tauri::WindowEvent::Focused(focused) => {
                    logger::debug("window", &format!("{}: фокус = {focused}", window.label()));
                }
                _ => {}
            }
            if window.label() == windows::CONTROLLER_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let state = window.state::<AppState>();
                    let allow_close = state.allow_close.lock().map(|value| *value).unwrap_or(false);
                    if allow_close {
                        if let Ok(mut value) = state.allow_close.lock() {
                            *value = false;
                        }
                    } else if state.confirm_close.lock().map(|value| *value).unwrap_or(false) {
                        api.prevent_close();
                        let already_open = state
                            .close_prompt_open
                            .lock()
                            .map(|mut value| {
                                if *value {
                                    true
                                } else {
                                    *value = true;
                                    false
                                }
                            })
                            .unwrap_or(true);
                        if !already_open {
                            let app = window.app_handle().clone();
                            let controller = window.clone();
                            std::thread::spawn(move || {
                                let confirmed = app
                                    .dialog()
                                    .message("Вы действительно хотите закрыть программу?")
                                    .buttons(MessageDialogButtons::YesNo)
                                    .blocking_show();
                                if let Ok(mut value) = app.state::<AppState>().close_prompt_open.lock() {
                                    *value = false;
                                }
                                if confirmed {
                                    logger::info("app", "закрытие подтверждено пользователем");
                                    if let Ok(mut value) = app.state::<AppState>().allow_close.lock() {
                                        *value = true;
                                    }
                                    let _ = controller.close();
                                } else {
                                    logger::info("app", "закрытие отменено пользователем");
                                }
                            });
                        }
                    }
                }
                if let tauri::WindowEvent::Destroyed = event {
                    logger::info("app", "главное окно закрыто — выходим из приложения");
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_songs,
            commands::get_song,
            commands::resolve_wallpaper,
            commands::check_video_optimization,
            commands::optimize_video,
            commands::set_close_confirmation,
            commands::confirm_application_close,
            commands::backup_database,
            commands::restore_database,
            commands::list_collections,
            commands::create_collection,
            commands::rename_collection,
            commands::collection_song_count,
            commands::delete_collection,
            commands::save_song,
            commands::import_legacy_chorus_json,
            commands::delete_song,
            commands::next_song_id,
            commands::list_bible_books,
            commands::search_bible,
            commands::search_bible_query,
            commands::bible_chapter_count,
            commands::get_bible_chapter,
            commands::get_next_bible_chapter,
            commands::list_monitors,
            commands::set_display_monitor,
            commands::ensure_display,
            commands::close_display,
            commands::list_playlists,
            commands::get_playlist,
            commands::create_playlist,
            commands::delete_playlist,
            commands::list_styles,
            commands::get_active_style,
            commands::save_style,
            commands::set_active_style,
            commands::delete_style,
            commands::get_hotkeys,
            commands::save_hotkeys,
            commands::get_app_settings,
            commands::set_app_setting,
            commands::log_events,
            commands::get_log_info,
            commands::open_logs_folder,
            obs::obs_settings,
            obs::obs_save_settings,
            obs::obs_push_slide,
            obs::obs_push_style,
            opener::open_external_link,
            updater::get_app_info,
            updater::check_app_update,
            updater::skip_app_update,
            updater::install_app_update,
            yandex::yandex_settings,
            yandex::save_yandex_settings,
            yandex::check_yandex_token,
            yandex::yandex_backup,
            yandex::open_yandex_token_page,
            yandex::open_yandex_backups_folder,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| match event {
            tauri::RunEvent::ExitRequested { .. } => {
                logger::info("app", "запрошен выход из приложения");
            }
            tauri::RunEvent::Exit => {
                logger::info("app", "=== Сеанс завершён ===");
            }
            _ => {}
        });
}
