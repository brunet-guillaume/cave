//! Applications installed in a bottle, found the way Windows finds them:
//! through the shortcuts (.lnk) that installers put in the Start menu and
//! on the Desktop. Added to these are "portable" programs, with neither
//! installer nor shortcut, which cave records in the bottle's config.
//!
//! For the interface, one application = one bottle: each is installed in
//! its own (create_ready clones it from a template, at no cost), and its
//! settings, such as the renderer, apply to that application alone.

use crate::bottle::Bottle;
use crate::processes::{self, Processes};
use crate::runner::{run_exe, run_in_bottle, RunResult};
use crate::tr;
use crate::{icon, lnk, steam};
use anyhow::{bail, Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub struct App {
    /// Stable identifier within the bottle: the shortcut's path, or
    /// `cave:<name>` for a portable program.
    pub id: String,
    pub name: String,
    /// The .exe, as a macOS path.
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// Icon extracted from the program, kept in <bottle>/icons.
    pub icon: Option<PathBuf>,
    /// Steam is launched its own way (shim, -noverifyfiles: see steam.rs).
    pub is_steam: bool,
    /// Last launch by cave, in seconds since 1970.
    pub last_launched: Option<i64>,
}

const START_MENU: &str = "Microsoft/Windows/Start Menu/Programs";

/// The bottle's applications, sorted by name.
pub fn installed(bottle: &Bottle) -> Result<Vec<App>> {
    let mut apps = vec![];
    // The same program often has one shortcut in the Start menu and one on
    // the Desktop: the first one found (the menu's) wins.
    let mut seen = HashSet::new();
    for dir in shortcut_dirs(bottle) {
        let mut shortcuts = vec![];
        collect_shortcuts(&dir, 0, &mut shortcuts);
        shortcuts.sort();
        for path in shortcuts {
            if let Some(app) = from_shortcut(bottle, &path) {
                if seen.insert((app.exe.to_string_lossy().to_lowercase(), app.args.clone())) {
                    apps.push(app);
                }
            }
        }
    }
    for (name, windows) in &bottle.config.apps {
        let Some(exe) = bottle.unix_path(windows) else { continue };
        apps.push(App {
            id: format!("cave:{name}"),
            name: name.clone(),
            icon: cached_icon(bottle, &exe, 0),
            is_steam: false,
            args: vec![],
            exe,
            last_launched: None,
        });
    }
    for app in &mut apps {
        app.last_launched = bottle.config.launched.get(&app.id).copied();
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}

/// Finds an application by identifier or by name (ignoring case).
pub fn find(bottle: &Bottle, query: &str) -> Result<App> {
    let mut apps = installed(bottle)?;
    if let Some(i) = apps.iter().position(|a| a.id == query) {
        return Ok(apps.swap_remove(i));
    }
    let q = query.to_lowercase();
    let mut matches: Vec<App> = apps
        .into_iter()
        .filter(|a| a.name.to_lowercase().contains(&q))
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!(tr!(
            "aucune application ne correspond à « {query} » (cave apps {})",
            "no application matches “{query}” (cave apps {})",
            bottle.config.name
        )),
        _ => bail!(tr!(
            "plusieurs applications correspondent à « {query} » : {}",
            "several applications match “{query}”: {}",
            matches.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Launches the application; returns when it closes.
pub fn launch(bottle: &Bottle, app: &App, quiet: bool) -> Result<RunResult> {
    note_launch(bottle, &app.id);
    if app.is_steam {
        return steam::launch(bottle, &app.args, quiet);
    }
    // In the Dock, the tile's name rather than the .exe's.
    let names: Vec<(String, String)> = bottle
        .windows_path(&app.exe)
        .map(|exe| (exe, app.name.clone()))
        .into_iter()
        .collect();
    run_exe(&bottle.wine()?, bottle, &app.exe, &app.args, &names, quiet)
}

/// Records the launch time, to sort applications by recent use.
fn note_launch(bottle: &Bottle, id: &str) {
    // Config reread just before: it may have changed since `bottle` was loaded.
    if let Ok(mut fresh) = Bottle::load(&bottle.config.name) {
        fresh.config.launched.insert(id.to_string(), chrono::Local::now().timestamp());
        let _ = fresh.save();
    }
}

/// Asks the application to close (processes::close); Steam has its own
/// command.
pub fn quit(bottle: &Bottle, app: &App) -> Result<()> {
    // Nothing to close; and `steam.exe -shutdown` would start Steam.
    if !bottle.is_running() || !Processes::list()?.runs(bottle, &app.exe) {
        bail!(tr!("{} n'est pas ouvert", "{} is not open", app.name));
    }
    if !app.is_steam {
        return processes::close(bottle, &file_name(&app.exe));
    }
    // Steam already open: this second steam.exe passes the order on and exits.
    let res = run_in_bottle(&bottle.wine()?, bottle, &app.exe.to_string_lossy(), &["-shutdown".into()], true)?;
    if !res.status.success() {
        bail!(tr!(
            "Steam n'a pas pu être fermé ; essaie « Forcer à quitter » (voir {})",
            "Steam could not be closed; try “Force Quit” (see {})",
            res.log_file.display()
        ));
    }
    Ok(())
}

/// Installs a program in a new ready-to-use bottle: runs the installer
/// (.exe, or .msi via msiexec) and waits for it to finish. Returns the
/// applications found afterwards. None means that the installation was
/// cancelled, or that the program is portable (see add_portable): the
/// bottle is kept, and it is up to the caller to decide.
pub fn install(name: &str, installer: &Path, progress: &dyn Fn(&str)) -> Result<(Bottle, Vec<App>)> {
    if !installer.is_file() {
        bail!(tr!("{} introuvable", "{} not found", installer.display()));
    }
    let bottle = Bottle::create_ready(name, progress)?;
    match run_installer(&bottle, installer, &HashSet::new(), None, progress) {
        Ok(apps) => Ok((bottle, apps)),
        Err(e) => {
            let _ = bottle.delete();
            Err(e)
        }
    }
}

/// Installs a program in an existing bottle, next to those already there:
/// a plugin, a modding tool, a launcher… Returns the applications that
/// appeared; none: installation cancelled, or portable program (the
/// bottle itself is kept).
pub fn install_into(bottle: &Bottle, installer: &Path, progress: &dyn Fn(&str)) -> Result<Vec<App>> {
    if !installer.is_file() {
        bail!(tr!("{} introuvable", "{} not found", installer.display()));
    }
    let known: HashSet<String> = installed(bottle)?.into_iter().map(|a| a.id).collect();
    // If the bottle is already running (Steam open…), its stopping won't
    // tell us the installation is done: we'll follow the programs that
    // appear from now on.
    let before = if bottle.is_running() { Some(processes::windows_pids()?) } else { None };
    let apps = run_installer(bottle, installer, &known, before, progress)?;
    Ok(apps.into_iter().filter(|a| !known.contains(&a.id)).collect())
}

/// Runs the installer (.exe, or .msi through msiexec) and waits for its
/// applications (wait_for_apps).
fn run_installer(
    bottle: &Bottle,
    installer: &Path,
    known: &HashSet<String>,
    before: Option<HashSet<u32>>,
    progress: &dyn Fn(&str),
) -> Result<Vec<App>> {
    let file = file_name(installer);
    progress(&tr!("Installation de {file}", "Installing {file}"));
    let wine = bottle.wine()?;
    let path = installer.to_string_lossy();
    if is_msi(&file) {
        // msiexec would take `/Users/…` for an option: Windows path, through
        // drive Z:, which gives access to the whole Mac.
        let windows = format!("Z:{}", path.replace('/', "\\"));
        run_in_bottle(&wine, bottle, "msiexec", &["/i".into(), windows], true)?;
    } else {
        run_exe(&wine, bottle, installer, &[], &[], true)?;
    }
    wait_for_apps(bottle, known, before.as_ref(), progress)
}

/// Waits for the applications of an installation. Some installers (WinRAR…)
/// hand the end of the work over to a second program, which shows the
/// options and creates the shortcuts, and exit without waiting for it. So
/// we wait for an application to appear (outside `known`), or for the
/// installation to be finished (or cancelled): the bottle stops, or, if it
/// was already running (`before`: the Windows programs at that time), the
/// programs that appeared since have all gone away.
fn wait_for_apps(
    bottle: &Bottle,
    known: &HashSet<String>,
    before: Option<&HashSet<u32>>,
    progress: &dyn Fn(&str),
) -> Result<Vec<App>> {
    let start = Instant::now();
    let mut told = false;
    loop {
        // State first: that way, shortcuts created just before the end are
        // seen by the search that follows.
        let busy = bottle.is_running()
            && match before {
                None => true,
                Some(before) => processes::windows_pids()?.difference(before).next().is_some(),
            };
        let apps = installed(bottle)?;
        let new_app = apps.iter().any(|a| !known.contains(&a.id));
        // Safeguard, in case a program left behind by the installer never exits.
        let too_long = start.elapsed() > Duration::from_secs(30 * 60);
        if new_app || !busy || too_long {
            return Ok(apps);
        }
        // After a cancellation, Wine stops within a few seconds: we only
        // mention waiting beyond that.
        if !told && start.elapsed() > Duration::from_secs(5) {
            progress(&tr!(
                "Fin de l'installation, dans la fenêtre de l'installateur",
                "Finishing the installation, in the installer's window"
            ));
            told = true;
        }
        sleep(Duration::from_secs(1));
    }
}

/// Adds a portable program to the bottle, in `C:\Program Files`, then
/// records it in its config. A portable program often needs the files
/// sitting next to it (.dll, data): we copy its whole folder, unless it
/// sits alone in a common folder (Downloads, Desktop…), from which we only
/// take the .exe. The copy is an APFS clone when possible: instant, and
/// with no extra disk space.
pub fn add_portable(bottle: &mut Bottle, exe: &Path) -> Result<App> {
    let file = file_name(exe);
    let name = exe
        .file_stem()
        .with_context(|| tr!("nom de fichier invalide", "invalid file name"))?
        .to_string_lossy()
        .into_owned();
    let source = exe.parent().with_context(|| tr!("programme sans dossier", "program without a folder"))?;
    let program_files = bottle.prefix().join("drive_c/Program Files");
    let windows = if is_common_folder(source) {
        let dir = program_files.join(&name);
        fs::create_dir_all(&dir)?;
        fs::copy(exe, dir.join(&file)).with_context(|| tr!("copie de {}", "copying {}", exe.display()))?;
        format!(r"C:\Program Files\{name}\{file}")
    } else {
        let folder = file_name(source);
        let dir = program_files.join(&folder);
        if dir.exists() {
            bail!(tr!("{} existe déjà dans la bouteille", "{} already exists in the bottle", dir.display()));
        }
        copy_folder(source, &dir)?;
        format!(r"C:\Program Files\{folder}\{file}")
    };
    bottle.config.apps.insert(name.clone(), windows);
    bottle.save()?;
    find(bottle, &format!("cave:{name}"))
}

/// A folder that doesn't belong to the program: the home folder, its usual
/// folders (Downloads, Desktop, Documents…), or the root of a disk.
fn is_common_folder(dir: &Path) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    const USUAL: &[&str] = &["Downloads", "Desktop", "Documents", "Public", "Movies", "Music", "Pictures"];
    let in_usual = dir.parent() == Some(home.as_path())
        && dir.file_name().is_some_and(|n| USUAL.iter().any(|u| n == *u));
    // `/`, `/Users`, `/Applications`, `/Volumes/<disk>`…
    let shallow = dir.components().count() <= 2 || (dir.starts_with("/Volumes") && dir.components().count() <= 3);
    dir == home || in_usual || shallow
}

/// Copies a whole folder: APFS clone if possible, otherwise a regular copy
/// (other disk).
fn copy_folder(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst.parent().with_context(|| tr!("destination sans dossier", "destination without a folder"))?)?;
    if crate::bottle::clone_dir(src, dst).is_ok() {
        return Ok(());
    }
    let status = std::process::Command::new("/bin/cp")
        .arg("-R")
        .arg(src)
        .arg(dst)
        .status()
        .with_context(|| tr!("lancement de cp", "starting cp"))?;
    if !status.success() {
        bail!(tr!("copie de {} impossible ({status})", "could not copy {} ({status})", src.display()));
    }
    Ok(())
}

/// An available bottle name, taken from the installer's name without its
/// version numbers or technical words: `npp.8.6.Installer.x64.exe` → npp.
pub fn bottle_name_for(file: &Path) -> Result<String> {
    let stem = file.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let base = clean_name(&stem);
    let mut name = base.clone();
    let mut n = 2;
    while Bottle::path_for(&name)?.exists() {
        name = format!("{base}-{n}");
        n += 1;
    }
    Ok(name)
}

fn clean_name(stem: &str) -> String {
    let noise = |w: &&str| {
        let l = w.to_lowercase();
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        digits(&l)
            || l.strip_prefix('v').is_some_and(digits)
            || matches!(
                l.as_str(),
                "setup" | "install" | "installer" | "x64" | "x86" | "win" | "win32" | "win64"
                    | "amd64" | "arm64" | "64bit" | "32bit" | "full" | "release"
            )
    };
    let stem: String = stem.chars().map(without_accent).collect();
    let words: Vec<&str> = stem
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let kept: Vec<&str> = words.iter().copied().take_while(|w| !noise(w)).collect();
    let name = match (kept.is_empty(), words.first()) {
        (false, _) => kept.join("-"),
        (true, Some(first)) => first.to_string(),
        (true, None) => "application".to_string(),
    };
    name.chars().take(40).collect()
}

/// Bottle names are ASCII: "é" becomes "e".
fn without_accent(c: char) -> char {
    const FROM: &str = "àâäáãéèêëíìîïóòôöõúùûüçñÀÂÄÁÃÉÈÊËÍÌÎÏÓÒÔÖÕÚÙÛÜÇÑ";
    const TO: &str = "aaaaaeeeeiiiiooooouuuucnAAAAAEEEEIIIIOOOOOUUUUCN";
    FROM.chars().position(|f| f == c).and_then(|i| TO.chars().nth(i)).unwrap_or(c)
}

/// Shortcut folders: the common Start menu, then each user's Start menu
/// and Desktop (including `Public`, the common Desktop).
fn shortcut_dirs(bottle: &Bottle) -> Vec<PathBuf> {
    let drive_c = bottle.prefix().join("drive_c");
    let mut dirs = vec![drive_c.join("ProgramData").join(START_MENU)];
    if let Ok(users) = fs::read_dir(drive_c.join("users")) {
        let mut users: Vec<PathBuf> = users.flatten().map(|u| u.path()).collect();
        users.sort();
        for user in users {
            dirs.push(user.join("AppData/Roaming").join(START_MENU));
            dirs.push(user.join("Desktop"));
        }
    }
    dirs
}

/// The .lnk files of a folder and its subfolders. Symbolic links are not
/// followed: depending on the Wine version, a user's Desktop is a link to
/// the Mac's.
fn collect_shortcuts(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 || fs::symlink_metadata(dir).map_or(true, |m| !m.is_dir()) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() {
            collect_shortcuts(&path, depth + 1, out);
        } else if kind.is_file() && has_extension(&path, "lnk") {
            out.push(path);
        }
    }
}

fn from_shortcut(bottle: &Bottle, path: &Path) -> Option<App> {
    let shortcut = lnk::parse(&fs::read(path).ok()?).ok()?;
    let exe = match (&shortcut.target, &shortcut.relative_path) {
        (Some(target), _) => bottle.unix_path(target)?,
        (None, Some(relative)) => path.parent()?.join(relative.replace('\\', "/")),
        (None, None) => return None,
    };
    let name = path.file_stem()?.to_string_lossy().into_owned();
    if !has_extension(&exe, "exe") || !exe.is_file() || is_uninstaller(&name, &exe) {
        return None;
    }
    // The icon can come from a file other than the program: an .ico, or an
    // .exe/.dll and its index. Failing that (path with variables such as
    // %SystemRoot%, file without an icon…), we take the program's own.
    let location = shortcut
        .icon_location
        .as_deref()
        .and_then(|l| bottle.unix_path(l))
        .filter(|f| f.is_file());
    let icon = match location {
        Some(ico) if has_extension(&ico, "ico") => Some(ico),
        Some(file) => cached_icon(bottle, &file, shortcut.icon_index),
        None if shortcut.icon_location.is_none() => cached_icon(bottle, &exe, shortcut.icon_index),
        None => None,
    }
    .or_else(|| cached_icon(bottle, &exe, 0));
    let steam_exe = steam::steam_dir(bottle).join("steam.exe");
    Some(App {
        id: path.strip_prefix(bottle.prefix()).ok()?.to_string_lossy().into_owned(),
        name,
        args: split_args(shortcut.arguments.as_deref().unwrap_or("")),
        icon,
        is_steam: exe.to_string_lossy().eq_ignore_ascii_case(&steam_exe.to_string_lossy()),
        exe,
        last_launched: None,
    })
}

/// Uninstallers often have their own shortcut: they are not applications.
fn is_uninstaller(name: &str, exe: &Path) -> bool {
    let name = name.to_lowercase();
    let file = exe.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
    file.starts_with("unins")
        || ["uninstall", "désinstaller", "desinstaller", "uninst"]
            .iter()
            .any(|w| name.contains(w))
}

/// A program's icon, extracted once then kept in <bottle>/icons, and redone
/// if the program has changed since. A program without an icon is noted
/// (.none file) so that it isn't read again every time.
fn cached_icon(bottle: &Bottle, source: &Path, index: i32) -> Option<PathBuf> {
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    let source_time = modified(source)?;
    let mut hasher = DefaultHasher::new();
    (source, index).hash(&mut hasher);
    let stem = format!("{:016x}", hasher.finish());
    let dir = bottle.root.join("icons");
    for ext in ["png", "ico", "none"] {
        let cached = dir.join(format!("{stem}.{ext}"));
        if modified(&cached).is_some_and(|t| t >= source_time) {
            return (ext != "none").then_some(cached);
        }
    }
    fs::create_dir_all(&dir).ok()?;
    match fs::read(source).ok().and_then(|exe| icon::extract(&exe, index).ok()) {
        Some(image) => {
            let path = dir.join(format!("{stem}.{}", image.extension));
            fs::write(&path, image.bytes).ok()?;
            Some(path)
        }
        None => {
            let _ = fs::write(dir.join(format!("{stem}.none")), b"");
            None
        }
    }
}

/// Splits a Windows command line into arguments: separated by spaces,
/// except inside quotes.
fn split_args(line: &str) -> Vec<String> {
    let mut args = vec![];
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn is_msi(file: &str) -> bool {
    has_extension(Path::new(file), "msi")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_bottles_after_the_installer() {
        assert_eq!(clean_name("npp.8.6.Installer.x64"), "npp");
        assert_eq!(clean_name("VLC-3.0.21-win64"), "VLC");
        assert_eq!(clean_name("GOG_Galaxy_2.0"), "GOG-Galaxy");
        assert_eq!(clean_name("SteamSetup"), "SteamSetup");
        assert_eq!(clean_name("7z2408-x64"), "7z2408");
        assert_eq!(clean_name("setup"), "setup");
        assert_eq!(clean_name("Éditeur d'été"), "Editeur-d-ete");
        assert_eq!(clean_name("游戏"), "application");
    }

    #[test]
    fn copies_a_program_folder_but_not_a_common_one() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert!(is_common_folder(&home.join("Downloads")));
        assert!(is_common_folder(&home.join("Desktop")));
        assert!(is_common_folder(&home));
        assert!(is_common_folder(Path::new("/Applications")));
        assert!(is_common_folder(Path::new("/Volumes/USB Drive")));
        assert!(!is_common_folder(&home.join("Downloads/ILSpy_binaries")));
        assert!(!is_common_folder(Path::new("/Volumes/USB Drive/Games/MyGame")));
    }

    #[test]
    fn skips_uninstallers() {
        let exe = Path::new("/p/unins000.exe");
        assert!(is_uninstaller("Program", exe));
        assert!(is_uninstaller("Désinstaller Programme", Path::new("/p/x.exe")));
        assert!(!is_uninstaller("Program", Path::new("/p/program.exe")));
    }

    #[test]
    fn splits_windows_command_lines() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("-a  -b"), vec!["-a", "-b"]);
        assert_eq!(
            split_args(r#"--config "C:\My Documents\x.ini" """#),
            vec!["--config", r"C:\My Documents\x.ini", ""]
        );
    }
}
