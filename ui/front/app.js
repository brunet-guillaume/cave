// The interface page: calls the Rust commands of ui/src/main.rs.
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const gamesEl = $("games");
const appsEl = $("apps");
const menuEl = $("menu");
const toastEl = $("toast");
const confirmEl = $("confirm");

let lib = { games: [], apps: [], bottles: [] };
// Running bottles, and tiles whose program is open: a bottle can be running
// without all of its programs being open (Steam open, game closed).
let runningBottles = new Set();
let runningTiles = new Set();
const tileKey = (item) => (item.appid !== undefined ? `game:${item.bottle}:${item.appid}` : `app:${item.bottle}:${item.id}`);
// Long-running tasks per bottle: { label, step, kind } (install, uninstall,
// discard, components).
const pending = new Map();

// Page language: that of the messages coming from Rust (the `language`
// command, based on macOS), read at startup. Texts are written in both
// languages where they are used: t("Jouer", "Play").
let fr = true;
const t = (frText, enText) => (fr ? frText : enText);

const graphicsLabel = (g) =>
  ({
    d3dmetal: t("D3DMetal (DirectX 11 et 12)", "D3DMetal (DirectX 11 and 12)"),
    dxmt: t("DXMT (DirectX 10 et 11)", "DXMT (DirectX 10 and 11)"),
    wined3d: t("wined3d (OpenGL, le plus compatible)", "wined3d (OpenGL, most compatible)"),
  })[g] ?? g;

let toastTimer;
function toast(message) {
  toastEl.textContent = message;
  toastEl.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toastEl.hidden = true), 5000);
}

async function call(command, args) {
  try {
    return await invoke(command, args);
  } catch (e) {
    toast(String(e));
    throw e;
  }
}

const bottleOf = (name) => lib.bottles.find((b) => b.name === name);
const hasSteam = () => lib.bottles.some((b) => b.has_steam);

// ——— Tiles ———

function fromTemplate(id) {
  return $(id).content.firstElementChild.cloneNode(true);
}

// First letter or digit of the name, for a missing icon.
const initial = (name) => (name.match(/[\p{L}\p{N}]/u)?.[0] ?? "?").toUpperCase();

function setArt(tile, path, name) {
  const isApp = tile.classList.contains("tile--app");
  const img = tile.querySelector(".tile-art img");
  const fallback = tile.querySelector(".tile-fallback");
  fallback.textContent = isApp ? initial(name) : name;
  if (!path) return img.remove();
  img.src = convertFileSrc(path);
  img.addEventListener("load", () => {
    // Small icon (32, 48 px): scaled up by a whole factor, without blur.
    if (isApp && img.naturalWidth < 64) {
      const size = img.naturalWidth * Math.max(1, Math.floor(96 / img.naturalWidth));
      img.style.width = img.style.height = `${size}px`;
      img.classList.add("is-pixel");
    }
    fallback.hidden = true;
  });
  img.addEventListener("error", () => img.remove());
}

// "Running" state of a tile: the blue "Play" or "Open" button becomes
// "Quit", in grey.
function setRunning(tile, running) {
  tile.classList.toggle("is-running", running);
  const button = tile.querySelector(".tile-launch");
  button.dataset.label ??= button.textContent;
  button.textContent = running ? t("Quitter", "Quit") : button.dataset.label;
  button.classList.toggle("button--primary", !running);
  button.setAttribute("aria-label", `${button.textContent} ${tile.querySelector(".tile-name").textContent}`);
}

// Playtime recorded by Steam: "45 min played", "18 h played".
function playtimeText(minutes) {
  if (!minutes) return "";
  return minutes < 60
    ? t(`${minutes} min de jeu`, `${minutes} min played`)
    : t(`${Math.round(minutes / 60)} h de jeu`, `${Math.round(minutes / 60)} h played`);
}

const dateText = (seconds) =>
  new Date(seconds * 1000).toLocaleDateString(t("fr-FR", "en-US"), { day: "numeric", month: "long", year: "numeric" });

function fillTile(tile, item, ambiguous, launch, menu, quit) {
  tile.dataset.bottle = item.bottle;
  tile.dataset.key = tileKey(item);
  const name = tile.querySelector(".tile-name");
  name.textContent = item.name;
  // When hovering the name: the date it was last used.
  if (item.last_played) name.title = `${item.name}\n${t("Dernière partie :", "Last played:")} ${dateText(item.last_played)}`;
  else if (item.last_launched) name.title = `${item.name}\n${t("Dernière ouverture :", "Last opened:")} ${dateText(item.last_launched)}`;
  else name.title = item.name;
  // Below the name: the bottle if the name is ambiguous, a game's playtime.
  const sub = [ambiguous ? item.bottle : "", playtimeText(item.playtime_minutes)].filter(Boolean).join(" · ");
  tile.querySelector(".tile-sub").textContent = sub;
  tile.classList.toggle("has-sub", Boolean(sub));
  setRunning(tile, runningTiles.has(tile.dataset.key));
  const launchButton = tile.querySelector(".tile-launch");
  const more = tile.querySelector(".tile-more");
  more.setAttribute("aria-label", t(`Options de ${item.name}`, `Options for ${item.name}`));
  const isOpen = () => tile.classList.contains("is-running");
  // A program that is already open is not relaunched by a click on its image
  // (many would open a second window).
  tile.querySelector(".tile-art").addEventListener("click", () => isOpen() || launch());
  launchButton.addEventListener("click", () => (isOpen() ? quit() : launch()));
  const openFromButton = () => {
    const r = more.getBoundingClientRect();
    openMenu(menu(), r.left, r.bottom + 4, more);
  };
  more.addEventListener("click", (e) => {
    e.stopPropagation();
    openFromButton();
  });
  tile.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    openMenu(menu(), e.clientX, e.clientY, more);
  });
  // Menu key or Shift+F10 on one of the tile's buttons.
  tile.addEventListener("keydown", (e) => {
    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
      e.preventDefault();
      openFromButton();
    }
  });
}

function renderGame(g, ambiguous) {
  const tile = fromTemplate("game-tile");
  setArt(tile, g.cover, g.name);
  fillTile(tile, g, ambiguous, () => play(g), () => gameMenu(g), () => quitGame(g));
  return tile;
}

function renderApp(a, ambiguous) {
  const tile = fromTemplate("app-tile");
  setArt(tile, a.icon, a.name);
  fillTile(tile, a, ambiguous, () => launchApp(a), () => appMenu(a), () => quitApp(a));
  return tile;
}

function renderPending([, task]) {
  const tile = fromTemplate("pending-tile");
  tile.querySelector(".tile-name").textContent = task.label;
  tile.querySelector(".tile-name").title = task.label;
  tile.querySelector(".tile-step").textContent = task.step;
  tile.querySelector(".tile-hint").hidden = !["install", "components"].includes(task.kind);
  return tile;
}

// Empty-state block: a title, a sentence, and a button if needed.
function emptyState(title, text, action) {
  const box = document.createElement("div");
  box.className = "empty-state";
  const p = document.createElement("p");
  p.className = "empty-text";
  const strong = document.createElement("strong");
  strong.textContent = title;
  p.append(strong, text);
  box.append(p);
  if (action) {
    const button = document.createElement("button");
    button.className = "button";
    button.textContent = action.label;
    button.addEventListener("click", action.run);
    box.append(button);
  }
  return box;
}

function ambiguity(items) {
  const count = new Map();
  for (const i of items) count.set(i.name, (count.get(i.name) ?? 0) + 1);
  return (item) => count.get(item.name) > 1;
}

const installingSteam = () => [...pending.values()].some((t) => t.label === "Steam" && t.kind === "install");

// Search: ignoring both case and accents.
const searchEl = $("search");
const fold = (text) => text.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLowerCase();
const query = () => searchEl.value.trim();
const matches = (item) => fold(item.name).includes(fold(query()));

// Most recently launched first (last played according to Steam for a game,
// last launched by cave for an application); the others keep the
// alphabetical order, that of the library.
const recent = (item) => item.last_played ?? item.last_launched ?? 0;
const byRecent = (a, b) => recent(b) - recent(a);

function render() {
  // A bottle with work in progress only has its progress tile, except when a
  // program is being installed next to its own (`shared`).
  const busy = (item) => pending.has(item.bottle) && !pending.get(item.bottle).shared;
  const games = lib.games.filter((g) => !busy(g) && matches(g)).sort(byRecent);
  const apps = lib.apps.filter((a) => !busy(a) && matches(a)).sort(byRecent);

  const sameGame = ambiguity(games);
  gamesEl.replaceChildren(...games.map((g) => renderGame(g, sameGame(g))));
  if (!games.length && query()) {
    gamesEl.append(emptyState(t("Aucun jeu trouvé", "No game found"), t(`Aucun jeu ne contient « ${query()} ».`, `No game contains “${query()}”.`)));
  } else if (!games.length) {
    gamesEl.append(hasSteam() || installingSteam()
      ? emptyState(t("Aucun jeu pour l'instant", "No game yet"), t("Installe un jeu depuis Steam : il apparaîtra ici avec sa jaquette.", "Install a game from Steam: it will show up here with its cover."))
      : emptyState(t("Tes jeux Steam apparaîtront ici", "Your Steam games will show up here"), t("Installe Steam, puis tes jeux : chacun se lancera d'un clic.", "Install Steam, then your games: each one starts with a click."), { label: t("Installer Steam", "Install Steam"), run: installSteam }));
  }

  const sameApp = ambiguity(apps);
  appsEl.replaceChildren(...[...pending].map(renderPending), ...apps.map((a) => renderApp(a, sameApp(a))));
  if (!appsEl.children.length && query()) {
    appsEl.append(emptyState(t("Aucune application trouvée", "No application found"), t(`Aucune application ne contient « ${query()} ».`, `No application contains “${query()}”.`)));
  } else if (!appsEl.children.length) {
    appsEl.append(emptyState(t("Aucune application", "No application"), t("Choisis un installateur Windows (.exe ou .msi) : l'application apparaîtra ici avec son icône.", "Choose a Windows installer (.exe or .msi): the application will show up here with its icon."), { label: t("Installer une application…", "Install an Application…"), run: installApp }));
  }

  $("install-steam").hidden = hasSteam() || installingSteam();

  // First launch: without a runtime, nothing can be installed.
  $("setup").hidden = Boolean(lib.runtime);
  for (const id of ["install-app", "install-steam"]) $(id).disabled = !lib.runtime;
  document.querySelectorAll(".empty-state .button").forEach((b) => (b.disabled = !lib.runtime));
}

// Last library received, as text, to tell whether it has changed.
let libraryText = "";

async function refresh() {
  lib = await call("library");
  libraryText = JSON.stringify(lib);
  await updateRunning();
  render();
}

// Rereads the library and only redraws the page if it has changed.
async function refreshIfChanged() {
  const next = await invoke("library").catch(() => null);
  const text = JSON.stringify(next);
  if (!next || text === libraryText) return;
  lib = next;
  libraryText = text;
  render();
}

// Rereads what is running; returns true if a bottle has stopped.
async function updateRunning() {
  const now = await invoke("running").catch(() => null);
  if (!now) return false;
  const bottles = new Set(now.bottles);
  const stopped = [...runningBottles].some((n) => !bottles.has(n));
  runningBottles = bottles;
  runningTiles = new Set([
    ...now.apps.map(([bottle, id]) => tileKey({ bottle, id })),
    ...now.games.map(([bottle, appid]) => tileKey({ bottle, appid })),
  ]);
  return stopped;
}

// While a bottle is running (Steam installing a game…), the library is also
// reread every 4 checks, i.e. every 12 s.
let polls = 0;

async function pollRunning() {
  const stopped = await updateRunning();
  if (!stopped && runningBottles.size && ++polls % 4 === 0) refreshIfChanged();
  document.querySelectorAll(".tile[data-key]").forEach((t) => setRunning(t, runningTiles.has(t.dataset.key)));
  if (stopped) refresh();
}

// ——— Actions ———

async function launchApp(a) {
  await call("launch_app", { bottle: a.bottle, id: a.id });
  toast(`▶ ${a.name}…`);
  setTimeout(pollRunning, 1500);
}

async function quitApp(a) {
  toast(t(`Fermeture de ${a.name}…`, `Closing ${a.name}…`));
  await call("quit_app", { bottle: a.bottle, id: a.id });
  setTimeout(pollRunning, 1500);
}

async function quitGame(g) {
  toast(t(`Fermeture de ${g.name}…`, `Closing ${g.name}…`));
  await call("quit_game", { bottle: g.bottle, appid: g.appid });
  setTimeout(pollRunning, 1500);
}

async function play(g) {
  await call("play", { bottle: g.bottle, appid: g.appid });
  toast(`▶ ${g.name}…`);
  setTimeout(pollRunning, 1500);
}

function graphicsNote(bottle) {
  if (bottleOf(bottle)?.has_steam) return t("pour tous les jeux Steam", "for every Steam game");
  const apps = lib.apps.filter((a) => a.bottle === bottle).map((a) => a.name);
  return apps.length > 1 ? t(`commun à ${apps.join(", ")}`, `shared by ${apps.join(", ")}`) : "";
}

function graphicsItems(bottle) {
  const b = bottleOf(bottle);
  if (!b?.graphics_options.length) return [];
  const note = graphicsNote(bottle);
  return [
    { heading: note ? `${t("Rendu", "Graphics")} (${note})` : t("Rendu", "Graphics") },
    ...b.graphics_options.map((g) => ({
      label: graphicsLabel(g),
      checked: b.graphics === g,
      action: async () => {
        await call("set_graphics", { bottle, graphics: g });
        b.graphics = g;
        toast(t(`Rendu : ${graphicsLabel(g)}. Il s'applique au prochain lancement.`, `Graphics: ${graphicsLabel(g)}. It applies from the next launch.`));
      },
    })),
  ];
}

function bottleItems(bottle) {
  return [
    { label: "Logs", action: () => call("open_folder", { bottle, which: "logs" }) },
    {
      label: t("Forcer à quitter", "Force Quit"),
      disabled: !runningBottles.has(bottle),
      action: async () => {
        await call("kill", { bottle });
        toast(t("Programmes arrêtés", "Programs stopped"));
        setTimeout(pollRunning, 1000);
      },
    },
  ];
}

// Microsoft components (Visual C++, .NET, fonts…) for the bottle of an
// application or a game; those already installed are checked and greyed out.
async function chooseComponents(item) {
  const list = await call("components", { bottle: item.bottle });
  const dialog = $("components");
  dialog.querySelector(".dialog-title").textContent = t(`Composants pour ${item.name}`, `Components for ${item.name}`);
  dialog.querySelector(".component-list").replaceChildren(...list.map((c) => {
    const li = document.createElement("li");
    const label = document.createElement("label");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.value = c.verb;
    box.checked = c.installed;
    box.disabled = c.installed;
    const text = document.createElement("span");
    const name = document.createElement("strong");
    name.textContent = c.installed ? `${c.name} ${t("(installé)", "(installed)")}` : c.name;
    text.append(name, c.description);
    label.append(box, text);
    li.append(label);
    return li;
  }));
  const shared = graphicsNote(item.bottle);
  const notes = [];
  if (shared) notes.push(t(`Ils valent pour toute la bouteille (${shared}).`, `They apply to the whole bottle (${shared}).`));
  if (runningBottles.has(item.bottle)) notes.push(t("Les programmes ouverts seront fermés.", "Open programs will be closed."));
  dialog.querySelector(".dialog-note").textContent = notes.join(" ");
  dialog.returnValue = "";
  dialog.showModal();
  await new Promise((resolve) => dialog.addEventListener("close", resolve, { once: true }));
  const verbs = [...dialog.querySelectorAll("input:checked:not(:disabled)")].map((b) => b.value);
  if (dialog.returnValue !== "ok" || !verbs.length) return;
  pending.set(item.bottle, { label: item.name, step: t("Préparation…", "Preparing…"), kind: "components" });
  render();
  try {
    await call("install_components", { bottle: item.bottle, verbs });
  } catch {
    pending.delete(item.bottle);
    render();
  }
}

// Rename the bottle: its name tells two same-named applications apart.
async function renameBottle(bottle) {
  const dialog = $("rename");
  const input = dialog.querySelector("input");
  dialog.querySelector(".dialog-title").textContent = t(`Renommer la bouteille « ${bottle} »`, `Rename the “${bottle}” bottle`);
  dialog.querySelector(".dialog-note").textContent =
    runningBottles.has(bottle) ? t("Les programmes ouverts seront fermés.", "Open programs will be closed.") : "";
  input.value = bottle;
  dialog.returnValue = "";
  dialog.showModal();
  input.select();
  await new Promise((resolve) => dialog.addEventListener("close", resolve, { once: true }));
  const name = input.value.trim();
  if (dialog.returnValue !== "ok" || !name || name === bottle) return;
  await call("rename_bottle", { bottle, name });
  toast(t(`Bouteille renommée en « ${name} »`, `Bottle renamed to “${name}”`));
  refresh();
}

function appMenu(a) {
  return [
    runningTiles.has(tileKey(a))
      ? { label: t("Quitter", "Quit"), action: () => quitApp(a) }
      : { label: t("Ouvrir", "Open"), action: () => launchApp(a) },
    { separator: true },
    ...graphicsItems(a.bottle),
    { separator: true },
    { label: t("Afficher les fichiers", "Show Files"), action: () => call("open_folder", { bottle: a.bottle, which: "app", id: a.id }) },
    ...bottleItems(a.bottle),
    { separator: true },
    { label: t("Installer dans cette bouteille…", "Install in This Bottle…"), action: () => installApp(a.bottle) },
    { label: t("Installer des composants…", "Install Components…"), action: () => chooseComponents(a) },
    { label: t("Renommer la bouteille…", "Rename Bottle…"), action: () => renameBottle(a.bottle) },
    { label: t("Désinstaller…", "Uninstall…"), danger: true, action: () => uninstall(a) },
  ];
}

function gameMenu(g) {
  return [
    runningTiles.has(tileKey(g))
      ? { label: t("Quitter", "Quit"), action: () => quitGame(g) }
      : { label: t("Jouer", "Play"), action: () => play(g) },
    { separator: true },
    ...graphicsItems(g.bottle),
    { separator: true },
    { label: t("Afficher les fichiers", "Show Files"), action: () => call("open_folder", { bottle: g.bottle, which: "game", id: String(g.appid) }) },
    ...bottleItems(g.bottle),
    { separator: true },
    { label: t("Installer dans cette bouteille…", "Install in This Bottle…"), action: () => installApp(g.bottle) },
    { label: t("Installer des composants…", "Install Components…"), action: () => chooseComponents(g) },
    { label: t("Renommer la bouteille…", "Rename Bottle…"), action: () => renameBottle(g.bottle) },
  ];
}

// What goes away with the application is listed, one item per line.
async function uninstall(a) {
  const others = lib.apps.filter((o) => o.bottle === a.bottle && o.name !== a.name).map((o) => o.name);
  const games = lib.games.filter((g) => g.bottle === a.bottle).map((g) => `${g.name} ${t("(jeu)", "(game)")}`);
  const also = [...others, ...games];
  const ok = await ask({
    title: t(`Désinstaller ${a.name} ?`, `Uninstall ${a.name}?`),
    message: t(`${a.name} et tout ce qui a été installé avec seront supprimés de ce Mac.`, `${a.name} and everything installed with it will be removed from this Mac.`) +
      (also.length ? t(" Sont aussi supprimés :", " Also removed:") : ""),
    list: also,
    note: runningBottles.has(a.bottle) ? t("Les programmes ouverts seront fermés.", "Open programs will be closed.") : "",
    ok: t("Désinstaller", "Uninstall"),
    danger: true,
  });
  if (!ok) return;
  removeBottle(a.bottle, a.name);
}

async function removeBottle(bottle, label, kind = "uninstall") {
  pending.set(bottle, { label, step: t("Désinstallation…", "Uninstalling…"), kind });
  render();
  try {
    await call("uninstall", { bottle });
  } catch {
    pending.delete(bottle);
    render();
  }
}

// ——— Tile menu ———

let menuReturnFocus = null;

function openMenu(items, x, y, returnFocus) {
  menuReturnFocus = returnFocus ?? null;
  menuEl.replaceChildren();
  for (const item of items) {
    if (item.separator) {
      if (menuEl.lastElementChild && menuEl.lastElementChild.tagName !== "HR") {
        const hr = document.createElement("hr");
        hr.className = "menu-separator";
        menuEl.append(hr);
      }
      continue;
    }
    if (item.heading) {
      const p = document.createElement("p");
      p.className = "menu-heading";
      p.textContent = item.heading;
      menuEl.append(p);
      continue;
    }
    const button = document.createElement("button");
    button.className = "menu-item";
    button.setAttribute("role", item.checked !== undefined ? "menuitemradio" : "menuitem");
    if (item.checked !== undefined) button.setAttribute("aria-checked", String(Boolean(item.checked)));
    button.classList.toggle("is-checked", Boolean(item.checked));
    button.classList.toggle("is-danger", Boolean(item.danger));
    button.disabled = Boolean(item.disabled);
    button.textContent = item.label;
    button.addEventListener("click", () => {
      closeMenu();
      Promise.resolve().then(item.action).catch(() => {});
    });
    menuEl.append(button);
  }
  if (menuEl.lastElementChild?.tagName === "HR") menuEl.lastElementChild.remove();

  menuEl.hidden = false;
  const { width, height } = menuEl.getBoundingClientRect();
  menuEl.style.left = `${Math.max(8, Math.min(x, innerWidth - width - 8))}px`;
  menuEl.style.top = `${y + height > innerHeight - 8 ? Math.max(8, y - height) : y}px`;
  menuItems()[0]?.focus();
}

const menuItems = () => [...menuEl.querySelectorAll(".menu-item:not(:disabled)")];

function closeMenu(restoreFocus = false) {
  if (menuEl.hidden) return;
  menuEl.hidden = true;
  if (restoreFocus) menuReturnFocus?.focus();
}

menuEl.addEventListener("keydown", (e) => {
  const items = menuItems();
  const i = items.indexOf(document.activeElement);
  if (e.key === "ArrowDown") items[(i + 1) % items.length]?.focus();
  else if (e.key === "ArrowUp") items[(i - 1 + items.length) % items.length]?.focus();
  else if (e.key === "Home") items[0]?.focus();
  else if (e.key === "End") items[items.length - 1]?.focus();
  else if (e.key === "Escape" || e.key === "Tab") return closeMenu(true);
  else return;
  e.preventDefault();
});

document.addEventListener("mousedown", (e) => {
  if (!menuEl.hidden && !menuEl.contains(e.target)) closeMenu();
});
document.addEventListener("keydown", (e) => e.key === "Escape" && closeMenu(true));
window.addEventListener("blur", () => closeMenu());
window.addEventListener("scroll", () => closeMenu(), true);

// ——— Confirmation dialog ———

function ask({ title, message, list = [], note = "", ok, cancel = t("Annuler", "Cancel"), danger = false }) {
  confirmEl.querySelector(".dialog-title").textContent = title;
  confirmEl.querySelector(".dialog-message").textContent = message;
  confirmEl.querySelector(".dialog-list").replaceChildren(...list.map((text) => {
    const li = document.createElement("li");
    li.textContent = text;
    return li;
  }));
  confirmEl.querySelector(".dialog-note").textContent = note;
  confirmEl.querySelector(".cancel").textContent = cancel;
  const okButton = confirmEl.querySelector(".ok");
  okButton.textContent = ok;
  okButton.classList.toggle("button--danger", danger);
  okButton.classList.toggle("button--primary", !danger);
  confirmEl.returnValue = "";
  confirmEl.showModal();
  // Destructive action: the focus stays on "Cancel".
  (danger ? confirmEl.querySelector(".cancel") : okButton).focus();
  return new Promise((resolve) =>
    confirmEl.addEventListener("close", () => resolve(confirmEl.returnValue === "ok"), { once: true }));
}

// ——— Installations ———

// A new bottle, or that of an application (`into`, a bottle name).
function installApp(into) {
  call("install_app", { into: typeof into === "string" ? into : null });
}

async function installSteam() {
  const bottle = await call("install_steam");
  pending.set(bottle, { label: "Steam", step: t("Préparation de Windows…", "Preparing Windows…"), kind: "install" });
  render();
}

$("install-app").addEventListener("click", installApp);

// ——— Runtime (first launch) ———
// Without a runtime, Cave downloads it as soon as it opens. "Cancel" stops
// the download (what was received is kept: "Download" resumes from there),
// and an archive dropped or chosen meanwhile is installed instead.

let runtimeDownload = null; // download_runtime in progress

// idle: "Choose File…" and "Download"; downloading: "Download" becomes
// "Cancel"; installing: both wait.
function setupState(state) {
  const download = $("download-runtime");
  download.textContent = state === "downloading" ? t("Annuler", "Cancel") : t("Télécharger", "Download");
  download.classList.toggle("button--primary", state !== "downloading");
  download.disabled = $("choose-runtime").disabled = state === "installing";
  if (state === "idle") $("runtime-progress").hidden = $("runtime-status").hidden = true;
}

function setupStatus(text) {
  $("runtime-status").textContent = text;
  $("runtime-status").hidden = $("runtime-progress").hidden = false;
}

const megabytes = (bytes) => `${Math.round(bytes / 1e6)} ${t("Mo", "MB")}`;

// Fraction received, or null when the duration cannot be measured.
function setupProgress(fraction) {
  const bar = $("runtime-progress");
  bar.classList.toggle("is-indeterminate", fraction === null);
  bar.firstElementChild.style.width = fraction === null ? "" : `${fraction * 100}%`;
}

listen("runtime-progress", ({ payload: [step, received, total] }) => {
  if (step === "download") {
    setupProgress(total ? received / total : 0);
    setupStatus(t(`${megabytes(received)} sur ${megabytes(total)}`, `${megabytes(received)} of ${megabytes(total)}`));
  } else {
    // Checksum, then extraction: no measurable progress, nothing to cancel.
    setupState("installing");
    setupProgress(null);
    setupStatus(step === "check" ? t("Vérification de l'empreinte…", "Checking the checksum…") : t("Installation…", "Installing…"));
  }
});

async function downloadRuntime() {
  setupState("downloading");
  setupStatus(t("Recherche du runtime…", "Looking for the runtime…"));
  setupProgress(0);
  runtimeDownload = call("download_runtime");
  try {
    const name = await runtimeDownload; // null if cancelled
    if (name) {
      toast(t(`Runtime ${name} installé ✓`, `Runtime ${name} installed ✓`));
      await refresh();
    }
  } catch {
    // Error already shown by call().
  } finally {
    runtimeDownload = null;
    setupState("idle");
  }
}

// Stops the download, if any, and waits until it has really stopped.
async function stopRuntimeDownload() {
  if (!runtimeDownload) return;
  const download = runtimeDownload;
  await invoke("cancel_runtime_download");
  await download.catch(() => {});
}

async function installRuntimeFile(path) {
  await stopRuntimeDownload();
  setupState("installing");
  setupStatus(t("Installation…", "Installing…"));
  setupProgress(null);
  try {
    const name = await call("install_runtime", { path });
    toast(t(`Runtime ${name} installé ✓`, `Runtime ${name} installed ✓`));
    await refresh();
  } catch {
    // Error already shown by call().
  } finally {
    setupState("idle");
  }
}

$("download-runtime").addEventListener("click", () => (runtimeDownload ? stopRuntimeDownload() : downloadRuntime()));
$("choose-runtime").addEventListener("click", async () => {
  const path = await call("pick_runtime").catch(() => null);
  if (path) installRuntimeFile(path);
});
$("install-steam").addEventListener("click", installSteam);

listen("install-started", ({ payload: [bottle, file, shared] }) => {
  const step = shared ? t("Installation…", "Installing…") : t("Préparation de Windows…", "Preparing Windows…");
  pending.set(bottle, { label: file, step, kind: "install", shared });
  render();
});

listen("bottle-progress", ({ payload: [bottle, step] }) => {
  const task = pending.get(bottle);
  if (!task) return;
  task.step = step;
  render();
});

listen("bottle-done", ({ payload: [bottle, error] }) => {
  const task = pending.get(bottle);
  pending.delete(bottle);
  if (error) toast(task ? t(`${task.label} : ${error}`, `${task.label}: ${error}`) : error);
  else if (task?.kind === "uninstall") toast(t(`${task.label} désinstallé`, `${task.label} uninstalled`));
  else if (task?.kind === "install") toast(t(`${task.label} installé ✓`, `${task.label} installed ✓`));
  else if (task?.kind === "components") toast(t(`${task.label} : composants installés ✓`, `${task.label}: components installed ✓`));
  refresh();
});

listen("install-empty", async ({ payload: [bottle, path, file, isNew] }) => {
  pending.delete(bottle);
  render();
  const keep = await ask({
    title: t("Aucune application trouvée", "No application found"),
    message: t(
      `L'installation de ${file} n'a créé aucun raccourci : elle a peut-être été annulée. ` +
        "S'il s'agit d'un programme qui fonctionne sans installation, Cave peut le garder tel quel.",
      `Installing ${file} created no shortcut: it may have been cancelled. ` +
        "If it is a program that works without installation, Cave can keep it as is.",
    ),
    ok: t("Garder le programme", "Keep the Program"),
    cancel: t("Ne rien garder", "Keep Nothing"),
  });
  if (keep) {
    await call("add_portable", { bottle, path }).catch(() => {});
    refresh();
  } else if (isNew) {
    removeBottle(bottle, file, "discard");
  } else {
    // Existing bottle (that of another application): it stays.
    toast(t("Rien n'a été gardé.", "Nothing was kept."));
    refresh();
  }
});

listen("app-exited", ({ payload: outcome }) => {
  toast(outcome);
  refresh();
});

// ——— Search ———

searchEl.addEventListener("input", render);
searchEl.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  searchEl.value = "";
  searchEl.blur();
  render();
});
// ⌘F puts the cursor in the search field.
document.addEventListener("keydown", (e) => {
  if (e.metaKey && e.key.toLowerCase() === "f") {
    e.preventDefault();
    searchEl.focus();
    searchEl.select();
  }
});

// ——— Drag and drop of an installer onto the window ———

const isInstaller = (path) => /\.(exe|msi)$/i.test(path);
const isRuntime = (path) => /\.tar\.xz$/i.test(path);
const dropLabel = document.querySelector(".drop-label");
// What is being dragged: a runtime, installers, or nothing useful.
let dragging = null;
listen("tauri://drag-enter", ({ payload }) => {
  const paths = payload?.paths ?? [];
  dragging = paths.some(isRuntime) ? "runtime" : paths.some(isInstaller) ? "installer" : null;
  document.body.classList.toggle("is-dropping", Boolean(dragging));
  updateDropLabel();
});
// Dropped on a tile: the installer goes into its bottle. On macOS, the
// position given by Tauri is already in page points (wry takes it from macOS
// without converting it, despite its `PhysicalPosition` name).
let dropTile = null;
function setDropTile(tile) {
  if (dropTile === tile) return;
  dropTile?.classList.remove("is-drop-target");
  dropTile = tile;
  dropTile?.classList.add("is-drop-target");
  document.body.classList.toggle("is-dropping-tile", Boolean(tile));
  updateDropLabel();
}
function updateDropLabel() {
  const name = dropTile?.querySelector(".tile-name")?.textContent;
  dropLabel.textContent = dragging === "runtime"
    ? t("Dépose le runtime pour l'installer", "Drop the runtime to install it")
    : name
      ? t(`Dépose pour l'installer à côté ${of(name)}`, `Drop to install it next to ${name}`)
      : t("Dépose l'installateur pour l'ajouter à Cave", "Drop the installer to add it to Cave");
}
// "d'ILSpy", "de Steam" (French elision before a vowel, not before an h).
const of = (name) => (/^[aeiouyàâäéèêëîïôöùûü]/i.test(name) ? `d'${name}` : `de ${name}`);
function tileAt(position) {
  if (!position) return null;
  const element = document.elementFromPoint(position.x, position.y);
  return element?.closest(".tile[data-bottle]") ?? null;
}
listen("tauri://drag-over", ({ payload }) => {
  // A runtime does not go into a bottle: no tile is targeted.
  if (dragging === "installer") setDropTile(tileAt(payload?.position));
});
listen("tauri://drag-leave", () => {
  document.body.classList.remove("is-dropping");
  setDropTile(null);
});
listen("tauri://drag-drop", ({ payload }) => {
  document.body.classList.remove("is-dropping");
  const into = dropTile?.dataset.bottle ?? null;
  setDropTile(null);
  const runtime = (payload?.paths ?? []).find(isRuntime);
  if (runtime) return installRuntimeFile(runtime);
  if (!lib.runtime) {
    return toast(runtimeDownload
      ? t("Le runtime de Cave est en cours de téléchargement : dépose ton programme ensuite.", "Cave's runtime is downloading: drop your program afterwards.")
      : t("Installe d'abord le runtime de Cave.", "Install Cave's runtime first."));
  }
  const installers = (payload?.paths ?? []).filter(isInstaller);
  if (!installers.length) return toast(t("Dépose un programme Windows (.exe ou .msi) pour l'installer.", "Drop a Windows program (.exe or .msi) to install it."));
  for (const path of installers) call("install_path", { path, into }).catch(() => {});
});

// ——— Miscellaneous ———

// Page links: a single listener, because the translation rewrites the
// paragraphs that contain them.
document.addEventListener("click", (e) => {
  const a = e.target.closest("[data-link]");
  if (!a) return;
  e.preventDefault();
  call("open_link", { link: a.dataset.link });
});

$("refresh").addEventListener("click", refresh);

// Header and footer stay fixed: a thin rule appears when the library scrolls beneath them.
const libraryEl = document.querySelector(".library");
function updateEdges() {
  const { scrollTop, scrollHeight, clientHeight } = libraryEl;
  document.body.classList.toggle("is-scrolled", scrollTop > 0);
  document.body.classList.toggle("has-more", scrollTop + clientHeight < scrollHeight - 1);
}
libraryEl.addEventListener("scroll", updateEdges, { passive: true });
new ResizeObserver(updateEdges).observe(libraryEl);
new MutationObserver(updateEdges).observe(libraryEl, { childList: true, subtree: true });

// Fixed texts of the page (index.html, templates included): their English
// version is in data-en… attributes.
function translatePage() {
  document.documentElement.lang = t("fr", "en");
  if (fr) return;
  const roots = [document, ...[...document.querySelectorAll("template")].map((tpl) => tpl.content)];
  for (const root of roots) {
    root.querySelectorAll("[data-en]").forEach((el) => (el.textContent = el.dataset.en));
    root.querySelectorAll("[data-en-html]").forEach((el) => (el.innerHTML = el.dataset.enHtml));
    root.querySelectorAll("[data-en-placeholder]").forEach((el) => (el.placeholder = el.dataset.enPlaceholder));
    root.querySelectorAll("[data-en-title]").forEach((el) => (el.title = el.dataset.enTitle));
    root.querySelectorAll("[data-en-aria-label]").forEach((el) => el.setAttribute("aria-label", el.dataset.enAriaLabel));
  }
}

async function start() {
  fr = (await invoke("language").catch(() => "fr")) === "fr";
  translatePage();
  const loaded = await refresh().then(() => true, () => false);
  setInterval(pollRunning, 3000);
  if (loaded && !lib.runtime) downloadRuntime();
}
start();
