//! Импорт текста песен из внешних источников.
//!
//! Две команды для пунктов меню «Песня»:
//!
//! * `parse_presentation` — читает презентацию (`.pptx` или `.odp`). Оба формата
//!   устроены одинаково: это ZIP-архив с XML внутри, поэтому текст достаётся
//!   штатным `zip` без сторонних библиотек. У `.pptx` берутся файлы
//!   `ppt/slides/slideN.xml`, у `.odp` — страницы `<draw:page>` файла
//!   `content.xml`;
//! * `fetch_website_song` — скачивает страницу, вытаскивает из неё текст песни
//!   (контейнер ищется по CSS-селекторам `scraper`: класс или идентификатор со
//!   словом `lyrics`, затем `pre`, `article`, `main`, `body`) и первую строку
//!   названия песни — ею предзаполняется поле «Название» в редакторе.
//!
//! Разметка превращается в текст своей функцией `strip_markup`, а не
//! `ElementRef::text()`: нужны переносы строк от `<br>` и `<p>` — без них куплет
//! склеился бы в одну строку. Там, где текст лежит в `<pre>`, строки разделены
//! переводами строк исходника — их тоже нужно сохранить (см. `Markup`). Заодно
//! одна и та же функция разбирает XML презентаций.
//!
//! Текст в обеих командах — один формат: «слайд, пустая строка, слайд». Во
//! фронтенде (`song-editor.ts`) пустая строка означает новый куплет, поэтому
//! редактор новой песни открывается уже разбитым по слайдам.
//!
//! Обе команды помечены `#[tauri::command(async)]` и работают через
//! `updater::run_blocking`: чтение архива и сеть — блокирующие операции, в
//! главном потоке Tauri они подвесили бы окно (см. комментарий у `run_blocking`).

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Duration;

use encoding_rs::{Encoding, UTF_8};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Serialize;
use url::Url;

use crate::logger;
use crate::updater::run_blocking;

/// Таймаут загрузки страницы: текст песни приходит одним ответом, ждать дольше
/// нечего, а без таймаута окно импорта «висело» бы на недоступном сайте.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Представляемся браузером: сайты с текстами песен часто отвечают отказом на
/// запросы без обычного `User-Agent`.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ChyguiSlide";

/// Предельный размер страницы: дальше это уже каталог песен, а не одна песня.
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

/// Сколько значимых символов должно быть в блоке, чтобы счесть его текстом песни:
/// по этому порогу отбрасываются ссылки и подписи вида «Текст песни».
const MIN_LYRICS_CHARS: usize = 40;

/// Где сайты держат название песни: чаще всего это `<h1>`, а если заголовка нет —
/// `og:title` или `<title>`.
const TITLE_PROBES: [&str; 3] = ["h1", "meta[property=\"og:title\"]", "title"];

/// Название длиннее этого — уже не название, а абзац текста, случайно попавший в
/// заголовок страницы: такой селектор пропускается.
const MAX_TITLE_CHARS: usize = 120;

/// Подсказка для старого формата: `.ppt` — не ZIP, а OLE-контейнер, разбирать
/// его нечем.
const PPT_HINT: &str = "Формат .ppt не поддерживается, сохраните презентацию как .pptx";

/// Контейнеры страницы, где обычно лежит текст песни: от самого точного к общему.
/// Последним идёт `body` — если разметка незнакомая, разбираем всю страницу.
const LYRICS_PROBES: [&str; 10] = [
    "[class*=lyrics]",
    "[class*=Lyrics]",
    "[class*=LYRICS]",
    "[id*=lyrics]",
    "[id*=Lyrics]",
    "[id*=LYRICS]",
    "pre",
    "article",
    "main",
    "body",
];

/// Теги, на границах которых в тексте начинается новая строка.
///
/// Сравнение регистронезависимое, поэтому в одном списке и HTML, и теги
/// презентаций: `<p>`, `<a:p>` и `<text:p>` одинаково означают новый абзац,
/// `<br>`, `<a:br>` и `<text:line-break>` — перенос внутри абзаца.
const LINE_TAGS: [&str; 24] = [
    "br",
    "p",
    "div",
    "li",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "section",
    "article",
    "pre",
    "hr",
    "figure",
    "figcaption",
    "a:br",
    "a:p",
    "text:p",
    "text:line-break",
];

/// Элементы, содержимое которых текстом песни не является: выкидываются вместе
/// с вложенными тегами. Меню, подвал и скрипты иначе попали бы в куплеты.
const DROPPED_TAGS: [&str; 16] = [
    "script",
    "style",
    "noscript",
    "template",
    "svg",
    "iframe",
    "canvas",
    "nav",
    "header",
    "footer",
    "aside",
    "form",
    "button",
    "select",
    "option",
    "textarea",
];

/// Начала строк, с которых на странице начинается её подвал («Поделиться»,
/// «Комментарии» и т. п.). Всё, что идёт дальше, к тексту песни не относится.
const CHROME_MARKERS: [&str; 8] = [
    "поделиться",
    "комментарии",
    "комментарий",
    "оставить комментарий",
    "похожие",
    "ещё песни",
    "другие песни",
    "добавить в плейлист",
];

/// Текст слайдов презентации: `.pptx` и `.odp`.
#[tauri::command(async)]
pub fn parse_presentation(path: String) -> Result<String, String> {
    logger::info("import", &format!("→ parse_presentation: {path}"));
    // `run_blocking` возвращает результат потока, поэтому «вложенный» `Result`
    // разворачиваем: ошибка разбора и сбой потока логируются одинаково.
    let result = run_blocking(move || parse_presentation_file(Path::new(&path)))
        .and_then(|result| result);
    match result {
        Ok(text) => {
            logger::info(
                "import",
                &format!("← parse_presentation: {} символов", text.chars().count()),
            );
            Ok(text)
        }
        Err(error) => {
            logger::error("import", &format!("презентация не разобрана: {error}"));
            Err(error)
        }
    }
}

/// Название песни и её текст со страницы сайта.
#[derive(Debug, Serialize)]
pub struct WebsiteSong {
    /// Первая строка названия песни — ею предзаполняется поле «Название».
    pub title: String,
    /// Текст песни в формате «слайд, пустая строка, слайд».
    pub text: String,
}

/// Название и текст песни со страницы сайта.
#[tauri::command(async)]
pub fn fetch_website_song(url: String) -> Result<WebsiteSong, String> {
    let url = match normalize_url(&url) {
        Ok(url) => url,
        Err(error) => {
            logger::warn("import", &format!("адрес отклонён: {error}"));
            return Err(error);
        }
    };
    logger::info("import", &format!("→ fetch_website_song: {url}"));
    let result = run_blocking(move || fetch_song(&url)).and_then(|result| result);
    match result {
        Ok(song) => {
            logger::info(
                "import",
                &format!(
                    "← fetch_website_song: «{}», {} символов",
                    song.title,
                    song.text.chars().count()
                ),
            );
            Ok(song)
        }
        Err(error) => {
            logger::error("import", &format!("страница не разобрана: {error}"));
            Err(error)
        }
    }
}

/* ——— Презентации ——— */

/// Разбирает презентацию по расширению файла.
fn parse_presentation_file(path: &Path) -> Result<String, String> {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "pptx" => read_pptx(path),
        "odp" => read_odp(path),
        "ppt" => Err(PPT_HINT.to_string()),
        other => Err(format!(
            "Формат .{other} не поддерживается. Выберите файл .pptx или .odp."
        )),
    }
}

/// Текст слайдов `.pptx`: внутри архива это `ppt/slides/slideN.xml`.
fn read_pptx(path: &Path) -> Result<String, String> {
    let mut archive = open_archive(path)?;
    let mut slides = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Не удалось прочитать архив: {error}"))?;
        let Some(number) = pptx_slide_number(entry.name()) else {
            continue;
        };
        let mut xml = String::new();
        entry
            .read_to_string(&mut xml)
            .map_err(|error| format!("Не удалось прочитать слайд {number}: {error}"))?;
        slides.push((
            number,
            slide_lines(&strip_markup(&xml, &DROPPED_TAGS, Markup::PLAIN)),
        ));
    }
    join_slides(slides)
}

/// Текст слайдов `.odp`: страницы `<draw:page>` файла `content.xml`.
fn read_odp(path: &Path) -> Result<String, String> {
    let mut archive = open_archive(path)?;
    let xml = read_archive_entry(&mut archive, "content.xml")?;
    // Заметки докладчика лежат в том же файле и повторяют текст слайда —
    // в песне они не нужны.
    let xml = remove_block(&xml, "presentation:notes");
    let pages = elements(&xml, "draw:page");
    let slides: Vec<(u32, String)> = if pages.is_empty() {
        // Разметки страниц нет — считаем весь документ одним слайдом.
        vec![(
            1,
            slide_lines(&strip_markup(&xml, &DROPPED_TAGS, Markup::PLAIN)),
        )]
    } else {
        pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                (
                    index as u32 + 1,
                    slide_lines(&strip_markup(page, &DROPPED_TAGS, Markup::PLAIN)),
                )
            })
            .collect()
    };
    join_slides(slides)
}

/// Номер слайда из имени файла внутри архива (`ppt/slides/slide12.xml`).
///
/// Файлы связей (`ppt/slides/_rels/slide12.xml.rels`) отсеиваются: приставка не
/// совпадает, поэтому номер не разбирается.
fn pptx_slide_number(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("ppt/slides/slide")?;
    rest.strip_suffix(".xml")?.parse().ok()
}

/// Открывает ZIP-архив презентации.
fn open_archive(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>, String> {
    if !path.is_file() {
        return Err(format!("Файл не найден: {}", path.display()));
    }
    let file = File::open(path).map_err(|error| format!("Не удалось открыть файл: {error}"))?;
    zip::ZipArchive::new(BufReader::new(file))
        .map_err(|error| format!("Файл не является презентацией .pptx/.odp: {error}"))
}

/// Читает текстовый файл из архива (XML презентаций всегда в UTF-8).
fn read_archive_entry(
    archive: &mut zip::ZipArchive<BufReader<File>>,
    name: &str,
) -> Result<String, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| format!("В архиве нет файла {name} — возможно, это не презентация."))?;
    let mut xml = String::new();
    entry
        .read_to_string(&mut xml)
        .map_err(|error| format!("Не удалось прочитать {name}: {error}"))?;
    Ok(xml)
}

/// Склеивает слайды в текст: слайды разделены пустой строкой.
///
/// Пустые слайды (титул без текста, картинки) пропускаются: пустая строка во
/// фронтенде означает новый куплет, а не «пустой куплет».
fn join_slides(mut slides: Vec<(u32, String)>) -> Result<String, String> {
    slides.sort_by_key(|(number, _)| *number);
    let blocks: Vec<String> = slides
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| !text.trim().is_empty())
        .collect();
    if blocks.is_empty() {
        return Err("В презентации не найден текст.".to_string());
    }
    Ok(blocks.join("\n\n"))
}

/// Содержимое всех элементов `name` во фрагменте XML.
///
/// Полноценный XML-парсер здесь избыточен: разметка презентаций машинная и
/// предсказуемая, а вложенных элементов с тем же именем в ней не бывает.
fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut result = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after_name = &rest[start + open.len()..];
        // `<a:t>` и `<a:txBody>` начинаются одинаково — имя должно закончиться.
        if !after_name.starts_with('>')
            && !after_name.starts_with('/')
            && !after_name.starts_with(char::is_whitespace)
        {
            rest = after_name;
            continue;
        }
        let Some(open_end) = after_name.find('>') else {
            break;
        };
        let inner = &after_name[open_end + 1..];
        // Самозакрывающийся элемент (`<text:s/>`) содержимого не имеет.
        if after_name[..open_end].ends_with('/') {
            rest = inner;
            continue;
        }
        let Some(close_at) = inner.find(&close) else {
            break;
        };
        result.push(&inner[..close_at]);
        rest = &inner[close_at + close.len()..];
    }
    result
}

/// Убирает элемент `name` вместе с содержимым.
///
/// Так выкидываются заметки докладчика в `.odp` и служебные блоки страницы
/// (`script`, `style`): их текст к песне не относится.
fn remove_block(fragment: &str, name: &str) -> String {
    let close = format!("</{name}>");
    let mut result = String::with_capacity(fragment.len());
    let mut rest = fragment;
    loop {
        let Some(start) = find_open_tag(rest, name) else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..start]);
        let Some(open_end) = rest[start..].find('>') else {
            break;
        };
        let after_open = start + open_end + 1;
        // Самозакрывающийся элемент (`<text:s/>`) содержимого не имеет.
        if rest[start..after_open].ends_with("/>") {
            rest = &rest[after_open..];
            continue;
        }
        let Some(close_at) = find_ignore_case(&rest[after_open..], &close) else {
            break;
        };
        rest = &rest[after_open + close_at + close.len()..];
    }
    result
}

/// Начало открывающего тега `name` (регистр не важен).
///
/// Проверяется и конец имени: `<header` не должен совпасть с `<headers>`.
fn find_open_tag(haystack: &str, name: &str) -> Option<usize> {
    let needle = format!("<{name}");
    let mut from = 0;
    while let Some(found) = find_ignore_case(&haystack[from..], &needle) {
        let start = from + found;
        let after = &haystack[start + needle.len()..];
        if after.is_empty()
            || after.starts_with('>')
            || after.starts_with('/')
            || after.starts_with(char::is_whitespace)
        {
            return Some(start);
        }
        from = start + needle.len();
    }
    None
}

/// Регистронезависимый поиск подстроки.
///
/// Разметка страниц приходит и в верхнем регистре (`<P>`), а XML презентаций
/// регистрозависим — сравнивать без учёта регистра безопасно для обоих.
fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .char_indices()
        .find(|(index, _)| {
            haystack[*index..]
                .get(..needle.len())
                .is_some_and(|slice| slice.eq_ignore_ascii_case(needle))
        })
        .map(|(index, _)| index)
}

/* ——— Разметка → текст ——— */

/// Как понимать разметку при превращении её в текст.
#[derive(Clone, Copy)]
struct Markup {
    /// Граница абзаца означает границу куплета (страницы, где строки разделены
    /// `<br>`); иначе каждый абзац — отдельная строка (см. `has_line_break`).
    paragraph_is_verse: bool,
    /// Текст лежит в `<pre>`: строки разделены переводами строк исходника, а не
    /// разметкой, поэтому их нельзя заменять пробелами.
    preformatted: bool,
}

impl Markup {
    /// Разметка без особых правил: абзац — строка, переводы строк исходника —
    /// пробелы. Так разбираются презентации.
    const PLAIN: Markup = Markup {
        paragraph_is_verse: false,
        preformatted: false,
    };
}

/// Превращает разметку в текст: теги убираются, границы строк становятся
/// переносами, сущности (`&amp;`, `&#1055;`) декодируются.
fn strip_markup(fragment: &str, dropped: &[&str], markup: Markup) -> String {
    let mut cleaned = fragment.to_string();
    for name in dropped {
        cleaned = remove_block(&cleaned, name);
    }

    let mut text = String::with_capacity(cleaned.len());
    // Начала абзацев, которые ещё не закрыты: по ним видно, был абзац пустым
    // (разделитель куплетов) или содержал строку.
    let mut open_blocks: Vec<usize> = Vec::new();
    let mut rest = cleaned.as_str();
    loop {
        let Some(start) = rest.find('<') else {
            push_text(&mut text, rest, markup);
            break;
        };
        push_text(&mut text, &rest[..start], markup);
        let tail = &rest[start..];
        if let Some(comment) = tail.strip_prefix("<!--") {
            // Комментарий текста не содержит — выкидываем целиком.
            match comment.find("-->") {
                Some(end) => {
                    rest = &comment[end + 3..];
                    continue;
                }
                None => break,
            }
        }
        let Some(end) = tag_end(tail) else {
            // Незакрытый `<` — это уже текст, а не тег.
            text.push_str(tail);
            break;
        };
        let tag = &tail[1..end];
        if is_line_tag(tag) {
            if tag.starts_with('/') {
                text.push('\n');
                if let Some(length) = open_blocks.pop() {
                    if text[length..].trim().is_empty() {
                        // Пустой абзац разделяет куплеты, а не строки: второй
                        // перенос даёт пустую строку между блоками.
                        text.push('\n');
                    }
                }
            } else if is_break_tag(tag) {
                text.push('\n');
            } else {
                open_blocks.push(text.len());
                if markup.paragraph_is_verse {
                    text.push('\n');
                }
            }
        } else if is_space_tag(tag) {
            // `<text:s/>` и `<text:tab/>` заменяют пробелы: без этого слова
            // склеились бы в одно.
            text.push_str(&" ".repeat(space_run(tag)));
        }
        rest = &tail[end + 1..];
    }
    text
}

/// Добавляет к тексту фрагмент между тегами.
///
/// Переводы строк исходника заменяются пробелами: переносы в тексте песни
/// расставляет разметка (`<br>`, абзацы), а не форматирование HTML-файла.
/// Исключение — `<pre>` (`Markup::preformatted`): там строки разделены именно
/// переводами строк исходника, поэтому они сохраняются. Иначе текст сайта
/// вроде hvalite.com (весь текст песни в одном `<pre>`) склеился бы в одну
/// строку, а редактор принял бы её за заголовок слайда.
/// Сущности декодируются сразу — по декодированному тексту видно, был абзац
/// пустым (`<p>&nbsp;</p>`) или содержал строку.
fn push_text(target: &mut String, chunk: &str, markup: Markup) {
    if chunk.is_empty() {
        return;
    }
    if !markup.preformatted {
        target.push_str(&decode_entities(&chunk.replace(['\n', '\r', '\t'], " ")));
        return;
    }
    let text = chunk.replace("\r\n", "\n").replace('\r', "\n");
    // Перенос сразу после тега (`<br>`, `</p>`) — тот же самый перенос, что уже
    // добавлен разметкой, а не пустая строка между куплетами: второй не нужен.
    let text = if target.ends_with('\n') {
        text.strip_prefix('\n').unwrap_or(&text)
    } else {
        &text
    };
    target.push_str(&decode_entities(text));
}

/// Ищет конец тега и не путает его с `>` внутри значения атрибута.
fn tag_end(tail: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, ch) in tail.char_indices() {
        match ch {
            '"' | '\'' if quote == Some(ch) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            '>' if quote.is_none() => return Some(index),
            _ => {}
        }
    }
    None
}

/// Имя тега без `</`, `/>` и атрибутов.
fn tag_name(tag: &str) -> &str {
    tag.trim_start_matches('/')
        .split(|ch: char| ch.is_whitespace() || ch == '/' || ch == '>')
        .next()
        .unwrap_or("")
}

/// Относится ли тег к переносам строк: `<br>`, `</p>`, `<a:p>` и подобные.
fn is_line_tag(tag: &str) -> bool {
    let name = tag_name(tag);
    LINE_TAGS
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// Тег, который сам по себе означает перенос строки внутри абзаца.
fn is_break_tag(tag: &str) -> bool {
    let name = tag_name(tag);
    ["br", "hr", "a:br", "text:line-break"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// Есть ли в разметке перенос строки.
///
/// По этому признаку выбирается, что означают абзацы: там, где строки разделены
/// `<br>` (типовая разметка сайтов с текстами песен), абзац — это куплет;
/// там, где `<br>` нет, каждый абзац — отдельная строка, а куплеты разделяет
/// пустой абзац.
fn has_line_break(fragment: &str) -> bool {
    ["br", "a:br", "text:line-break"]
        .iter()
        .any(|name| find_open_tag(fragment, name).is_some())
}

/// Тег-пробел: `<text:s/>` и `<text:tab/>` в `.odp`, `<a:tab/>` в `.pptx`.
///
/// LibreOffice пишет этими тегами последовательности пробелов и табуляцию, а
/// слова вокруг них разделены только ими — без замены текст склеился бы.
fn is_space_tag(tag: &str) -> bool {
    let name = tag_name(tag);
    ["text:s", "text:tab", "a:tab"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// Сколько пробелов заменяет тег: у `<text:s text:c="3"/>` их три.
///
/// Число ограничено: защита от файла, в котором записано `text:c="999999999"`.
fn space_run(tag: &str) -> usize {
    const SPACE_LIMIT: usize = 64;
    let Some(at) = tag.find("text:c") else {
        return 1;
    };
    let tail = tag[at + "text:c".len()..].trim_start_matches(['=', ' ', '"', '\'']);
    let digits: String = tail.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits
        .parse::<usize>()
        .ok()
        .filter(|count| *count > 0)
        .unwrap_or(1)
        .min(SPACE_LIMIT)
}

/// Строки слайда: пустые убираются, одиночные переносы остаются.
///
/// Внутри слайда пустых строк быть не должно: пустая строка во фронтенде
/// разделяет куплеты, то есть слайды.
fn slide_lines(text: &str) -> String {
    text.split('\n')
        .map(collapse_spaces)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Схлопывает пробелы: в разметке переносы и отступы форматирования лишние.
fn collapse_spaces(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Приводит текст страницы к виду «строка — строка, пустая строка между
/// куплетами».
fn normalize_lyrics(raw: &str) -> String {
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = collapse_spaces(line);
        if line.is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    if let Some(index) = blocks.iter().position(|block| is_chrome_block(block)) {
        blocks.truncate(index);
    }
    blocks
        .iter()
        .map(|block| block.join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Подвал страницы: если блок начинается со «Поделиться», «Комментарии» и
/// подобного, то куплеты закончились, а дальше идут ссылки сайта.
fn is_chrome_block(block: &[String]) -> bool {
    let Some(first) = block.first() else {
        return false;
    };
    let lower = first.to_lowercase();
    CHROME_MARKERS.iter().any(|marker| lower.starts_with(marker))
}

/* ——— Сущности ——— */

/// Декодирует сущности разметки: и именованные, и числовые (`&#1055;`, `&#x41F;`).
fn decode_entities(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        result.push_str(&rest[..index]);
        let tail = &rest[index..];
        // Имя сущности короткое: если за десяток символов `;` не нашёлся, это
        // просто амперсанд в тексте.
        let window_end = tail
            .char_indices()
            .nth(12)
            .map(|(offset, _)| offset)
            .unwrap_or(tail.len());
        let Some(semi) = tail[..window_end].find(';') else {
            result.push('&');
            rest = &tail[1..];
            continue;
        };
        match decode_entity(&tail[1..semi]) {
            Some(ch) => {
                result.push(ch);
                rest = &tail[semi + 1..];
            }
            None => {
                result.push('&');
                rest = &tail[1..];
            }
        }
    }
    result.push_str(rest);
    result
}

/// Значение одной сущности без `&` и `;`.
fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => numeric_entity(entity.strip_prefix('#')?),
    }
}

/// Числовая сущность: `1055` или `x41F`.
fn numeric_entity(code: &str) -> Option<char> {
    let value = match code.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => code.parse::<u32>().ok()?,
    };
    char::from_u32(value)
}

/* ——— Сайты ——— */

/// Приводит адрес к виду, понятному `reqwest`: без схемы подставляется `https`.
///
/// Разрешены только `http` и `https` — остальные схемы (`file:`, `javascript:`)
/// загрузкой страницы не являются.
fn normalize_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Адрес страницы не указан.".to_string());
    }
    match Url::parse(trimmed) {
        Ok(url) => match url.scheme() {
            "http" | "https" => Ok(url.to_string()),
            other => Err(format!(
                "Схема «{other}» не поддерживается — нужен адрес http или https."
            )),
        },
        // Схемы нет (`example.com/song`) — считаем введённое адресом сайта.
        Err(_) => Url::parse(&format!("https://{trimmed}"))
            .map(|url| url.to_string())
            .map_err(|error| format!("Некорректный адрес страницы: {error}")),
    }
}

/// Скачивает страницу и достаёт из неё название и текст песни.
fn fetch_song(url: &str) -> Result<WebsiteSong, String> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| format!("Не удалось создать HTTP-клиент: {error}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("Не удалось открыть страницу: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("Страница вернула ошибку {status}."));
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("Не удалось прочитать страницу: {error}"))?;
    if bytes.len() > MAX_PAGE_BYTES {
        return Err(format!(
            "Страница слишком большая ({} КБ) — импорт из неё не поддерживается.",
            bytes.len() / 1024
        ));
    }
    let html = decode_page(&bytes);
    let text = lyrics_from_html(&html).ok_or_else(|| {
        "На странице не найден текст песни — проверьте адрес и попробуйте другую страницу."
            .to_string()
    })?;
    Ok(WebsiteSong {
        title: song_title_from_html(&html).unwrap_or_default(),
        text,
    })
}

/// Текст песни из разметки страницы: берётся первый достаточно крупный
/// контейнер из `LYRICS_PROBES`.
fn lyrics_from_html(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    for probe in LYRICS_PROBES {
        let Ok(selector) = Selector::parse(probe) else {
            continue;
        };
        for element in document.select(&selector) {
            let inner = element.inner_html();
            // `<pre>` — единственный контейнер, где строки заданы переводами
            // строк исходника, а не тегами: там абзацы ничего не разделяют.
            let preformatted = element.value().name().eq_ignore_ascii_case("pre");
            let markup = Markup {
                paragraph_is_verse: !preformatted && has_line_break(&inner),
                preformatted,
            };
            let text = normalize_lyrics(&strip_markup(&inner, &DROPPED_TAGS, markup));
            if meaningful_chars(&text) >= MIN_LYRICS_CHARS {
                return Some(text);
            }
        }
    }
    None
}

/// Название песни из разметки: сначала `<h1>` — сайты держат имя песни там,
/// затем `og:title`, затем `<title>` без названия сайта.
fn song_title_from_html(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    for probe in TITLE_PROBES {
        let Ok(selector) = Selector::parse(probe) else {
            continue;
        };
        let Some(element) = document.select(&selector).next() else {
            continue;
        };
        // У `<meta>` название лежит в атрибуте `content`, у остальных — в тексте.
        let raw = match element.value().attr("content") {
            Some(content) => content.to_string(),
            None => element.text().collect::<Vec<_>>().join(" "),
        };
        // `<title>` склеивает название песни с названием сайта — его отбрасываем.
        let title = if element.value().name().eq_ignore_ascii_case("title") {
            strip_site_title(&title_line(&raw))
        } else {
            title_line(&raw)
        };
        if !title.is_empty() && title.chars().count() <= MAX_TITLE_CHARS {
            return Some(title);
        }
    }
    None
}

/// Первая строка названия: в разметке название может переноситься по строкам.
fn title_line(raw: &str) -> String {
    raw.replace("\r\n", "\n")
        .split('\n')
        .map(collapse_spaces)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// Отбрасывает хвост `<title>` с названием сайта: «Песня | HVALITE.COM»,
/// «Песня — текст песни, слова» и подобное.
fn strip_site_title(line: &str) -> String {
    for (index, ch) in line.char_indices() {
        let separator = match ch {
            '|' | '·' | '•' | '»' | '—' | '–' => true,
            // Дефис разделяет название сайта только с пробелами по бокам.
            '-' => index > 0 && line[..index].ends_with(' ') && line[index + 1..].starts_with(' '),
            _ => false,
        };
        if separator {
            return collapse_spaces(&line[..index]);
        }
    }
    line.to_string()
}

/// Значимые символы — без пробелов: по ним контейнер отличается от ссылки.
fn meaningful_chars(text: &str) -> usize {
    text.chars().filter(|ch| !ch.is_whitespace()).count()
}

/// Переводит байты страницы в строку с учётом объявленной кодировки.
///
/// `response.text()` у `reqwest` смотрит только на заголовок `Content-Type`, а
/// сайты с текстами песен до сих пор объявляют кодировку в `<meta>`
/// (`windows-1251`) — без этой проверки русский текст пришёл бы мусором.
fn decode_page(bytes: &[u8]) -> String {
    let encoding = charset_label(bytes)
        .and_then(|label| Encoding::for_label(label.as_bytes()))
        .unwrap_or(UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Кодировка из первых килобайт разметки: `<meta charset="windows-1251">` или
/// `content="text/html; charset=…"`.
fn charset_label(bytes: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let index = head.find("charset")?;
    let tail = head[index + "charset".len()..].trim_start_matches(['=', ' ', '"', '\'', ':', ';']);
    let label: String = tail
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_' || *ch == '.')
        .collect();
    (!label.is_empty()).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Складывает файлы в ZIP — так проверяются настоящие `.pptx` и `.odp`.
    fn write_archive(path: &Path, files: &[(&str, &str)]) {
        let file = File::create(path).expect("архив создаётся");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in files {
            zip.start_file(*name, options).expect("файл открывается");
            zip.write_all(content.as_bytes()).expect("файл пишется");
        }
        zip.finish().expect("архив закрывается");
    }

    /// Каталог для временных файлов теста (после проверки удаляется).
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("каталог создаётся");
        dir
    }

    /// Слайд `.pptx`: абзацы становятся строками, слайды разделяются пустой
    /// строкой — именно так редактор понимает куплеты.
    #[test]
    fn pptx_slides_become_verses() {
        let dir = temp_dir("chyguislide-importer-pptx");
        let path = dir.join("Песня.pptx");
        write_archive(
            &path,
            &[
                (
                    "ppt/slides/slide1.xml",
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<p:cSld><p:spTree><p:sp><p:txBody><a:bodyPr/><a:lstStyle/>
<a:p><a:r><a:t>Великий Бог</a:t></a:r></a:p>
<a:p><a:r><a:t>как Ты &amp; велик</a:t></a:r></a:p>
</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
                ),
                (
                    "ppt/slides/slide2.xml",
                    r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<p:cSld><p:spTree><p:sp><p:txBody>
<a:p><a:r><a:t>Припев</a:t></a:r></a:p>
<a:p><a:r><a:t>Ты достоин</a:t></a:r></a:p>
</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
                ),
                // Файл связей слайдом не считается.
                ("ppt/slides/_rels/slide1.xml.rels", "<Relationships/>"),
            ],
        );

        let text = parse_presentation_file(&path).expect("презентация разбирается");
        assert_eq!(
            text, "Великий Бог\nкак Ты & велик\n\nПрипев\nТы достоин",
            "абзацы — строки, слайды — куплеты, сущности декодированы"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Страницы `.odp` разбираются из `content.xml`, а заметки докладчика в
    /// текст песни не попадают.
    #[test]
    fn odp_pages_become_verses() {
        let dir = temp_dir("chyguislide-importer-odp");
        let path = dir.join("Песня.odp");
        write_archive(
            &path,
            &[(
                "content.xml",
                r#"<office:document-content xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
<office:body><office:presentation>
<draw:page draw:name="page1">
<draw:frame><draw:text-box>
<text:p>Куплет 1</text:p>
<text:p>Первая строка<text:line-break/>Вторая строка</text:p>
<text:p>Хвалы<text:s text:c="3"/>и славы</text:p>
</draw:text-box></draw:frame>
<presentation:notes><draw:frame><draw:text-box>
<text:p>Заметка докладчика</text:p>
</draw:text-box></draw:frame></presentation:notes>
</draw:page>
<draw:page draw:name="page2">
<draw:frame><draw:text-box><text:p>Припев</text:p></draw:text-box></draw:frame>
</draw:page>
</office:presentation></office:body></office:document-content>"#,
            )],
        );

        let text = parse_presentation_file(&path).expect("презентация разбирается");
        assert_eq!(
            text,
            "Куплет 1\nПервая строка\nВторая строка\nХвалы и славы\n\nПрипев",
            "страницы — куплеты, заметки докладчика не попадают в текст"
        );
        assert!(!text.contains("Заметка"), "заметки докладчика пропускаются");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Старый формат `.ppt` — не ZIP: вместо непонятной ошибки пользователь
    /// получает подсказку, что делать.
    #[test]
    fn old_ppt_format_reports_hint() {
        let dir = temp_dir("chyguislide-importer-ppt");
        let path = dir.join("Песня.ppt");
        std::fs::write(&path, b"PK").expect("файл пишется");

        let error = parse_presentation_file(&path).expect_err("старый формат не разбирается");
        assert_eq!(error, PPT_HINT);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Теги-пробелы `.odp`: `<text:s text:c="3"/>` — три пробела, иначе слова
    /// склеились бы в одно.
    #[test]
    fn odp_space_tags_keep_words_apart() {
        assert_eq!(space_run("text:s/"), 1);
        assert_eq!(space_run(r#"text:s text:c="3"/"#), 3);
        // Абсурдное значение из файла не превращается в мегабайты пробелов.
        assert_eq!(space_run(r#"text:s text:c="999999999"/"#), 64);
        assert_eq!(space_run(r#"text:s text:c="0"/"#), 1);
    }

    /// Разметка страницы: строки из `<br>` остаются строками, а пустая строка
    /// разделяет куплеты — ровно то, что ждёт редактор.
    #[test]
    fn website_lyrics_keep_line_breaks() {
        let html = r#"<!DOCTYPE html><html><head><meta charset="utf-8">
<title>Песня</title><style>.x{color:red}</style></head><body>
<header><nav>Меню сайта</nav></header>
<div class="lyrics">
Великий Бог, Ты &amp; велик<br>
Ты достоин хвалы<br>
<br>
Припев:<br>
Аллилуйя<br>
</div>
<footer><p>Поделиться</p><p>Комментарии</p></footer>
</body></html>"#;

        let text = lyrics_from_html(html).expect("текст песни находится");
        assert_eq!(
            text,
            "Великий Бог, Ты & велик\nТы достоин хвалы\n\nПрипев:\nАллилуйя"
        );
        assert!(!text.contains("Меню сайта"), "меню сайта пропускается");
    }

    /// Разметка с абзацем на каждую строку: пустой абзац разделяет куплеты,
    /// поэтому стихи не склеиваются в один слайд.
    #[test]
    fn website_lyrics_use_empty_paragraphs_as_separators() {
        let html = r#"<html><body>
<div class="lyrics">
<p>Великий Бог, как Ты велик</p><p>Ты достоин хвалы и славы</p>
<p>&nbsp;</p>
<p>Припев:</p><p>Аллилуйя, аллилуйя</p>
</div></body></html>"#;

        let text = lyrics_from_html(html).expect("текст песни находится");
        assert_eq!(
            text, "Великий Бог, как Ты велик\nТы достоин хвалы и славы\n\nПрипев:\nАллилуйя, аллилуйя",
            "пустой абзац разделяет куплеты"
        );
    }

    /// Текст песни в одном `<pre>` (hvalite.com и подобные сайты): строки там
    /// разделены переводами строк исходника, а не `<br>` или абзацами. Раньше
    /// такой текст склеивался в одну строку, и редактор показывал в заголовке
    /// куплет целиком.
    #[test]
    fn website_lyrics_from_pre_tag() {
        let html = concat!(
            "<html><body>\r\n",
            "<header><nav>Меню сайта</nav></header>\r\n",
            "<pre id=\"music_text\" class=\"\">",
            "Куплет 1:\r\n",
            "Слушайте повести любви в простоте,\r\n",
            "Слушайте дивный рассказ; \r\n",
            "\r\n",
            "Припев:\r\n",
            "Бог нас от гибели спас!\r\n",
            "</pre>\r\n",
            "</body></html>",
        );

        let text = lyrics_from_html(html).expect("текст песни находится");
        assert_eq!(
            text,
            "Куплет 1:\nСлушайте повести любви в простоте,\nСлушайте дивный рассказ;\n\nПрипев:\nБог нас от гибели спас!"
        );
        assert!(!text.contains("Меню сайта"), "меню сайта пропускается");
    }

    /// Внутри `<pre>` перенос строки задан и разметкой, и переводом строки
    /// исходника: второй перенос — тот же самый, а пустая строка между
    /// куплетами сохраняется.
    #[test]
    fn pre_tag_keeps_lines_with_br() {
        let html = concat!(
            "<html><body><pre id=\"music_text\">",
            "Первая строка песни<br>\n",
            "Вторая строка песни<br><br>\n",
            "\n",
            "Третья строка песни",
            "</pre></body></html>",
        );

        let text = lyrics_from_html(html).expect("текст песни находится");
        assert_eq!(
            text,
            "Первая строка песни\nВторая строка песни\n\nТретья строка песни"
        );
    }

    /// Название песни берётся из `<h1>`: в `<title>` сайта рядом с ним стоят
    /// сборник, слова «текст песни» и адрес сайта — в поле «Название» они не нужны.
    #[test]
    fn website_title_comes_from_h1() {
        let html = concat!(
            "<html><head><meta charset=\"utf-8\"><title>",
            "Слушайте повесть любви в простоте | Песнь возрождения №1 | ",
            "слова, текст песни с аккордами, ноты | HVALITE.COM",
            "</title></head><body>",
            "<div class=\"songs song\">",
            "<h1>Слушайте повесть любви в простоте</h1>",
            "<h2 class=\"second_name\">Песнь возрождения №1</h2>",
            "<pre id=\"music_text\">Куплет 1:\nСлушайте повесть любви в простоте</pre>",
            "</div></body></html>",
        );

        assert_eq!(
            song_title_from_html(html).as_deref(),
            Some("Слушайте повесть любви в простоте")
        );
    }

    /// Заголовка `<h1>` на странице нет — название берётся из `<title>` до
    /// разделителя с названием сайта, а первая строка отбрасывает остальное.
    #[test]
    fn website_title_falls_back_to_title_tag() {
        let html = concat!(
            "<html><head><title>Великий Бог — текст песни | example.com</title>",
            "</head><body><div class=\"lyrics\">Великий Бог, как Ты велик</div>",
            "</body></html>",
        );

        assert_eq!(
            song_title_from_html(html).as_deref(),
            Some("Великий Бог"),
            "название сайта в название песни не попадает"
        );
    }

    /// В `<h1>` может лежать абзац текста вместо названия: такой заголовок
    /// пропускается, и название берётся из `<title>`.
    #[test]
    fn website_title_skips_long_heading() {
        let long = "длинный заголовок вместо названия ".repeat(5);
        let html = format!(
            "<html><head><title>Великий Бог | example.com</title></head><body><h1>{long}</h1></body></html>"
        );

        assert_eq!(song_title_from_html(&html).as_deref(), Some("Великий Бог"));
    }

    /// Страница без текста песни распознаётся как таковая: фронтенд покажет
    /// понятное сообщение вместо пустого редактора.
    #[test]
    fn website_without_lyrics_is_reported() {
        let html = "<html><body><h1>Ошибка 404</h1></body></html>";
        assert!(lyrics_from_html(html).is_none());
    }

    /// Числовые сущности и `nbsp`: ими на сайтах набраны тире и пробелы.
    #[test]
    fn entities_are_decoded() {
        assert_eq!(
            decode_entities("Ты &#1055;&#x440;&nbsp;велик"),
            "Ты Пр велик"
        );
        assert_eq!(decode_entities("Мир &amp; любовь"), "Мир & любовь");
        // Одинокий амперсанд остаётся как есть.
        assert_eq!(decode_entities("Рок & ролл"), "Рок & ролл");
    }

    /// Адрес без схемы получает `https`, а чужие схемы отклоняются: загрузка
    /// страницы — это только `http`/`https`.
    #[test]
    fn url_normalization_works() {
        assert_eq!(
            normalize_url("  example.com/song  ").expect("адрес дополняется"),
            "https://example.com/song"
        );
        assert_eq!(
            normalize_url("http://example.com").expect("адрес принимается"),
            "http://example.com/"
        );
        assert!(normalize_url("   ").is_err());
        assert!(normalize_url("file:///C:/Windows/win.ini").is_err());
        assert!(normalize_url("javascript:alert(1)").is_err());
    }

    /// Кодировка страницы берётся из `<meta>`: без этого текст сайтов с
    /// `windows-1251` приходил бы мусором.
    #[test]
    fn page_encoding_is_detected() {
        let mut html = br#"<html><head><meta charset="windows-1251"></head><body>"#.to_vec();
        html.extend_from_slice(b"\xcf\xf0\xe8\xe2\xe5\xf2");
        html.extend_from_slice(b"</body></html>");
        assert_eq!(charset_label(&html).as_deref(), Some("windows-1251"));
        assert!(decode_page(&html).contains("Привет"));

        // Кодировка не объявлена — считаем UTF-8.
        let utf8 = "<html><body>Привет</body></html>".as_bytes();
        assert_eq!(decode_page(utf8), "<html><body>Привет</body></html>");
    }
}
