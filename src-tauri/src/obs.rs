//! Вывод слов слайда в OBS.
//!
//! OBS забирает текст источником «Браузер» (Browser Source): приложение поднимает
//! небольшой HTTP-сервер на всех интерфейсах (`0.0.0.0:<порт>`, по умолчанию
//! 8765 — как в прежней версии программы) и отдаёт страницу-оверлей с прозрачным
//! фоном. Слова приходят на страницу потоком событий (SSE, `/events`), поэтому
//! оверлей не опрашивает сервер и не отстаёт от эфира: каждое изменение состояния
//! уходит подписчикам сразу.
//!
//! Состояние вывода (текст слайда и оформление) живёт в памяти приложения: страница
//! получает готовый снимок при подключении, поэтому перезапуск OBS или перезагрузка
//! источника не оставляют оверлей пустым. Оформление приходит из интерфейса готовым
//! объектом активного стиля — бэкенд его не разбирает, а только передаёт дальше.
//!
//! Сервер написан на стандартной библиотеке (TCP + потоки): новых зависимостей
//! модуль не требует. Настройки (включён, порт, подложка под текст) лежат в таблице
//! `app_settings`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::db;
use crate::logger;
use crate::AppState;

/// Порт по умолчанию — тот же, что был в прежней версии программы.
const DEFAULT_PORT: u16 = 8765;
/// Нижняя граница порта: системные и привилегированные порты не предлагаем.
const PORT_MIN: u16 = 1024;
/// Ключ настройки в базе: вывод в OBS включён.
const ENABLED_KEY: &str = "obs.enabled";
/// Ключ настройки в базе: порт сервера вывода.
const PORT_KEY: &str = "obs.port";
/// Ключ настройки в базе: подложка под текст включена.
const BACKDROP_ENABLED_KEY: &str = "obs.backdropEnabled";
/// Ключ настройки в базе: непрозрачность подложки в процентах (0…100).
const BACKDROP_OPACITY_KEY: &str = "obs.backdropOpacity";
/// Непрозрачность подложки по умолчанию — та же, что была в прежней версии.
const BACKDROP_OPACITY_DEFAULT: f64 = 90.0;
/// Сколько обновлений ждёт медленный подписчик: дальше он получает только свежий снимок.
const CLIENT_QUEUE: usize = 32;
/// Как часто проверяем флаг остановки в цикле приёма соединений.
const ACCEPT_POLL: Duration = Duration::from_millis(200);
/// Как часто отправляем подписчику комментарий-пульс, чтобы соединение не «умирало».
const HEARTBEAT: Duration = Duration::from_secs(10);
/// Сколько ждём строку запроса: браузер открывает соединения и заранее, поэтому
/// брошенное соединение не должно занимать поток бесконечно.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Предел строки запроса: заголовки источника «Браузер» заметно короче.
const REQUEST_LIMIT: usize = 8 * 1024;
/// Страница-оверлей для источника «Браузер».
const OVERLAY_HTML: &str = include_str!("obs-overlay.html");
/// Скрипт страницы-оверлея (отдаётся по адресу `/app.js`).
const OVERLAY_SCRIPT: &str = include_str!("obs-overlay.js");

/// Текст текущего слайда — то, что видно в OBS.
///
/// Текст всегда одна строка: строки слайда склеиваются пробелом ещё в интерфейсе
/// (см. `src/controller/obs.ts`), подпись стиха идёт в ту же строку — так было и в
/// прежней версии программы. Переносы по ширине источника делает оверлей.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsSlide {
    /// Текст слайда одной строкой; пустая строка — очистка экрана.
    pub text: String,
    /// Режим вывода: bible / song / announcement (для отладки и будущих нужд).
    pub mode: String,
}

/// Подложка под текст: непрозрачность — доля от 0 до 1.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsBackdrop {
    pub enabled: bool,
    pub opacity: f64,
}

impl Default for ObsBackdrop {
    fn default() -> Self {
        Self {
            enabled: false,
            opacity: BACKDROP_OPACITY_DEFAULT / 100.0,
        }
    }
}

/// Снимок состояния вывода — его получает каждый подключённый оверлей.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsSnapshot {
    pub slide: ObsSlide,
    pub backdrop: ObsBackdrop,
    /// Оформление (активный стиль из интерфейса) как есть.
    pub style: serde_json::Value,
}

/// Настройки вывода из базы: включённость, порт и подложка.
#[derive(Clone, Debug)]
pub struct ObsConfig {
    enabled: bool,
    port: u16,
    backdrop_enabled: bool,
    /// Непрозрачность подложки в процентах — как в форме настроек.
    backdrop_opacity: f64,
}

impl ObsConfig {
    /// Подложка для оверлея: проценты формы превращаем в долю непрозрачности.
    fn backdrop(&self) -> ObsBackdrop {
        ObsBackdrop {
            enabled: self.backdrop_enabled,
            opacity: (self.backdrop_opacity / 100.0).clamp(0.0, 1.0),
        }
    }
}

/// Настройки и состояние вывода в OBS для вкладки настроек.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsSettings {
    pub enabled: bool,
    pub port: u16,
    /// Подложка под текст включена.
    pub backdrop_enabled: bool,
    /// Непрозрачность подложки в процентах (0…100).
    pub backdrop_opacity: f64,
    pub running: bool,
    /// Последняя ошибка запуска сервера (нет, если всё в порядке).
    pub last_error: Option<String>,
    /// Адреса страницы-оверлея: локальный и (если есть) адрес в локальной сети.
    pub urls: Vec<String>,
}

/// Общее состояние сервера вывода: снимок слайда, подписчики и поток приёма.
pub struct ObsRuntime {
    /// Снимок, который получает каждый подключившийся оверлей.
    snapshot: Mutex<ObsSnapshot>,
    /// Подписчики потока событий: номер и канал, в который пишет рассылка.
    clients: Mutex<Vec<(u64, SyncSender<String>)>>,
    next_client: AtomicU64,
    /// Флаг остановки работающего сервера (нет — сервер не запущен).
    stop: Mutex<Option<Arc<AtomicBool>>>,
    running: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl ObsRuntime {
    pub fn new() -> Self {
        Self {
            snapshot: Mutex::new(ObsSnapshot {
                slide: ObsSlide::default(),
                backdrop: ObsBackdrop::default(),
                style: serde_json::Value::Null,
            }),
            clients: Mutex::new(Vec::new()),
            next_client: AtomicU64::new(1),
            stop: Mutex::new(None),
            running: AtomicBool::new(false),
            last_error: Mutex::new(None),
        }
    }

    /// Обновляет текст слайда и рассылает снимок подписчикам.
    pub fn set_slide(&self, slide: ObsSlide) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.slide = slide;
        }
        self.broadcast_current();
    }

    /// Обновляет оформление и рассылает снимок подписчикам.
    pub fn set_style(&self, style: serde_json::Value) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.style = style;
        }
        self.broadcast_current();
    }

    /// Обновляет подложку под текст и рассылает снимок подписчикам.
    pub fn set_backdrop(&self, backdrop: ObsBackdrop) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.backdrop = backdrop;
        }
        self.broadcast_current();
    }

    /// Снимок состояния в виде JSON (нет, если состояние недоступно).
    fn snapshot_json(&self) -> Option<String> {
        let snapshot = self.snapshot.lock().ok()?;
        serde_json::to_string(&*snapshot).ok()
    }

    /// Рассылает текущий снимок всем подписчикам.
    fn broadcast_current(&self) {
        let Some(json) = self.snapshot_json() else {
            return;
        };
        let Ok(mut clients) = self.clients.lock() else {
            return;
        };
        // Медленному подписчику сообщения не копим: следующее состояние всё равно
        // заменит пропущенное, поэтому переполненный канал — не ошибка.
        clients.retain(|(_, sender)| {
            !matches!(
                sender.try_send(json.clone()),
                Err(TrySendError::Disconnected(_))
            )
        });
    }

    /// Добавляет подписчика и возвращает его номер для последующего удаления.
    fn add_client(&self, sender: SyncSender<String>) -> u64 {
        let id = self.next_client.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut clients) = self.clients.lock() {
            clients.push((id, sender));
        }
        id
    }

    fn remove_client(&self, id: u64) {
        if let Ok(mut clients) = self.clients.lock() {
            clients.retain(|(client_id, _)| *client_id != id);
        }
    }

    /// Запускает сервер вывода. Повторный вызов перезапускает его на новом порту.
    pub fn start(self: &Arc<Self>, port: u16) -> Result<(), String> {
        self.stop();
        let listener = TcpListener::bind(("0.0.0.0", port))
            .map_err(|error| format!("порт {port} недоступен: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;

        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut slot) = self.stop.lock() {
            *slot = Some(Arc::clone(&stop));
        }
        self.running.store(true, Ordering::Relaxed);
        self.set_last_error(None);

        let runtime = Arc::clone(self);
        thread::spawn(move || {
            logger::info(
                "obs",
                &format!(
                    "сервер вывода слушает 0.0.0.0:{port} ({})",
                    overlay_urls(port).join(", ")
                ),
            );
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve_connection(stream, &runtime),
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(ACCEPT_POLL);
                    }
                    Err(error) => {
                        logger::error("obs", &format!("приём соединения прерван: {error}"));
                        break;
                    }
                }
            }
            runtime.finish(&stop);
            logger::info("obs", "сервер вывода остановлен");
        });
        Ok(())
    }

    /// Останавливает сервер (если он запущен) и отпускает подписчиков.
    pub fn stop(&self) {
        let flag = self.stop.lock().ok().and_then(|mut slot| slot.take());
        if let Some(flag) = flag {
            flag.store(true, Ordering::Relaxed);
            // Поток приёма выходит из цикла в пределах ACCEPT_POLL: ждём, чтобы
            // следующий запуск гарантированно получил свободный порт.
            thread::sleep(ACCEPT_POLL + Duration::from_millis(50));
        }
        if let Ok(mut clients) = self.clients.lock() {
            // Страница сама переподключится к новому серверу.
            clients.clear();
        }
        self.running.store(false, Ordering::Relaxed);
    }

    /// Помечает сервер остановленным, только если его флаг ещё актуален: иначе уже
    /// работает новый сервер и трогать состояние нельзя.
    fn finish(&self, stop: &Arc<AtomicBool>) {
        if let Ok(mut slot) = self.stop.lock() {
            let is_current = slot
                .as_ref()
                .map(|current| Arc::ptr_eq(current, stop))
                .unwrap_or(false);
            if is_current {
                slot.take();
                self.running.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Приводит сервер и подложку в соответствие сохранённым настройкам.
    pub fn apply(self: &Arc<Self>, config: &ObsConfig) {
        // Подложку применяем всегда: она нужна и при выключенном выводе, чтобы
        // оверлей показывал настройку сразу после включения.
        self.set_backdrop(config.backdrop());
        if config.enabled {
            if let Err(error) = self.start(config.port) {
                logger::error(
                    "obs",
                    &format!("не удалось запустить сервер вывода: {error}"),
                );
                self.set_last_error(Some(error));
            }
        } else {
            self.stop();
            self.set_last_error(None);
            logger::info("obs", "вывод в OBS выключен в настройках");
        }
    }

    /// Состояние вывода для интерфейса.
    pub fn view(&self, config: &ObsConfig) -> ObsSettings {
        ObsSettings {
            enabled: config.enabled,
            port: config.port,
            backdrop_enabled: config.backdrop_enabled,
            backdrop_opacity: config.backdrop_opacity,
            running: self.running.load(Ordering::Relaxed),
            last_error: self.last_error.lock().ok().and_then(|value| value.clone()),
            urls: overlay_urls(config.port),
        }
    }

    fn set_last_error(&self, message: Option<String>) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = message;
        }
    }
}

impl Default for ObsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Запрос оверлея: метод и путь без параметров строки запроса.
struct HttpRequest {
    method: String,
    path: String,
}

/// Обслуживает соединение в отдельном потоке: источник «Браузер» открывает их
/// несколько (страница, поток событий), и медленное соединение не должно
/// задерживать остальные.
fn serve_connection(stream: TcpStream, runtime: &Arc<ObsRuntime>) {
    let runtime = Arc::clone(runtime);
    thread::spawn(move || {
        if let Err(error) = serve(stream, &runtime) {
            logger::debug("obs", &format!("соединение закрыто: {error}"));
        }
    });
}

fn serve(mut stream: TcpStream, runtime: &Arc<ObsRuntime>) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    // Слушающий сокет неблокирующий, и в Windows это свойство наследует сокет,
    // принятый `accept`: чтение запроса «на лету» сразу возвращало ошибку 10035 и
    // соединение закрывалось без ответа — источник «Браузер» в OBS оставался
    // пустым. Поэтому принятое соединение переводим в обычный режим.
    stream.set_nonblocking(false)?;
    // Ждём запрос не бесконечно: браузер открывает соединения и заранее.
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    // Дальше соединение только пишет: таймаут чтения больше не нужен.
    stream.set_read_timeout(None)?;
    if request.method != "GET" {
        return write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "Поддерживается только GET".as_bytes(),
        );
    }
    match request.path.as_str() {
        "/" | "/index.html" => write_response(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            OVERLAY_HTML.as_bytes(),
        ),
        "/api/state" => {
            let body = runtime.snapshot_json().unwrap_or_else(|| "{}".to_string());
            write_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            )
        }
        "/events" => stream_events(&mut stream, runtime),
        "/app.js" => write_response(
            &mut stream,
            "200 OK",
            "application/javascript; charset=utf-8",
            OVERLAY_SCRIPT.as_bytes(),
        ),
        _ => write_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "Не найдено".as_bytes(),
        ),
    }
}

/// Читает строку запроса и заголовки (до пустой строки).
fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<HttpRequest>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    loop {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            // Таймаут ожидания запроса: соединение открыто заранее и брошено
            // (так делает браузер источника «Браузер»). Это не ошибка — молча закрываем.
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() >= REQUEST_LIMIT {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buffer);
    let request_line = text.split("\r\n").next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default();
    if method.is_empty() || target.is_empty() {
        return Ok(None);
    }
    // Параметры строки запроса (например, размер сцены) сервер не разбирает:
    // их читает сама страница-оверлей.
    let path = target.split('?').next().unwrap_or("/").to_string();
    Ok(Some(HttpRequest { method, path }))
}

/// Отправляет ответ с телом и закрывает соединение.
fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Поток событий (SSE): сразу отдаёт текущее состояние и дальше — каждое
/// изменение, поэтому оверлей не опрашивает сервер и не отстаёт от эфира.
fn stream_events(stream: &mut TcpStream, runtime: &Arc<ObsRuntime>) -> std::io::Result<()> {
    let (sender, receiver): (SyncSender<String>, Receiver<String>) =
        mpsc::sync_channel(CLIENT_QUEUE);
    let _registration = ClientRegistration::new(runtime, sender);

    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n";
    stream.write_all(head.as_bytes())?;
    stream.flush()?;

    if let Some(snapshot) = runtime.snapshot_json() {
        write_event(stream, &snapshot)?;
    }
    loop {
        match receiver.recv_timeout(HEARTBEAT) {
            Ok(payload) => write_event(stream, &payload)?,
            // Пульс: без него сеть или OBS могут закрыть «тихое» соединение.
            Err(RecvTimeoutError::Timeout) => {
                stream.write_all(b": ping\r\n\r\n")?;
                stream.flush()?;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Отправляет одно событие. `serde_json` экранирует переводы строк, поэтому тело
/// помещается в одно поле `data:` — как и требует формат SSE.
fn write_event(stream: &mut TcpStream, payload: &str) -> std::io::Result<()> {
    stream.write_all(format!("data: {payload}\n\n").as_bytes())?;
    stream.flush()
}

/// Подписчик: снимает себя со списка рассылки при любом выходе из потока.
struct ClientRegistration {
    runtime: Arc<ObsRuntime>,
    id: u64,
}

impl ClientRegistration {
    fn new(runtime: &Arc<ObsRuntime>, sender: SyncSender<String>) -> Self {
        Self {
            runtime: Arc::clone(runtime),
            id: runtime.add_client(sender),
        }
    }
}

impl Drop for ClientRegistration {
    fn drop(&mut self) {
        self.runtime.remove_client(self.id);
    }
}

/// Адреса страницы-оверлея: локальный (OBS на этом же компьютере) и адрес в
/// локальной сети (OBS на другом компьютере).
fn overlay_urls(port: u16) -> Vec<String> {
    let mut urls = vec![format!("http://127.0.0.1:{port}/")];
    if let Some(address) = primary_lan_ipv4() {
        urls.push(format!("http://{address}:{port}/"));
    }
    urls
}

/// Основной адрес компьютера в локальной сети. Сокет «примеряет» соединение с
/// внешним адресом, и система сама выбирает рабочий интерфейс — без сетевых
/// запросов и новых зависимостей.
fn primary_lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) => Some(*address.ip()),
        SocketAddr::V6(_) => None,
    }
}

/// Читает настройки вывода из базы: включённость, порт и подложку.
fn load_settings(state: &State<AppState>) -> Result<ObsConfig, String> {
    let db = state.db.lock().map_err(|error| error.to_string())?;
    let enabled = matches!(
        db::get_setting(&db, ENABLED_KEY).ok().flatten().as_deref(),
        Some("1")
    );
    let port = db::get_setting(&db, PORT_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value >= PORT_MIN)
        .unwrap_or(DEFAULT_PORT);
    let backdrop_enabled = matches!(
        db::get_setting(&db, BACKDROP_ENABLED_KEY)
            .ok()
            .flatten()
            .as_deref(),
        Some("1")
    );
    let backdrop_opacity = db::get_setting(&db, BACKDROP_OPACITY_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<f64>().ok())
        .map(clamp_opacity_percent)
        .unwrap_or(BACKDROP_OPACITY_DEFAULT);
    Ok(ObsConfig {
        enabled,
        port,
        backdrop_enabled,
        backdrop_opacity,
    })
}

/// Приводит непрозрачность подложки из формы к диапазону 0…100 (без дробей).
fn clamp_opacity_percent(value: f64) -> f64 {
    if !value.is_finite() {
        return BACKDROP_OPACITY_DEFAULT;
    }
    value.round().clamp(0.0, 100.0)
}

/// Проверяет порт из формы настроек.
fn validate_port(port: i64) -> Result<u16, String> {
    match u16::try_from(port) {
        Ok(value) if value >= PORT_MIN => Ok(value),
        _ => Err(format!("Порт должен быть числом от {PORT_MIN} до 65535.")),
    }
}

/// Запускает сервер вывода, если он был включён в настройках. Вызывается при
/// старте приложения: сбой запуска не должен мешать работе программы.
pub fn start_from_settings(app: &AppHandle) {
    let state = app.state::<AppState>();
    match load_settings(&state) {
        Ok(config) => {
            if !config.enabled {
                logger::info("obs", "вывод в OBS выключен в настройках");
            }
            state.obs.apply(&config);
        }
        Err(error) => logger::error(
            "obs",
            &format!("не удалось прочитать настройки вывода: {error}"),
        ),
    }
}

/// Настройки и состояние вывода в OBS для вкладки настроек.
#[tauri::command]
pub fn obs_settings(state: State<AppState>) -> Result<ObsSettings, String> {
    let config = load_settings(&state)?;
    Ok(state.obs.view(&config))
}

/// Сохраняет настройки вывода и сразу применяет их (запуск или остановка сервера).
#[tauri::command]
pub fn obs_save_settings(
    enabled: bool,
    port: i64,
    backdrop_enabled: bool,
    backdrop_opacity: f64,
    state: State<AppState>,
) -> Result<ObsSettings, String> {
    let config = ObsConfig {
        enabled,
        port: validate_port(port)?,
        backdrop_enabled,
        backdrop_opacity: clamp_opacity_percent(backdrop_opacity),
    };
    {
        let db = state.db.lock().map_err(|error| error.to_string())?;
        db::set_setting(&db, ENABLED_KEY, if enabled { "1" } else { "0" })
            .map_err(|error| error.to_string())?;
        db::set_setting(&db, PORT_KEY, &config.port.to_string())
            .map_err(|error| error.to_string())?;
        db::set_setting(
            &db,
            BACKDROP_ENABLED_KEY,
            if backdrop_enabled { "1" } else { "0" },
        )
        .map_err(|error| error.to_string())?;
        db::set_setting(
            &db,
            BACKDROP_OPACITY_KEY,
            &format!("{:.0}", config.backdrop_opacity),
        )
        .map_err(|error| error.to_string())?;
    }
    state.obs.apply(&config);
    logger::info(
        "obs",
        &format!(
            "настройки вывода сохранены: включён={enabled}, порт={}, подложка={} {}%",
            config.port, backdrop_enabled, config.backdrop_opacity
        ),
    );
    Ok(state.obs.view(&config))
}

/// Отправляет подписчикам текст текущего слайда — всегда одной строкой. Пустая
/// строка означает очистку: так команда `display:clear` доходит до OBS тем же путём.
#[tauri::command]
pub fn obs_push_slide(text: String, mode: String, state: State<AppState>) {
    state.obs.set_slide(ObsSlide { text, mode });
}

/// Отправляет подписчикам оформление — активный стиль из интерфейса как есть.
#[tauri::command]
pub fn obs_push_style(style: serde_json::Value, state: State<AppState>) {
    state.obs.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_must_be_in_allowed_range() {
        assert_eq!(validate_port(8765).unwrap(), 8765);
        assert_eq!(validate_port(i64::from(PORT_MIN)).unwrap(), PORT_MIN);
        assert!(validate_port(80).is_err());
        assert!(validate_port(0).is_err());
        assert!(validate_port(-1).is_err());
        assert!(validate_port(70000).is_err());
    }

    #[test]
    fn snapshot_carries_single_line_text_in_camel_case() {
        let runtime = ObsRuntime::new();
        runtime.set_slide(ObsSlide {
            text: "Господь — Пастырь мой Ин 3:16".to_string(),
            mode: "bible".to_string(),
        });
        let json = runtime.snapshot_json().unwrap();
        assert!(json.contains("\"text\":\"Господь — Пастырь мой Ин 3:16\""));
        assert!(json.contains("\"mode\":\"bible\""));
        // Подложка по умолчанию — как в прежней версии программы: выключена, 90%.
        assert!(json.contains("\"backdrop\":{\"enabled\":false,\"opacity\":0.9}"));
    }

    #[test]
    fn empty_slide_is_a_clear_command() {
        let runtime = ObsRuntime::new();
        runtime.set_slide(ObsSlide {
            text: "Строка".to_string(),
            mode: "bible".to_string(),
        });
        runtime.set_slide(ObsSlide::default());
        let json = runtime.snapshot_json().unwrap();
        assert!(json.contains("\"text\":\"\""));
    }

    #[test]
    fn backdrop_percent_becomes_fraction() {
        let config = ObsConfig {
            enabled: true,
            port: DEFAULT_PORT,
            backdrop_enabled: true,
            backdrop_opacity: 90.0,
        };
        assert!(config.backdrop().enabled);
        assert!((config.backdrop().opacity - 0.9).abs() < f64::EPSILON);

        let off = ObsConfig {
            backdrop_enabled: false,
            backdrop_opacity: 0.0,
            ..config
        };
        assert_eq!(off.backdrop().opacity, 0.0);
        assert!(!off.backdrop().enabled);
    }

    #[test]
    fn backdrop_opacity_is_clamped_and_rounded() {
        assert_eq!(clamp_opacity_percent(90.4), 90.0);
        assert_eq!(clamp_opacity_percent(150.0), 100.0);
        assert_eq!(clamp_opacity_percent(-20.0), 0.0);
        assert_eq!(clamp_opacity_percent(f64::NAN), BACKDROP_OPACITY_DEFAULT);
    }

    #[test]
    fn overlay_urls_start_with_local_address() {
        let urls = overlay_urls(8765);
        assert_eq!(urls[0], "http://127.0.0.1:8765/");
    }

    /// Свободный порт: система выдаёт его сама, тест не зависит от занятых портов.
    fn free_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// Отправляет запрос и возвращает открытый сокет ответа.
    fn request(port: u16, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        stream
    }

    /// Читает ответ, пока в нём не появится нужная подстрока (или не истечёт таймаут).
    fn read_until(stream: &mut TcpStream, needle: &str) -> String {
        let mut text = String::new();
        let mut buffer = [0u8; 1024];
        for _ in 0..64 {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    text.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    if text.contains(needle) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        text
    }

    #[test]
    fn serves_overlay_page_and_streams_slide() {
        let port = free_port();
        let runtime = Arc::new(ObsRuntime::new());
        runtime.set_slide(ObsSlide {
            text: "Первая строка Ин 3:16".to_string(),
            mode: "bible".to_string(),
        });
        runtime.set_backdrop(ObsBackdrop {
            enabled: true,
            opacity: 0.4,
        });
        runtime.start(port).unwrap();

        let mut page = request(port, "/");
        let html = read_until(&mut page, "</html>");
        assert!(html.starts_with("HTTP/1.1 200 OK"));
        assert!(html.contains("ChyguiSlide"));

        let mut script = request(port, "/app.js");
        let js = read_until(&mut script, "EventSource");
        assert!(js.starts_with("HTTP/1.1 200 OK"));

        // Оверлей сразу получает текущий слайд, а затем — каждое изменение.
        let mut events = request(port, "/events");
        let first = read_until(&mut events, "Первая строка");
        assert!(first.contains("text/event-stream"));
        assert!(first.contains("\"backdrop\":{\"enabled\":true,\"opacity\":0.4}"));

        runtime.set_slide(ObsSlide {
            text: "Вторая строка".to_string(),
            mode: "song".to_string(),
        });
        let next = read_until(&mut events, "Вторая строка");
        assert!(next.contains("Вторая строка"));

        runtime.stop();
    }

    /// Браузер открывает соединения заранее и может прислать запрос с задержкой:
    /// принятое соединение должно дождаться данных. Иначе (ошибка 10035 в Windows,
    /// неблокирующий режим наследуется от слушающего сокета) сервер закрывал
    /// соединение без ответа, и источник «Браузер» в OBS оставался пустым.
    #[test]
    fn waits_for_delayed_request() {
        let port = free_port();
        let runtime = Arc::new(ObsRuntime::new());
        runtime.start(port).unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        thread::sleep(Duration::from_millis(300));
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();

        let html = read_until(&mut stream, "</html>");
        assert!(html.starts_with("HTTP/1.1 200 OK"), "ответ: {html:?}");

        runtime.stop();
    }
}
