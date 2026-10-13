//! Graphical interface of cave: a Tauri window whose page (front/) calls the
//! commands below. All the domain logic comes from the cave library, as for
//! the CLI.
//!
//! The page only shows games and applications. Bottles stay behind the
//! scenes: each application is installed in its own, and its settings
//! (renderer…) are those of its bottle.

use cave::bottle::Bottle;
use cave::config::bottles_dir;
use cave::runtime::{Graphics, Runtime};
use cave::processes::Processes;
use cave::runtimes::InstallStep;
use std::sync::atomic::{AtomicBool, Ordering};
use cave::steam::LaunchState;
use cave::{apps, components, games, i18n, runtimes, steam, tr};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

/// Errors reach the page as text.
fn text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
struct BottleView {
    name: String,
    graphics: Option<Graphics>,
    /// Renderers that the bottle's runtime supports.
    graphics_options: Vec<Graphics>,
    has_steam: bool,
    running: bool,
}

#[derive(Serialize)]
struct AppView {
    bottle: String,
    id: String,
    name: String,
    /// Path of the icon, served to the page through the asset: protocol.
    icon: Option<String>,
    is_steam: bool,
    /// Last launch by cave (seconds since 1970).
    last_launched: Option<i64>,
}

#[derive(Serialize)]
struct GameView {
    bottle: String,
    appid: u32,
    name: String,
    /// Path of the cover art, served to the page through the asset: protocol.
    cover: Option<String>,
    /// Last played (seconds since 1970) and playtime, according to Steam.
    last_played: Option<i64>,
    playtime_minutes: u32,
}

#[derive(Serialize, Default)]
struct Library {
    games: Vec<GameView>,
    apps: Vec<AppView>,
    bottles: Vec<BottleView>,
    /// Runtime used for new bottles; None as long as none is installed
    /// (first launch on a Mac).
    runtime: Option<String>,
}

fn path_text(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

/// Everything the page shows: the games and applications of all bottles,
/// and the bottles themselves for the menus (renderer, state).
#[tauri::command]
fn library() -> Result<Library, String> {
    let mut lib = Library {
        runtime: runtimes::default().map_err(text)?.map(|rt| rt.name),
        ..Default::default()
    };
    for b in Bottle::list().map_err(text)? {
        let name = &b.config.name;
        let stats = games::play_stats(&b);
        for g in games::installed(&b).map_err(text)? {
            let played = stats.get(&g.appid).copied().unwrap_or_default();
            lib.games.push(GameView {
                bottle: name.clone(),
                appid: g.appid,
                name: g.name,
                cover: g.cover.map(path_text),
                last_played: (played.last_played > 0).then_some(played.last_played),
                playtime_minutes: played.minutes,
            });
        }
        for a in apps::installed(&b).map_err(text)? {
            lib.apps.push(AppView {
                bottle: name.clone(),
                id: a.id,
                name: a.name,
                icon: a.icon.map(path_text),
                is_steam: a.is_steam,
                last_launched: a.last_launched,
            });
        }
        lib.bottles.push(BottleView {
            graphics_options: graphics_options(&b),
            has_steam: steam::steam_dir(&b).join("steam.exe").is_file(),
            running: b.is_running(),
            graphics: b.config.graphics,
            name: b.config.name,
        });
    }
    lib.games.sort_by_key(|g| g.name.to_lowercase());
    lib.apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(lib)
}

fn graphics_options(b: &Bottle) -> Vec<Graphics> {
    let Ok(wine) = b.wine() else { return vec![] };
    let runtime = Runtime::for_wine(&wine);
    [Graphics::D3dmetal, Graphics::Dxmt, Graphics::Wined3d]
        .into_iter()
        .filter(|g| runtime.env_vars(Some(*g)).is_ok())
        .collect()
}

#[derive(Serialize, Default)]
struct Running {
    /// Bottles whose wineserver is running.
    bottles: Vec<String>,
    /// Open applications: (bottle, id).
    apps: Vec<(String, String)>,
    /// Running games: (bottle, appid). Steam open on its own adds nothing.
    games: Vec<(String, u32)>,
}

/// What is running: the page polls often, since a program can start or stop
/// outside the interface. A bottle can be running without any of its
/// applications being open (Steam alone, lingering Windows services): we look
/// at the programs themselves.
#[tauri::command]
fn running() -> Result<Running, String> {
    let mut out = Running::default();
    let bottles: Vec<Bottle> = Bottle::list().map_err(text)?.into_iter().filter(|b| b.is_running()).collect();
    if bottles.is_empty() {
        return Ok(out);
    }
    let processes = Processes::list().map_err(text)?;
    for b in bottles {
        let name = &b.config.name;
        for a in apps::installed(&b).map_err(text)? {
            if processes.runs(&b, &a.exe) {
                out.apps.push((name.clone(), a.id));
            }
        }
        for g in games::installed(&b).map_err(text)? {
            if processes.runs_from(&b, &g.dir) {
                out.games.push((name.clone(), g.appid));
            }
        }
        out.bottles.push(b.config.name);
    }
    Ok(out)
}

/// Tells the page how a long task (installation…) is progressing.
fn progress(app: &AppHandle, name: &str) -> impl Fn(&str) {
    let (app, name) = (app.clone(), name.to_string());
    move |step: &str| {
        let _ = app.emit("bottle-progress", (&name, step));
    }
}

/// End of a long task: the error, if any, for the page.
fn done(app: &AppHandle, name: &str, result: anyhow::Result<()>) {
    let _ = app.emit("bottle-done", (name, result.err().map(text)));
}

/// Installs a program in a new bottle, or in `into` if given, on a separate
/// thread. The page follows along with "install-started", "bottle-progress",
/// then "bottle-done", or "install-empty" if no application appeared (with,
/// last, true if the bottle is new: only a new one can be deleted).
fn start_install(app: AppHandle, path: PathBuf, into: Option<String>) {
    let name = match into.clone().map_or_else(|| apps::bottle_name_for(&path), Ok) {
        Ok(name) => name,
        Err(e) => return done(&app, "", Err(e)),
    };
    let file_name = path.file_name().map(|f| f.to_string_lossy().into_owned());
    // Last field: the bottle already existed (its tiles stay visible).
    let _ = app.emit("install-started", (&name, &file_name, into.is_some()));
    bring_to_front(&app);
    std::thread::spawn(move || {
        let report = progress(&app, &name);
        let result = match &into {
            Some(bottle) => Bottle::load(bottle).and_then(|b| apps::install_into(&b, &path, &report)),
            None => apps::install(&name, &path, &report).map(|(_, installed)| installed),
        };
        match result {
            Ok(installed) if installed.is_empty() => {
                let _ = app.emit("install-empty", (&name, path_text(path), file_name, into.is_none()));
            }
            Ok(_) => done(&app, &name, Ok(())),
            Err(e) => done(&app, &name, Err(e)),
        }
    });
}

/// Asks for a program to install (.exe or .msi) with the macOS file picker,
/// into a new bottle or into `into`.
#[tauri::command]
fn install_app(app: AppHandle, into: Option<String>) {
    let handle = app.clone();
    let title = match &into {
        Some(bottle) => tr!("Installer dans la bouteille « {bottle} »", "Install in the “{bottle}” bottle"),
        None => tr!("Installer une application", "Install an application"),
    };
    app.dialog()
        .file()
        .set_title(title)
        .add_filter(tr!("Programmes Windows", "Windows programs"), &["exe", "msi"])
        .pick_file(move |file| {
            if let Some(path) = file.and_then(|f| f.into_path().ok()) {
                start_install(handle, path, into);
            }
        });
}

/// Installs a program dropped on the window (into `into` if it was dropped
/// on a tile).
#[tauri::command]
fn install_path(app: AppHandle, path: String, into: Option<String>) -> Result<(), String> {
    let path = PathBuf::from(path);
    let installer = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("msi"));
    if !installer {
        return Err(tr!(
            "seuls les programmes Windows (.exe ou .msi) s'installent",
            "only Windows programs (.exe or .msi) can be installed"
        ));
    }
    start_install(app, path, into);
    Ok(())
}

/// Installs the runtime from its archive (.tar.xz from the releases page).
/// Async: extraction takes a few seconds.
#[tauri::command]
async fn install_runtime(path: String) -> Result<String, String> {
    Ok(runtimes::install_archive(Path::new(&path)).map_err(text)?.name)
}

/// Asks for a runtime archive with the macOS file picker; None if the user
/// cancels. Async: the blocking picker must not run on the main thread.
#[tauri::command]
async fn pick_runtime(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .set_title(tr!("Installer le runtime de Cave", "Install the Cave runtime"))
        .add_filter(tr!("Runtime de Cave", "Cave runtime"), &["xz"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
        .map(path_text)
}

/// Set by "Cancel" in the setup panel, read by the download in progress.
static CANCEL_DOWNLOAD: AtomicBool = AtomicBool::new(false);

/// Sends the steps of a runtime installation to the page ("runtime-progress").
fn emit_progress(app: &AppHandle, step: InstallStep) {
    let (step, received, total) = match step {
        InstallStep::Download { received, total } => ("download", received, total),
        InstallStep::Check => ("check", 0, 0),
        InstallStep::Extract => ("extract", 0, 0),
        InstallStep::Switch => ("switch", 0, 0),
        InstallStep::Prepare => ("prepare", 0, 0),
    };
    let _ = app.emit("runtime-progress", (step, received, total));
}

/// Downloads the latest runtime published with Cave and installs it; the
/// page follows along through "runtime-progress" events. None if cancelled.
#[tauri::command]
async fn download_runtime(app: AppHandle) -> Result<Option<String>, String> {
    CANCEL_DOWNLOAD.store(false, Ordering::Relaxed);
    let rt = runtimes::latest_published().map_err(text)?;
    let installed =
        runtimes::install_published(&rt, &|step| emit_progress(&app, step), &CANCEL_DOWNLOAD).map_err(text)?;
    Ok(installed.map(|rt| rt.name))
}

#[derive(Serialize)]
struct RuntimeUpdate {
    name: String,
    size: u64,
    /// Tag of the release that carries it, for its notes.
    release: String,
}

/// A published runtime more recent than the installed one, if any. Checked
/// when the page opens; offline, there is simply no update.
#[tauri::command]
async fn runtime_update() -> Option<RuntimeUpdate> {
    let rt = runtimes::update_available().ok()??;
    Some(RuntimeUpdate { name: rt.name, size: rt.size, release: rt.release })
}

/// Installs the newer runtime and moves the bottles to it (their open
/// programs are closed: the page asks first). None if cancelled.
#[tauri::command]
async fn update_runtime(app: AppHandle) -> Result<Option<String>, String> {
    CANCEL_DOWNLOAD.store(false, Ordering::Relaxed);
    let Some(rt) = runtimes::update_available().map_err(text)? else {
        return Err(tr!("le runtime est déjà à jour", "the runtime is already up to date"));
    };
    let new = runtimes::update(&rt, &|step| emit_progress(&app, step), &CANCEL_DOWNLOAD).map_err(text)?;
    Ok(new.map(|rt| rt.name))
}

/// Stops the runtime download; what was received stays in the cache, and
/// the next download resumes from there.
#[tauri::command]
fn cancel_runtime_download() {
    CANCEL_DOWNLOAD.store(true, Ordering::Relaxed);
}

#[derive(Serialize)]
struct ComponentView {
    verb: &'static str,
    name: &'static str,
    description: &'static str,
    installed: bool,
}

/// Microsoft components offered for a bottle, and those already installed.
#[tauri::command]
fn components(bottle: String) -> Result<Vec<ComponentView>, String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    let installed = components::installed(&b);
    Ok(components::COMPONENTS
        .iter()
        .map(|c| ComponentView {
            verb: c.verb,
            name: c.name(),
            description: c.description(),
            installed: installed.iter().any(|v| v == c.verb),
        })
        .collect())
}

/// Installs components on a separate thread; the page follows along with
/// "bottle-progress" then "bottle-done".
#[tauri::command]
fn install_components(app: AppHandle, bottle: String, verbs: Vec<String>) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    std::thread::spawn(move || {
        let result = components::install(&b, &verbs, &progress(&app, &bottle));
        done(&app, &bottle, result);
    });
    Ok(())
}

/// Keeps a program that created no shortcut: it is a portable program, added
/// as is to its bottle.
#[tauri::command]
fn add_portable(bottle: String, path: String) -> Result<(), String> {
    let mut b = Bottle::load(&bottle).map_err(text)?;
    apps::add_portable(&mut b, Path::new(&path)).map_err(text)?;
    Ok(())
}

/// Installs Steam in a new bottle, on a separate thread; returns the bottle's
/// name so that the page can follow the progress.
#[tauri::command]
fn install_steam(app: AppHandle) -> Result<String, String> {
    let name = apps::bottle_name_for(Path::new("Steam")).map_err(text)?;
    let bottle = name.clone();
    std::thread::spawn(move || {
        let report = progress(&app, &bottle);
        let result = Bottle::create_ready(&bottle, &report).and_then(|b| {
            // A bottle without Steam would show up nowhere: remove it.
            let installed = steam::install(&b, &report);
            if installed.is_err() {
                let _ = b.delete();
            }
            installed
        });
        done(&app, &bottle, result);
    });
    Ok(name)
}

/// Launches an application on a separate thread: the page is told when it
/// closes by "app-exited".
#[tauri::command]
fn launch_app(app: AppHandle, bottle: String, id: String) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    let target = apps::find(&b, &id).map_err(text)?;
    bring_to_front(&app);
    std::thread::spawn(move || {
        let outcome = match apps::launch(&b, &target, true) {
            Ok(_) => tr!("{} s'est fermé", "{} has closed", target.name),
            Err(e) => tr!("Échec du lancement de {} : {e:#}", "Could not start {}: {e:#}", target.name),
        };
        let _ = app.emit("app-exited", outcome);
    });
    Ok(())
}

/// Asks an open application to close. Async: this means running a program
/// in the bottle, and the window must not freeze.
#[tauri::command]
async fn quit_app(bottle: String, id: String) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    let target = apps::find(&b, &id).map_err(text)?;
    apps::quit(&b, &target).map_err(text)
}

/// Asks a running game to close (Steam stays open).
#[tauri::command]
async fn quit_game(bottle: String, appid: u32) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    let game = games::find(&b, &appid.to_string()).map_err(text)?;
    games::quit(&b, &game).map_err(text)
}

/// Launches a game through Steam, on a separate thread (like launch_app).
#[tauri::command]
fn play(app: AppHandle, bottle: String, appid: u32) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    let game = games::find(&b, &appid.to_string()).map_err(text)?;
    bring_to_front(&app);
    show_steam_questions(&app, steam::LaunchWatch::new(&b, appid), bottle);
    std::thread::spawn(move || {
        let outcome = match games::play(&b, &game, true) {
            Ok(_) => tr!("Steam s'est fermé", "Steam has closed"),
            Err(e) => tr!("Échec du lancement de {} : {e:#}", "Could not start {}: {e:#}", game.name),
        };
        let _ = app.emit("app-exited", outcome);
    });
    Ok(())
}

#[tauri::command]
fn set_graphics(bottle: String, graphics: Graphics) -> Result<(), String> {
    let mut b = Bottle::load(&bottle).map_err(text)?;
    b.set_graphics(graphics).map_err(text)
}

#[tauri::command]
fn kill(bottle: String) -> Result<(), String> {
    Bottle::load(&bottle).map_err(text)?.kill().map_err(text)?;
    Ok(())
}

/// Renames a bottle. Async: if its programs must be stopped first, the
/// window does not freeze meanwhile.
#[tauri::command]
async fn rename_bottle(bottle: String, name: String) -> Result<(), String> {
    let mut b = Bottle::load(&bottle).map_err(text)?;
    b.rename(&name).map_err(text)
}

/// Uninstalls an application by deleting its bottle, on a separate thread
/// (a bottle of games easily weighs several GB).
#[tauri::command]
fn uninstall(app: AppHandle, bottle: String) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    std::thread::spawn(move || done(&app, &bottle, b.delete()));
    Ok(())
}

/// Opens in the Finder the folder of an application, of a game, or the logs
/// of its bottle.
#[tauri::command]
fn open_folder(bottle: String, which: String, id: Option<String>) -> Result<(), String> {
    let b = Bottle::load(&bottle).map_err(text)?;
    let id = id.unwrap_or_default();
    let dir = match which.as_str() {
        "app" => {
            let exe = apps::find(&b, &id).map_err(text)?.exe;
            exe.parent().map(Path::to_path_buf).unwrap_or(exe)
        }
        "game" => games::find(&b, &id).map_err(text)?.dir,
        "logs" => b.logs_dir(),
        other => return Err(tr!("dossier inconnu : {other}", "unknown folder: {other}")),
    };
    Command::new("open").arg(dir).status().map_err(|e| e.to_string())?;
    Ok(())
}

/// Language of the interface: "fr" or "en" (app_language). The page asks for
/// it so as to speak the same language as the messages coming from here.
#[tauri::command]
fn language() -> &'static str {
    match i18n::get() {
        i18n::Lang::Fr => "fr",
        i18n::Lang::En => "en",
    }
}

/// A release tag of Cave: "v" or "wine-cx-", then a version (digits and dots).
fn is_release_tag(tag: &str) -> bool {
    tag.strip_prefix("wine-cx-")
        .or_else(|| tag.strip_prefix('v'))
        .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Opens one of the page's links in the browser: fixed addresses, or a
/// release page of Cave (the page cannot get just any URL opened).
#[tauri::command]
fn open_link(link: String) -> Result<(), String> {
    let url = match link.as_str() {
        "crossover" => "https://www.codeweavers.com/crossover".to_string(),
        "sources" => "https://www.codeweavers.com/crossover/source".to_string(),
        "releases" => "https://github.com/brunet-guillaume/cave/releases".to_string(),
        // A release page ("release:wine-cx-26.10.0", "release:v0.3.0"), with its notes.
        other => match other.strip_prefix("release:") {
            Some(tag) if is_release_tag(tag) => format!("https://github.com/brunet-guillaume/cave/releases/tag/{tag}"),
            _ => return Err(tr!("lien inconnu : {other}", "unknown link: {other}")),
        },
    };
    Command::new("open").arg(url).status().map_err(|e| e.to_string())?;
    Ok(())
}

/// After a launch, each new Wine program that opens a window within the
/// minute comes to the front. Without this, it would stay behind Cave: since
/// macOS 14, an application can no longer bring itself in front of the
/// others, the active application (Cave) has to yield to it. Wine does try,
/// but nobody yields to it.
fn bring_to_front(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let mut seen = front::wine_apps();
        let app = app.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(300));
                for pid in front::wine_apps() {
                    if seen.insert(pid) {
                        let _ = app.run_on_main_thread(move || front::activate(pid));
                    }
                }
            }
        });
    }
}

/// Before launching a game, Steam sometimes asks something in its own window
/// ("better with a controller", a license…), which may be hidden: the game
/// then seems not to start. Its console log tells when it waits for an
/// answer: Cave then has Steam show its window (steam.exe again, like "Open"
/// on its tile) and brings it to the front.
fn show_steam_questions(app: &AppHandle, mut watch: steam::LaunchWatch, bottle: String) {
    use std::time::{Duration, Instant};
    let app = app.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut waiting_since = None;
        let mut shown = false;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
            match watch.poll() {
                LaunchState::Done => return,
                LaunchState::Preparing => (waiting_since, shown) = (None, false),
                LaunchState::WaitingForUser => {
                    // Steam also "waits" for a fraction of a second at every launch.
                    let since = *waiting_since.get_or_insert_with(Instant::now);
                    if !shown && since.elapsed() >= Duration::from_secs(2) {
                        shown = true;
                        if let Ok(b) = Bottle::load(&bottle) {
                            let _ = steam::launch(&b, &[], true);
                        }
                        #[cfg(target_os = "macos")]
                        let _ = app.run_on_main_thread(front::activate_steam);
                    }
                }
            }
        }
    });
}

#[cfg(target_os = "macos")]
mod front {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication,
        NSWorkspace,
    };
    use std::collections::HashSet;

    /// Wine programs that have a window (a spot in the Dock), with the name
    /// of their executable: the Wine launcher, or a link to it named after
    /// them (winetemp-… folder, see runner::run_exe).
    fn wine_programs() -> Vec<(i32, String)> {
        NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .filter_map(|a| {
                let path = a.executableURL()?.path()?.to_string();
                let wine = path.contains("/winetemp-") || path.ends_with("/x86_64-unix/wine");
                let name = path.rsplit('/').next().unwrap_or_default().to_string();
                wine.then(|| (a.processIdentifier(), name))
            })
            .collect()
    }

    pub fn wine_apps() -> HashSet<i32> {
        wine_programs().into_iter().map(|(pid, _)| pid).collect()
    }

    /// Brings Steam to the front: its most recent program with a window,
    /// the webhelper that shows Steam's windows (steam.exe starts it).
    /// Called on the main thread.
    pub fn activate_steam() {
        let steam = wine_programs()
            .into_iter()
            .filter(|(_, name)| name.to_lowercase().starts_with("steam"))
            .map(|(pid, _)| pid)
            .max();
        if let Some(pid) = steam {
            activate(pid);
        }
    }

    /// Brings the program to the front; called on the main thread.
    pub fn activate(pid: i32) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let Some(target) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
            return;
        };
        let options = NSApplicationActivationOptions::ActivateAllWindows;
        if objc2::available!(macos = 14.0) {
            NSApplication::sharedApplication(mtm).yieldActivationToApplication(&target);
            target.activateFromApplication_options(&NSRunningApplication::currentApplication(), options);
        } else {
            target.activateWithOptions(options);
        }
    }
}

/// Dock icon when the interface runs outside a Cave.app (cargo run): macOS
/// only finds an application's icon in its .app bundle, and Tauri only sets
/// it itself with a development server, not with the embedded page we use.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let in_bundle = std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"));
    if in_bundle {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(include_bytes!("../icons/icon.png"));
    if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: called on the main thread (MainThreadMarker), with a valid image.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
    }
}

/// Language of the interface: the first of Cave's preferred languages
/// according to macOS (NSLocale). It follows the system's, unless System
/// Settings → Language & Region gives Cave another one, or when testing with
/// `open Cave.app --args -AppleLanguages '(en)'`.
#[cfg(target_os = "macos")]
fn app_language() -> i18n::Lang {
    let languages = objc2_foundation::NSLocale::preferredLanguages();
    languages.firstObject().map_or(i18n::Lang::En, |code| i18n::of(&code.to_string()))
}

fn main() {
    #[cfg(target_os = "macos")]
    i18n::set(app_language());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Covers and icons live in the bottles: only this folder is served
        // through the asset: protocol (computed here to follow CAVE_HOME).
        .setup(|app| {
            #[cfg(target_os = "macos")]
            set_dock_icon();
            if let Ok(dir) = bottles_dir() {
                app.asset_protocol_scope().allow_directory(dir, true)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library,
            running,
            install_app,
            install_path,
            install_runtime,
            pick_runtime,
            download_runtime,
            cancel_runtime_download,
            runtime_update,
            update_runtime,
            add_portable,
            components,
            install_components,
            install_steam,
            launch_app,
            quit_app,
            quit_game,
            play,
            set_graphics,
            kill,
            rename_bottle,
            uninstall,
            open_folder,
            open_link,
            language
        ])
        .build(tauri::generate_context!())
        .expect("could not start the interface")
        .run(|_, event| {
            // A runtime download (curl) would otherwise go on after Cave quits.
            if let tauri::RunEvent::Exit = event {
                runtimes::stop_download();
            }
        });
}
