import { confirmDialog, invoke } from "../shared/ipc";
import { logError, logInfo } from "../shared/logger";

export type Collection = { id: number; title: string };

export type SongDetail = {
  id: number;
  title: string;
  slides: string[];
  collection_id?: number | null;
};

export type SlideKind = "verse" | "chorus";

export type EditorSlide = {
  kind: SlideKind;
  heading: string;
  body: string;
};

type OpenOptions = {
  mode: "create" | "edit";
  song?: SongDetail | null;
  collections: Collection[];
  preferredCollectionId?: number | null;
  onSaved: (song: SongDetail) => void | Promise<void>;
};

let editId: number | null = null;
let slides: EditorSlide[] = [];
let selectedIndex = 0;
let syncing = false;
let onSavedCb: OpenOptions["onSaved"] | null = null;

function $(sel: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(sel);
  if (!el) {
    throw new Error(`Missing ${sel}`);
  }
  return el;
}

function input(sel: string): HTMLInputElement {
  return $(sel) as HTMLInputElement;
}

function select(sel: string): HTMLSelectElement {
  return $(sel) as HTMLSelectElement;
}

function textarea(sel: string): HTMLTextAreaElement {
  return $(sel) as HTMLTextAreaElement;
}

function dialog(): HTMLDialogElement {
  return $("#song-editor") as HTMLDialogElement;
}

export function parseSlideText(raw: string, index: number): EditorSlide {
  const lines = raw.split("\n");
  const first = (lines[0] || "").trim();
  if (/^припев/i.test(first)) {
    return {
      kind: "chorus",
      heading: first || "Припев",
      body: lines.slice(1).join("\n").trimEnd(),
    };
  }
  if (/^куплет/i.test(first)) {
    return {
      kind: "verse",
      heading: first,
      body: lines.slice(1).join("\n").trimEnd(),
    };
  }
  return {
    kind: "verse",
    heading: `Куплет ${index + 1}`,
    body: raw.trimEnd(),
  };
}

export function serializeSlide(slide: EditorSlide): string {
  const heading = slide.heading.trim() || (slide.kind === "chorus" ? "Припев" : "Куплет");
  const body = slide.body.replace(/\r\n/g, "\n").trimEnd();
  return body ? `${heading}\n${body}` : heading;
}

function nextVerseHeading(): string {
  const n = slides.filter((s) => s.kind === "verse").length + 1;
  return `Куплет ${n}`;
}

function renumberVerseHeadings() {
  let n = 0;
  for (const slide of slides) {
    if (slide.kind !== "verse") {
      continue;
    }
    n += 1;
    if (/^куплет(\s+\d+)?$/i.test(slide.heading.trim()) || !slide.heading.trim()) {
      slide.heading = `Куплет ${n}`;
    }
  }
}

function fillCollections(collections: Collection[], selectedId?: number | null) {
  const sel = select("#se-collection");
  const current = selectedId != null ? String(selectedId) : "";
  sel.replaceChildren();
  const none = document.createElement("option");
  none.value = "";
  none.textContent = "Не выбрано";
  sel.appendChild(none);
  for (const c of collections) {
    const opt = document.createElement("option");
    opt.value = String(c.id);
    opt.textContent = c.title;
    sel.appendChild(opt);
  }
  sel.value = current && [...sel.options].some((o) => o.value === current) ? current : "";
}

function commitPanelToSlide() {
  if (syncing || selectedIndex < 0 || selectedIndex >= slides.length) {
    return;
  }
  const slide = slides[selectedIndex];
  slide.kind = select("#se-slide-type").value === "chorus" ? "chorus" : "verse";
  slide.heading = input("#se-slide-heading").value;
  slide.body = textarea("#se-slide-text").value;
}

function loadPanelFromSlide() {
  syncing = true;
  const slide = slides[selectedIndex];
  if (!slide) {
    select("#se-slide-type").value = "verse";
    input("#se-slide-heading").value = "";
    textarea("#se-slide-text").value = "";
    syncing = false;
    return;
  }
  select("#se-slide-type").value = slide.kind;
  input("#se-slide-heading").value = slide.heading;
  textarea("#se-slide-text").value = slide.body;
  syncing = false;
}

function renderSlideList() {
  const list = $("#se-slide-list");
  list.replaceChildren();
  slides.forEach((slide, index) => {
    const li = document.createElement("li");
    li.className = "editor-slide-item" + (index === selectedIndex ? " selected" : "");
    li.textContent = slide.heading.trim() || (slide.kind === "chorus" ? "Припев" : `Слайд ${index + 1}`);
    li.addEventListener("click", () => {
      commitPanelToSlide();
      selectedIndex = index;
      renderSlideList();
      loadPanelFromSlide();
    });
    list.appendChild(li);
  });
}

function ensureSelection() {
  if (slides.length === 0) {
    selectedIndex = -1;
    return;
  }
  if (selectedIndex < 0) {
    selectedIndex = 0;
  }
  if (selectedIndex >= slides.length) {
    selectedIndex = slides.length - 1;
  }
}

function addSlide() {
  commitPanelToSlide();
  slides.push({
    kind: "verse",
    heading: nextVerseHeading(),
    body: "",
  });
  selectedIndex = slides.length - 1;
  renderSlideList();
  loadPanelFromSlide();
}

function deleteSlide() {
  if (slides.length === 0) {
    return;
  }
  commitPanelToSlide();
  slides.splice(selectedIndex, 1);
  ensureSelection();
  renumberVerseHeadings();
  renderSlideList();
  loadPanelFromSlide();
}

function moveSlide(delta: number) {
  commitPanelToSlide();
  const next = selectedIndex + delta;
  if (selectedIndex < 0 || next < 0 || next >= slides.length) {
    return;
  }
  const tmp = slides[selectedIndex];
  slides[selectedIndex] = slides[next];
  slides[next] = tmp;
  selectedIndex = next;
  renderSlideList();
  loadPanelFromSlide();
}

function chorusAfterVerses() {
  commitPanelToSlide();
  const template =
    (selectedIndex >= 0 && slides[selectedIndex]?.kind === "chorus"
      ? slides[selectedIndex]
      : slides.find((s) => s.kind === "chorus")) || null;
  if (!template) {
    window.alert("Сначала создайте слайд с типом «Припев».");
    return;
  }
  const chorus: EditorSlide = {
    kind: "chorus",
    heading: template.heading.trim() || "Припев",
    body: template.body,
  };
  const verses = slides.filter((s) => s.kind === "verse");
  if (verses.length === 0) {
    window.alert("Нет слайдов с типом «Куплет».");
    return;
  }
  const rebuilt: EditorSlide[] = [];
  for (const verse of verses) {
    rebuilt.push(verse);
    rebuilt.push({ ...chorus });
  }
  slides = rebuilt;
  selectedIndex = Math.min(selectedIndex, slides.length - 1);
  renderSlideList();
  loadPanelFromSlide();
}

function textToOneLine() {
  if (selectedIndex < 0 || !slides[selectedIndex]) {
    return;
  }
  commitPanelToSlide();
  const slide = slides[selectedIndex];
  slide.body = slide.body
    .replace(/\r\n/g, "\n")
    .replace(/\n+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  loadPanelFromSlide();
  renderSlideList();
}

async function save() {
  commitPanelToSlide();
  const title = input("#se-title").value.trim();
  if (!title) {
    window.alert("Введите название песни.");
    return;
  }
  const collectionRaw = select("#se-collection").value;
  if (!collectionRaw) {
    window.alert("Сборник не выбран.");
    return;
  }
  if (slides.length === 0) {
    window.alert("Добавьте хотя бы один слайд.");
    return;
  }

  const numberRaw = input("#se-number").value.trim();
  let number: number | null = null;
  if (numberRaw) {
    number = Number(numberRaw);
    if (!Number.isFinite(number) || number <= 0) {
      window.alert("Номер песни должен быть положительным числом.");
      return;
    }
  }

  try {
    const song = await invoke<SongDetail>("save_song", {
      editId,
      number,
      title,
      collectionId: Number(collectionRaw),
      slides: slides.map(serializeSlide),
    });
    dialog().close();
    if (onSavedCb) {
      await onSavedCb(song);
    }
    logInfo("song", `сохранена песня «${song.title}» (#${song.id})`);
  } catch (error) {
    logError("song", `не удалось сохранить песню «${title}»`, { error: String(error) });
    window.alert(String(error));
  }
}

export function openSongEditor(options: OpenOptions) {
  logInfo(
    "song",
    options.mode === "edit"
      ? `открыт редактор песни «${options.song?.title ?? ""}» (#${options.song?.id ?? 0})`
      : "открыт редактор новой песни",
  );
  editId = options.mode === "edit" && options.song ? options.song.id : null;
  onSavedCb = options.onSaved;

  $("#song-editor-title").textContent =
    options.mode === "edit" ? "Изменить песню" : "Новая песня";

  fillCollections(
    options.collections,
    options.mode === "edit"
      ? options.song?.collection_id
      : options.preferredCollectionId,
  );

  input("#se-title").value = options.song?.title || "";
  input("#se-number").value =
    options.mode === "edit" && options.song ? String(options.song.id) : "";

  if (options.song?.slides?.length) {
    slides = options.song.slides.map((s, i) => parseSlideText(s, i));
  } else {
    slides = [{ kind: "verse", heading: "Куплет 1", body: "" }];
  }
  selectedIndex = 0;
  renderSlideList();
  loadPanelFromSlide();

  const dlg = dialog();
  if (!dlg.open) {
    dlg.showModal();
  }
  input("#se-title").focus();
}

export function closeSongEditor() {
  dialog().close();
}

export function bindSongEditor() {
  $("#song-editor-form").addEventListener("submit", (event) => {
    event.preventDefault();
  });

  select("#se-slide-type").addEventListener("change", () => {
    if (syncing || selectedIndex < 0 || !slides[selectedIndex]) {
      return;
    }
    const slide = slides[selectedIndex];
    const kind = select("#se-slide-type").value === "chorus" ? "chorus" : "verse";
    slide.kind = kind;
    if (kind === "chorus" && !/^припев/i.test(slide.heading)) {
      slide.heading = "Припев";
    } else if (kind === "verse" && (!slide.heading.trim() || /^припев/i.test(slide.heading))) {
      slide.heading = nextVerseHeading();
    }
    input("#se-slide-heading").value = slide.heading;
    renderSlideList();
  });

  input("#se-slide-heading").addEventListener("input", () => {
    commitPanelToSlide();
    renderSlideList();
  });
  textarea("#se-slide-text").addEventListener("input", () => commitPanelToSlide());

  $("#se-add").addEventListener("click", () => addSlide());
  $("#se-delete").addEventListener("click", () => deleteSlide());
  $("#se-up").addEventListener("click", () => moveSlide(-1));
  $("#se-down").addEventListener("click", () => moveSlide(1));
  $("#se-chorus-after").addEventListener("click", () => chorusAfterVerses());
  $("#se-one-line").addEventListener("click", () => textToOneLine());
  $("#se-save").addEventListener("click", () => void save());
  $("#se-close").addEventListener("click", () => closeSongEditor());

  dialog().addEventListener("cancel", (event) => {
    event.preventDefault();
    closeSongEditor();
  });
}

let collectionEditId: number | null = null;
let collectionOnSaved: ((c: Collection) => void | Promise<void>) | null = null;

export function openCollectionEditor(options: {
  mode: "create" | "edit";
  collection?: Collection | null;
  onSaved: (c: Collection) => void | Promise<void>;
}) {
  collectionOnSaved = options.onSaved;
  collectionEditId = options.mode === "edit" && options.collection ? options.collection.id : null;
  logInfo(
    "collection",
    options.mode === "edit"
      ? `открыт редактор сборника «${options.collection?.title ?? ""}»`
      : "открыт редактор нового сборника",
  );
  $("#ce-title").textContent =
    options.mode === "edit" ? "Изменить сборник" : "Новый сборник";
  input("#ce-name").value = options.collection?.title || "";
  const dlg = $("#collection-editor") as HTMLDialogElement;
  dlg.showModal();
  input("#ce-name").focus();
}

type DeleteCollectionOptions = {
  collection: Collection;
  others: Collection[];
  onDeleted: () => void | Promise<void>;
};

let deleteTarget: Collection | null = null;
let deleteOthers: Collection[] = [];
let deleteOnDone: (() => void | Promise<void>) | null = null;

function cdDialog(): HTMLDialogElement {
  return $("#collection-delete") as HTMLDialogElement;
}

function showCdStep(step: "confirm" | "songs" | "move") {
  $("#cd-step-confirm").hidden = step !== "confirm";
  $("#cd-step-songs").hidden = step !== "songs";
  $("#cd-step-move").hidden = step !== "move";
}

export function openCollectionDeleteDialog(options: DeleteCollectionOptions) {
  deleteTarget = options.collection;
  deleteOthers = options.others;
  deleteOnDone = options.onDeleted;
  $("#cd-confirm-text").textContent =
    `Вы точно хотите удалить сборник «${options.collection.title}»?`;
  showCdStep("confirm");
  cdDialog().showModal();
}

async function finishDeleteCollection(deleteSongs: boolean, moveTo: number | null) {
  if (!deleteTarget) {
    return;
  }
  try {
    await invoke("delete_collection", {
      id: deleteTarget.id,
      deleteSongs,
      moveTo,
    });
    logInfo(
      "collection",
      `удалён сборник «${deleteTarget.title}» (удалить песни: ${deleteSongs}, перенос в: ${moveTo ?? "—"})`,
    );
    cdDialog().close();
    if (deleteOnDone) {
      await deleteOnDone();
    }
  } catch (error) {
    logError("collection", "не удалось удалить сборник", { error: String(error) });
    window.alert(String(error));
  }
}

export function bindCollectionEditor() {
  $("#collection-editor-form").addEventListener("submit", (event) => {
    event.preventDefault();
  });
  $("#ce-close").addEventListener("click", () => {
    ($("#collection-editor") as HTMLDialogElement).close();
  });
  $("#ce-save").addEventListener("click", async () => {
    const name = input("#ce-name").value.trim();
    if (!name) {
      window.alert("Введите название сборника.");
      return;
    }
    try {
      const saved =
        collectionEditId != null
          ? await invoke<Collection>("rename_collection", {
              id: collectionEditId,
              name,
            })
          : await invoke<Collection>("create_collection", { name });
      ($("#collection-editor") as HTMLDialogElement).close();
      if (collectionOnSaved) {
        await collectionOnSaved(saved);
      }
    } catch (error) {
      window.alert(String(error));
    }
  });
  ($("#collection-editor") as HTMLDialogElement).addEventListener("cancel", (event) => {
    event.preventDefault();
    ($("#collection-editor") as HTMLDialogElement).close();
  });

  $("#cd-no").addEventListener("click", () => cdDialog().close());
  $("#cd-songs-cancel").addEventListener("click", () => cdDialog().close());
  $("#cd-move-cancel").addEventListener("click", () => cdDialog().close());
  cdDialog().addEventListener("cancel", (event) => {
    event.preventDefault();
    cdDialog().close();
  });

  $("#cd-yes").addEventListener("click", async () => {
    if (!deleteTarget) {
      return;
    }
    let count = 0;
    try {
      count = await invoke<number>("collection_song_count", { id: deleteTarget.id });
    } catch {
      count = 0;
    }
    if (count <= 0) {
      await finishDeleteCollection(true, null);
      return;
    }
    $("#cd-songs-text").textContent =
      `В сборнике «${deleteTarget.title}» песен: ${count}. Что с ними сделать?`;
    showCdStep("songs");
  });

  $("#cd-delete-songs").addEventListener("click", async () => {
    const ok = await confirmDialog(
      "Удалить все песни этого сборника вместе со сборником? Это нельзя отменить.",
      { title: "Удаление сборника", kind: "warning", okLabel: "Удалить", cancelLabel: "Отмена" },
    );
    if (!ok) {
      return;
    }
    void finishDeleteCollection(true, null);
  });

  $("#cd-move-songs").addEventListener("click", () => {
    if (deleteOthers.length === 0) {
      window.alert("Нет другого сборника для переноса. Создайте новый или удалите песни вместе со сборником.");
      return;
    }
    const sel = select("#cd-target");
    sel.replaceChildren();
    for (const c of deleteOthers) {
      const opt = document.createElement("option");
      opt.value = String(c.id);
      opt.textContent = c.title;
      sel.appendChild(opt);
    }
    showCdStep("move");
  });

  $("#cd-move-confirm").addEventListener("click", () => {
    const target = Number(select("#cd-target").value);
    if (!Number.isFinite(target) || target <= 0) {
      window.alert("Выберите сборник для переноса.");
      return;
    }
    void finishDeleteCollection(false, target);
  });
}
