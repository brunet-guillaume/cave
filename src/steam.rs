//! Launching Steam: works around the black window of its interface.
//!
//! Steam's interface is built with CEF (embedded Chromium). Chromium
//! composites its frames in a separate process (the GPU process), which
//! draws into the main process's window. Wine cannot paint into another
//! process's window: the window stays black.
//!
//! The fix is `--in-process-gpu` (what CrossOver adds), but Steam does not
//! pass it on to CEF.
//! So we replace `steamwebhelper.exe` with a small shim (shim/, built by
//! build.rs) that relaunches the real binary, set aside under the name
//! `steamwebhelper_real.exe`, with the right arguments.
//!
//! At startup, Steam checks its executables and would put the original
//! back: hence `-noverifyfiles`. A Steam update overwrites the shim; it is
//! reinstalled on the next launch.

use crate::bottle::Bottle;
use crate::config::{cache_dir, download};
use crate::runner::{run_exe, run_in_bottle, RunResult};
use crate::tr;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

const SHIM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/steamwebhelper.exe"));

/// The shim is a few KB, Valve's webhelper several MB: the size is enough
/// to tell them apart, even after the shim changes.
const SHIM_MAX_SIZE: u64 = 1024 * 1024;

pub fn steam_dir(bottle: &Bottle) -> PathBuf {
    bottle.prefix().join("drive_c/Program Files (x86)/Steam")
}

/// Launches Steam in the bottle; returns when Steam exits.
pub fn launch(bottle: &Bottle, args: &[String], quiet: bool) -> Result<RunResult> {
    let wine = bottle.wine()?;
    let (exe, mut steam_args) = prepare(bottle)?;
    steam_args.extend_from_slice(args);
    // If Steam was already running, steam.exe hands over to it and returns at once.
    let started_here = !bottle.is_running();
    remove_autostart(bottle)?;
    let res = run_exe(&wine, bottle, &exe, &steam_args, &dock_names(bottle), quiet)?;
    if started_here {
        // Steam registered itself again while running: clean up once the
        // wineserver has stopped (it rewrites the registry when it exits).
        for _ in 0..20 {
            if !bottle.is_running() {
                break;
            }
            sleep(Duration::from_millis(500));
        }
        remove_autostart(bottle)?;
    }
    Ok(res)
}

/// Names shown in the Dock (see runner::run_exe): "Steam" for its whole
/// folder, including the webhelper that displays its windows, and each
/// game's name for its folder. The longest prefix wins: a game stored in
/// Steam's folder keeps its name.
fn dock_names(bottle: &Bottle) -> Vec<(String, String)> {
    let mut names = vec![];
    let mut add = |dir: &Path, name: String| {
        if let Some(windows) = bottle.windows_path(dir) {
            names.push((format!("{windows}\\"), name));
        }
    };
    add(&steam_dir(bottle), "Steam".to_string());
    for game in crate::games::installed(bottle).unwrap_or_default() {
        add(&game.dir, game.name);
    }
    names
}

const INSTALLER_URL: &str = "https://cdn.akamai.steamstatic.com/client/installer/SteamSetup.exe";

/// Installs Steam in the bottle: the official installer in silent mode, then
/// a first start that downloads the client (including CEF: without it, there
/// is no steamwebhelper.exe to replace with the shim).
/// `progress` receives each step.
pub fn install(bottle: &Bottle, progress: &dyn Fn(&str)) -> Result<()> {
    let wine = bottle.wine()?;

    progress(&tr!("Téléchargement de l'installateur Steam", "Downloading the Steam installer"));
    let setup = cache_dir()?.join("SteamSetup.exe");
    if !setup.is_file() {
        download(INSTALLER_URL, &setup)?;
    }

    progress(&tr!("Installation de Steam", "Installing Steam"));
    // /S: silent install (NSIS installer).
    let res = run_in_bottle(&wine, bottle, &setup.to_string_lossy(), &["/S".into()], true)?;
    if !res.status.success() {
        bail!(tr!("l'installateur de Steam a échoué, voir {}", "the Steam installer failed, see {}", res.log_file.display()));
    }

    set_language(bottle, &wine)?;
    progress(&tr!(
        "Premier démarrage : téléchargement du client Steam (quelques minutes)",
        "First start: downloading the Steam client (a few minutes)"
    ));
    bootstrap(bottle, &wine)?;
    remove_autostart(bottle)
}

/// Steam's language, based on macOS's. Installed silently, Steam would start
/// in English; it reads its language from the registry
/// (HKCU\Software\Valve\Steam, value Language), which we write before its
/// first start. The user can still change it in Steam.
fn set_language(bottle: &Bottle, wine: &Path) -> Result<()> {
    let Some(language) = steam_language(&crate::i18n::mac_languages()) else {
        return Ok(());
    };
    let args = ["add", r"HKCU\Software\Valve\Steam", "/v", "Language", "/t", "REG_SZ", "/d", language, "/f"];
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let res = run_in_bottle(wine, bottle, "reg", &args, true)?;
    if !res.status.success() {
        bail!(tr!("impossible de régler la langue de Steam, voir {}", "could not set Steam's language, see {}", res.log_file.display()));
    }
    Ok(())
}

/// Steam name of the first macOS language that Steam knows.
fn steam_language(languages: &[String]) -> Option<&'static str> {
    // Language codes (and variants) → names used by Steam.
    const LANGUAGES: &[(&str, &str)] = &[
        ("pt-BR", "brazilian"), ("es-419", "latam"), ("es-MX", "latam"),
        ("zh-Hant", "tchinese"), ("zh-TW", "tchinese"), ("zh-HK", "tchinese"),
        ("zh", "schinese"), ("fr", "french"), ("en", "english"), ("de", "german"),
        ("es", "spanish"), ("it", "italian"), ("pt", "portuguese"), ("nl", "dutch"),
        ("da", "danish"), ("fi", "finnish"), ("nb", "norwegian"), ("no", "norwegian"),
        ("sv", "swedish"), ("pl", "polish"), ("cs", "czech"), ("hu", "hungarian"),
        ("ro", "romanian"), ("ru", "russian"), ("uk", "ukrainian"), ("tr", "turkish"),
        ("el", "greek"), ("bg", "bulgarian"), ("ja", "japanese"), ("ko", "koreana"),
        ("th", "thai"), ("vi", "vietnamese"), ("id", "indonesian"), ("ar", "arabic"),
    ];
    languages.iter().find_map(|lang| {
        LANGUAGES
            .iter()
            .find(|(code, _)| lang == code || lang.starts_with(&format!("{code}-")))
            .map(|(_, steam)| *steam)
    })
}

/// Runs Steam in the background (-silent) until its webhelper starts, a sign
/// that the client is downloaded and installed, then stops it.
fn bootstrap(bottle: &Bottle, wine: &Path) -> Result<()> {
    let log = steam_dir(bottle).join("logs/webhelper.txt");
    // Only the lines written from now on count.
    let start = fs::metadata(&log).map(|m| m.len() as usize).unwrap_or(0);

    let (wine, exe) = (wine.to_path_buf(), steam_dir(bottle).join("steam.exe"));
    let copy = Bottle::load(&bottle.config.name)?;
    let steam = thread::spawn(move || {
        run_in_bottle(&wine, &copy, &exe.to_string_lossy(), &["-silent".into()], true)
    });

    // The client is a few hundred MB; Steam restarts after updating itself.
    let deadline = Instant::now() + Duration::from_secs(20 * 60);
    loop {
        let text = fs::read(&log).unwrap_or_default();
        if text.len() > start && String::from_utf8_lossy(&text[start..]).contains("webhelper launched") {
            break;
        }
        if Instant::now() > deadline {
            bottle.kill()?;
            bail!(tr!("Steam n'a pas fini son premier démarrage en 20 minutes", "Steam did not finish its first start within 20 minutes"));
        }
        sleep(Duration::from_secs(2));
    }
    // Let the webhelper finish starting before stopping the bottle.
    sleep(Duration::from_secs(5));
    bottle.kill()?;
    let _ = steam.join();
    for _ in 0..20 {
        if !bottle.is_running() {
            break;
        }
        sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// Registry section of the programs run at Windows startup.
const RUN_KEY: &str = "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Run]";

/// Steam registers itself to run at Windows startup ("Run Steam when my
/// computer starts"), and Wine runs that entry every time the bottle starts,
/// even for a mere winecfg. We remove it from user.reg, Wine's user registry
/// (a text file), while the bottle is stopped.
fn remove_autostart(bottle: &Bottle) -> Result<()> {
    if bottle.is_running() {
        return Ok(());
    }
    let reg = bottle.prefix().join("user.reg");
    let Ok(text) = fs::read_to_string(&reg) else {
        return Ok(());
    };
    if let Some(cleaned) = without_run_entry(&text, "Steam") {
        write_replacing(&reg, cleaned.as_bytes())?;
    }
    Ok(())
}

/// The registry without the Run section's `name` value; None if it is absent.
fn without_run_entry(reg: &str, name: &str) -> Option<String> {
    let value = format!("\"{name}\"=");
    let mut in_run = false;
    let mut removed = false;
    let mut out = String::with_capacity(reg.len());
    for line in reg.split_inclusive('\n') {
        if line.starts_with('[') {
            in_run = line.starts_with(RUN_KEY);
        } else if in_run && line.starts_with(&value) {
            removed = true;
            continue;
        }
        out.push_str(line);
    }
    removed.then_some(out)
}

/// Installs the shim in every `bin/cef/cef.win*`.
/// Returns the path of steam.exe and the arguments to pass to it.
pub fn prepare(bottle: &Bottle) -> Result<(PathBuf, Vec<String>)> {
    let dir = steam_dir(bottle);
    let exe = dir.join("steam.exe");
    if !exe.is_file() {
        bail!(tr!(
            "Steam n'est pas installé dans « {} » ({} introuvable)",
            "Steam is not installed in “{}” ({} not found)",
            bottle.config.name,
            exe.display()
        ));
    }

    let cef = dir.join("bin/cef");
    let mut patched = 0;
    for entry in fs::read_dir(&cef).with_context(|| tr!("lecture de {}", "reading {}", cef.display()))? {
        let path = entry?.path();
        let is_cef = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("cef.win"));
        if is_cef && path.join("steamwebhelper.exe").is_file() {
            install_shim(&path)?;
            patched += 1;
        }
    }
    if patched == 0 {
        bail!(tr!("aucun steamwebhelper.exe trouvé sous {}", "no steamwebhelper.exe found under {}", cef.display()));
    }
    Ok((exe, vec!["-noverifyfiles".to_string()]))
}

fn install_shim(cef_dir: &Path) -> Result<()> {
    let helper = cef_dir.join("steamwebhelper.exe");
    let real = cef_dir.join("steamwebhelper_real.exe");

    if is_shim(&helper)? {
        if !real.is_file() || is_shim(&real)? {
            bail!(tr!(
                "le webhelper d'origine a disparu de {} : réinstalle Steam dans cette bouteille",
                "the original webhelper is missing from {}: reinstall Steam in this bottle",
                cef_dir.display()
            ));
        }
        if fs::read(&helper)? == SHIM {
            return Ok(());
        }
    } else {
        // Valve's binary: first install, or Steam has updated itself.
        fs::rename(&helper, &real)
            .with_context(|| tr!("mise de côté de {}", "moving aside {}", helper.display()))?;
    }
    write_replacing(&helper, SHIM)
}

fn is_shim(path: &Path) -> Result<bool> {
    Ok(fs::metadata(path)?.len() < SHIM_MAX_SIZE)
}

/// Writes alongside, then renames: if Steam is running, the process running
/// the old file keeps it intact.
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).with_context(|| tr!("écriture de {}", "writing {}", path.display()))
}

/// Where a game launch stands, according to Steam's console log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchState {
    /// Steam is preparing the launch, or has not logged anything about it yet.
    Preparing,
    /// Steam waits for an answer in its own window before going on: a notice
    /// such as "better with a controller", a license…
    WaitingForUser,
    /// The game process is created: the launch is over.
    Done,
}

/// Follows the launch of one game in Steam's console log
/// (`logs/console_log.txt`): only what Steam writes after `new`.
pub struct LaunchWatch {
    log: PathBuf,
    appid: u32,
    inode: Option<u64>,
    offset: u64,
    state: LaunchState,
}

impl LaunchWatch {
    /// To be created before asking Steam to launch the game.
    pub fn new(bottle: &Bottle, appid: u32) -> LaunchWatch {
        LaunchWatch::at(steam_dir(bottle).join("logs/console_log.txt"), appid)
    }

    fn at(log: PathBuf, appid: u32) -> LaunchWatch {
        let meta = fs::metadata(&log).ok();
        LaunchWatch {
            inode: meta.as_ref().map(MetadataExt::ino),
            offset: meta.map_or(0, |m| m.len()),
            log,
            appid,
            state: LaunchState::Preparing,
        }
    }

    /// Reads what Steam has logged since the previous call.
    pub fn poll(&mut self) -> LaunchState {
        let Ok(meta) = fs::metadata(&self.log) else { return self.state };
        // Steam starts a new log when it starts: read that one from the top.
        if Some(meta.ino()) != self.inode || meta.len() < self.offset {
            self.inode = Some(meta.ino());
            self.offset = 0;
        }
        let mut new = Vec::new();
        let read = fs::File::open(&self.log)
            .and_then(|mut file| file.seek(SeekFrom::Start(self.offset)).and_then(|_| file.read_to_end(&mut new)));
        if read.is_ok() {
            // Whole lines only: Steam may be writing the last one.
            let end = new.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            self.offset += end as u64;
            self.state = launch_state(&String::from_utf8_lossy(&new[..end]), self.appid, self.state);
        }
        self.state
    }
}

/// The launch of `appid` after some new lines of Steam's console log, such as:
/// `GameAction [AppID 1588550, ActionID 1] : LaunchApp waiting for user response to ShowInterstitials ""`,
/// then `… LaunchApp continues with user response "ShowInterstitials"` and
/// `… LaunchApp changed task to Completed with ""`.
fn launch_state(lines: &str, appid: u32, mut state: LaunchState) -> LaunchState {
    let tag = format!("[AppID {appid},");
    for line in lines.lines().filter(|l| l.contains(&tag) && l.contains("LaunchApp")) {
        state = if line.contains("waiting for user response") {
            LaunchState::WaitingForUser
        } else if line.contains("changed task to Completed") {
            LaunchState::Done
        } else {
            LaunchState::Preparing
        };
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines from a real console_log.txt: Cairn stopped on a question, Baba
    // Is You only "waits" for a moment at each launch.
    const CAIRN: &str = "\
[2026-10-13 04:48:26] GameAction [AppID 1588550, ActionID 1] : LaunchApp changed task to RunningInstallScript with \"\"
[2026-10-13 04:48:39] GameAction [AppID 1588550, ActionID 1] : LaunchApp changed task to ShowInterstitials with \"\"
[2026-10-13 04:48:39] GameAction [AppID 1588550, ActionID 1] : LaunchApp waiting for user response to ShowInterstitials \"\"
";
    const BABA: &str = "\
[2026-10-12 23:04:11] GameAction [AppID 736260, ActionID 1] : LaunchApp waiting for user response to ShowInterstitials \"\"
[2026-10-12 23:04:12] GameAction [AppID 736260, ActionID 1] : LaunchApp continues with user response \"ShowInterstitials\"
[2026-10-12 23:04:12] GameAction [AppID 736260, ActionID 1] : LaunchApp changed task to CreatingProcess with \"\"
";
    const BABA_DONE: &str = "\
[2026-10-12 23:04:13] Game process added : AppID 736260 \"\"C:\\Program Files (x86)\\Steam\\steamapps\\common\\Baba Is You\\Baba Is You.exe\"\", ProcID 1472, IP 0.0.0.0:0
[2026-10-12 23:04:13] GameAction [AppID 736260, ActionID 1] : LaunchApp changed task to Completed with \"\"
";

    #[test]
    fn follows_a_game_launch_in_the_console_log() {
        use LaunchState::*;
        assert_eq!(launch_state(CAIRN, 1588550, Preparing), WaitingForUser);
        assert_eq!(launch_state(CAIRN, 736260, Preparing), Preparing, "another game");
        assert_eq!(launch_state(CAIRN, 158855, Preparing), Preparing, "appid prefix");
        assert_eq!(launch_state(BABA, 736260, Preparing), Preparing);
        assert_eq!(launch_state(BABA_DONE, 736260, Preparing), Done);
        assert_eq!(launch_state("", 736260, WaitingForUser), WaitingForUser, "nothing new");
    }

    #[test]
    fn launch_watch_reads_only_new_whole_lines() {
        let log = std::env::temp_dir().join(format!("cave-console-{}.txt", std::process::id()));
        fs::write(&log, CAIRN).unwrap();
        let mut watch = LaunchWatch::at(log.clone(), 1588550);
        assert_eq!(watch.poll(), LaunchState::Preparing, "older lines are ignored");

        // A line being written counts once it is complete.
        let (start, end) = CAIRN.split_at(CAIRN.len() - 10);
        let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut file, start.as_bytes()).unwrap();
        assert_eq!(watch.poll(), LaunchState::Preparing);
        std::io::Write::write_all(&mut file, end.as_bytes()).unwrap();
        assert_eq!(watch.poll(), LaunchState::WaitingForUser);

        // Steam started again with a new, shorter log.
        fs::remove_file(&log).unwrap();
        fs::write(&log, "[2026-10-13 05:00:00] GameAction [AppID 1588550, ActionID 2] : LaunchApp changed task to Completed with \"\"\n").unwrap();
        assert_eq!(watch.poll(), LaunchState::Done);
        fs::remove_file(&log).unwrap();
    }

    #[test]
    fn maps_macos_languages_to_steam() {
        let langs = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(steam_language(&langs(&["fr-FR", "en-FR"])), Some("french"));
        assert_eq!(steam_language(&langs(&["pt-BR"])), Some("brazilian"));
        assert_eq!(steam_language(&langs(&["pt-PT"])), Some("portuguese"));
        assert_eq!(steam_language(&langs(&["zh-Hans-CN"])), Some("schinese"));
        assert_eq!(steam_language(&langs(&["zh-Hant-TW"])), Some("tchinese"));
        assert_eq!(steam_language(&langs(&["es-419"])), Some("latam"));
        assert_eq!(steam_language(&langs(&["xx", "de-CH"])), Some("german"), "first known language");
        assert_eq!(steam_language(&langs(&["frr"])), None, "`frr` is not `fr`");
        assert_eq!(steam_language(&[]), None);
    }

    #[test]
    fn removes_steam_from_the_run_key_only() {
        let reg = concat!(
            "WINE REGISTRY Version 2\n\n",
            "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Run] 1791793083\n",
            "#time=1dd5a2233e4ff8c\n",
            "\"Steam\"=\"\\\"C:\\\\Program Files (x86)\\\\Steam\\\\steam.exe\\\" -silent\"\n",
            "\"Other\"=\"other.exe\"\n\n",
            "[Software\\\\Valve\\\\Steam] 1791807686\n",
            "\"Steam\"=\"to keep\"\n",
        );

        let cleaned = without_run_entry(reg, "Steam").unwrap();

        assert!(!cleaned.contains("-silent"));
        assert!(cleaned.contains("\"Other\"=\"other.exe\""));
        assert!(cleaned.contains("\"Steam\"=\"to keep\""));
        assert!(without_run_entry(&cleaned, "Steam").is_none());
    }

    fn cef_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cave-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn valve_binary() -> Vec<u8> {
        vec![0xAB; SHIM_MAX_SIZE as usize + 1]
    }

    #[test]
    fn first_install_keeps_the_original() {
        let dir = cef_dir("first");
        fs::write(dir.join("steamwebhelper.exe"), valve_binary()).unwrap();

        install_shim(&dir).unwrap();

        assert_eq!(fs::read(dir.join("steamwebhelper.exe")).unwrap(), SHIM);
        assert_eq!(fs::read(dir.join("steamwebhelper_real.exe")).unwrap(), valve_binary());
    }

    #[test]
    fn old_shim_is_replaced_without_touching_the_original() {
        let dir = cef_dir("old-shim");
        fs::write(dir.join("steamwebhelper.exe"), b"old shim").unwrap();
        fs::write(dir.join("steamwebhelper_real.exe"), valve_binary()).unwrap();

        install_shim(&dir).unwrap();

        assert_eq!(fs::read(dir.join("steamwebhelper.exe")).unwrap(), SHIM);
        assert_eq!(fs::read(dir.join("steamwebhelper_real.exe")).unwrap(), valve_binary());
    }

    #[test]
    fn steam_update_refreshes_the_original() {
        let dir = cef_dir("update");
        let updated = vec![0xCD; SHIM_MAX_SIZE as usize + 1];
        fs::write(dir.join("steamwebhelper.exe"), &updated).unwrap();
        fs::write(dir.join("steamwebhelper_real.exe"), valve_binary()).unwrap();

        install_shim(&dir).unwrap();

        assert_eq!(fs::read(dir.join("steamwebhelper.exe")).unwrap(), SHIM);
        assert_eq!(fs::read(dir.join("steamwebhelper_real.exe")).unwrap(), updated);
    }



    #[test]
    fn refuses_when_the_original_is_lost() {
        let dir = cef_dir("lost");
        fs::write(dir.join("steamwebhelper.exe"), SHIM).unwrap();

        assert!(install_shim(&dir).is_err());
    }
}
