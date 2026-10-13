//! A "bottle" = an isolated Wine prefix + its config.
//!
//! Layout of a bottle:
//!   bottles/<name>/
//!     bottle.toml   → config (Windows version, env variables…)
//!     prefix/       → the WINEPREFIX: drive_c, registry (*.reg), dosdevices
//!     logs/         → one log file per launch

use crate::config::{bottles_dir, data_dir, wineserver_for, Config};
use crate::runner::run_in_bottle;
use crate::runtime::{Graphics, Runtime};
use crate::runtimes::InstalledRuntime;
use crate::tr;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

#[derive(Debug, Serialize, Deserialize)]
pub struct BottleConfig {
    pub name: String,
    /// Emulated Windows version (win10, win7, winxp…), applied via winecfg.
    pub windows_version: String,
    pub created_at: String,
    /// Wine binary specific to this bottle. Takes precedence over the global
    /// config, which lets several runtimes coexist (versions, tests…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wine: Option<PathBuf>,
    /// Direct3D renderer (wined3d, dxmt, d3dmetal); otherwise the runtime's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphics: Option<Graphics>,
    /// Environment variables added to every launch.
    /// Useful examples: WINEMSYNC=1, DXVK_HUD=fps, WINEDEBUG=+loaddll
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Programs without an installer added by cave: name → Windows path of
    /// the .exe (the other applications are found through their shortcuts).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, String>,
    /// Last launch of each application by cave (identifier → seconds since
    /// 1970), to sort them by recent use.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub launched: BTreeMap<String, i64>,
}

/// Windows version of bottles created without choosing anything.
const WINDOWS: &str = "win10";

pub struct Bottle {
    pub root: PathBuf,
    pub config: BottleConfig,
}

impl Bottle {
    pub fn path_for(name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(bottles_dir()?.join(name))
    }

    pub fn prefix(&self) -> PathBuf {
        self.root.join("prefix")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Creates the folder and the config. The prefix is initialized
    /// (wineboot) afterwards by the caller, since that requires running Wine.
    pub fn create(name: &str, windows_version: &str, wine: Option<PathBuf>) -> Result<Self> {
        let root = Self::path_for(name)?;
        if root.exists() {
            bail!(tr!("la bouteille « {name} » existe déjà", "bottle “{name}” already exists"));
        }
        Self::create_at(root, name, windows_version, wine)
    }

    fn create_at(root: PathBuf, name: &str, windows_version: &str, wine: Option<PathBuf>) -> Result<Self> {
        fs::create_dir_all(root.join("prefix"))?;
        fs::create_dir_all(root.join("logs"))?;

        let mut env = BTreeMap::new();
        // Silences the "fixme" noise without hiding real errors.
        env.insert("WINEDEBUG".into(), "fixme-all".into());

        let bottle = Bottle {
            root,
            config: BottleConfig {
                name: name.to_string(),
                windows_version: windows_version.to_string(),
                created_at: chrono::Local::now().to_rfc3339(),
                wine,
                graphics: None,
                env,
                apps: BTreeMap::new(),
                launched: BTreeMap::new(),
            },
        };
        bottle.save()?;
        Ok(bottle)
    }

    /// Creates the bottle and initializes its prefix: `wineboot --init`
    /// (drive_c, registry, system folders) then the Windows version.
    /// `progress` receives the steps and the warnings. On failure, the
    /// half-created bottle is deleted.
    pub fn create_and_init(
        name: &str,
        windows_version: &str,
        wine: Option<PathBuf>,
        progress: &dyn Fn(&str),
    ) -> Result<Self> {
        // The binary is validated before creating the folder.
        let wine_bin = Config::load()?.resolve_wine_for(wine.as_ref())?;
        let bottle = Self::create(name, windows_version, wine)?;
        progress(&tr!("Initialisation du préfixe (peut prendre une minute)", "Setting up the prefix (may take a minute)"));
        if let Err(e) = bottle.init_prefix(&wine_bin, progress) {
            let _ = fs::remove_dir_all(&bottle.root);
            return Err(e);
        }
        Ok(bottle)
    }

    /// A ready-to-use bottle, with nothing to choose: the most recent runtime
    /// built by cave, Windows 10 and the runtime's best renderer.
    /// It is a copy (APFS clone) of a template prepared once per runtime:
    /// instant, except the very first time.
    pub fn create_ready(name: &str, progress: &dyn Fn(&str)) -> Result<Self> {
        let Some(runtime) = crate::runtimes::default()? else {
            bail!(tr!("aucun runtime installé : compile-en un avec « cave runtime build »", "no runtime installed: build one with “cave runtime build”"));
        };
        if Self::path_for(name)?.exists() {
            bail!(tr!("la bouteille « {name} » existe déjà", "bottle “{name}” already exists"));
        }
        let mut bottle = Self::template(&runtime, progress)?.clone_as(name)?;
        if let Some(graphics) = runtime.best_graphics() {
            bottle.set_graphics(graphics)?;
        }
        Ok(bottle)
    }

    /// A runtime's bottle template, in templates/<runtime>-win10.
    /// It is prepared (wineboot) in a temporary folder then renamed: an
    /// interrupted preparation doesn't leave a half-ready template behind.
    fn template(runtime: &InstalledRuntime, progress: &dyn Fn(&str)) -> Result<Self> {
        let dir = data_dir()?.join("templates");
        let root = dir.join(format!("{}-{WINDOWS}", runtime.name));
        if root.join("bottle.toml").is_file() {
            return Self::load_at(root);
        }
        progress(&tr!(
            "Préparation de Windows (la première fois seulement, environ une minute)",
            "Preparing Windows (first time only, about a minute)"
        ));
        let tmp = dir.join(format!(".{}-{}", runtime.name, std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let bottle = Self::create_at(tmp.clone(), &runtime.name, WINDOWS, Some(runtime.wine()))?;
        // The wineserver only writes the registry when it stops: we wait
        // for it before freezing the template.
        let ready = bottle
            .init_prefix(&runtime.wine(), progress)
            .and_then(|_| bottle.wait_stopped());
        if let Err(e) = ready {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        if fs::rename(&tmp, &root).is_err() {
            // Another launch prepared the template at the same time.
            let _ = fs::remove_dir_all(&tmp);
        }
        Self::load_at(root)
    }

    fn init_prefix(&self, wine: &Path, progress: &dyn Fn(&str)) -> Result<()> {
        let res = run_in_bottle(wine, self, "wineboot", &["--init".into()], true)?;
        if !res.status.success() {
            bail!(tr!("wineboot a échoué, voir {}", "wineboot failed, see {}", res.log_file.display()));
        }
        let version = self.config.windows_version.clone();
        let res = run_in_bottle(wine, self, "winecfg", &["/v".into(), version], true)?;
        if !res.status.success() {
            progress(&tr!(
                "⚠ impossible de régler la version de Windows, voir {}",
                "⚠ could not set the Windows version, see {}",
                res.log_file.display()
            ));
        }
        Ok(())
    }

    pub fn load(name: &str) -> Result<Self> {
        let root = Self::path_for(name)?;
        if !root.join("bottle.toml").exists() {
            bail!(tr!("bouteille « {name} » introuvable (essaie `cave list`)", "bottle “{name}” not found (try `cave list`)"));
        }
        Self::load_at(root)
    }

    fn load_at(root: PathBuf) -> Result<Self> {
        let cfg_path = root.join("bottle.toml");
        let raw = fs::read_to_string(&cfg_path)?;
        let config = toml::from_str(&raw)
            .with_context(|| tr!("bottle.toml invalide : {}", "invalid bottle.toml: {}", cfg_path.display()))?;
        Ok(Bottle { root, config })
    }

    pub fn save(&self) -> Result<()> {
        fs::write(
            self.root.join("bottle.toml"),
            toml::to_string_pretty(&self.config)?,
        )?;
        Ok(())
    }

    pub fn list() -> Result<Vec<Bottle>> {
        let dir = bottles_dir()?;
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            match Bottle::load(&name) {
                Ok(b) => out.push(b),
                Err(e) => eprintln!("{}", tr!("⚠ {name} ignorée : {e}", "⚠ {name} skipped: {e}")),
            }
        }
        out.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        Ok(out)
    }

    /// The bottle's wine binary: its own, otherwise the global config's.
    pub fn wine(&self) -> Result<PathBuf> {
        Config::load()?.resolve_wine_for(self.config.wine.as_ref())
    }

    /// Picks the Direct3D renderer, after checking that the runtime supports it.
    pub fn set_graphics(&mut self, graphics: Graphics) -> Result<()> {
        Runtime::for_wine(&self.wine()?).env_vars(Some(graphics))?;
        self.config.graphics = Some(graphics);
        self.save()
    }

    /// Is Wine running in this bottle, whatever launched it (CLI, interface,
    /// Steam…)? A prefix's wineserver listens on
    /// /tmp/.wine-<uid>/server-<dev>-<inode>/socket (server/request.c). The
    /// folder sometimes outlives the server: we test the connection, not its
    /// presence.
    pub fn is_running(&self) -> bool {
        let Ok(meta) = fs::metadata(self.prefix()) else {
            return false;
        };
        let socket = format!(
            "/tmp/.wine-{}/server-{:x}-{:x}/socket",
            meta.uid(),
            meta.dev(),
            meta.ino()
        );
        UnixStream::connect(socket).is_ok()
    }

    /// macOS path of a Windows path in the bottle (`C:\…`, `D:\…`).
    pub fn unix_path(&self, windows: &str) -> Option<PathBuf> {
        windows_to_unix(&self.prefix(), windows)
    }

    /// Windows path of a macOS path in the bottle (the inverse of unix_path).
    pub fn windows_path(&self, unix: &Path) -> Option<String> {
        unix_to_windows(&self.prefix(), unix)
    }

    /// Waits for the bottle's wineserver to stop; it outlives the last
    /// program by a few seconds (wineserver -w).
    pub fn wait_stopped(&self) -> Result<()> {
        Command::new(wineserver_for(&self.wine()?))
            .arg("-w")
            .env("WINEPREFIX", self.prefix())
            .status()?;
        Ok(())
    }

    /// Stops all of the bottle's Wine processes (wineserver -k).
    pub fn kill(&self) -> Result<ExitStatus> {
        // wineserver only needs the prefix to find its processes.
        Ok(Command::new(wineserver_for(&self.wine()?))
            .arg("-k")
            .env("WINEPREFIX", self.prefix())
            .status()?)
    }

    /// Copies the bottle under another name. On APFS, the copy is a clone:
    /// instant, and taking no disk space as long as the files don't diverge.
    /// To be done with the bottle stopped, or the copy may be inconsistent.
    pub fn clone_as(&self, name: &str) -> Result<Bottle> {
        let root = Self::path_for(name)?;
        if root.exists() {
            bail!(tr!("la bouteille « {name} » existe déjà", "bottle “{name}” already exists"));
        }
        // On a fresh Mac, bottles/ doesn't exist yet: clonefile doesn't
        // create parent folders.
        fs::create_dir_all(root.parent().with_context(|| tr!("dossier de bouteille sans parent", "bottle folder without a parent"))?)?;
        clone_dir(&self.root, &root)
            .with_context(|| tr!("copie de {} vers {}", "copying {} to {}", self.root.display(), root.display()))?;

        // The original's logs have no business in the copy.
        let mut copy = Bottle::load(name)?;
        fs::remove_dir_all(copy.logs_dir())?;
        fs::create_dir_all(copy.logs_dir())?;
        copy.config.name = name.to_string();
        copy.config.created_at = chrono::Local::now().to_rfc3339();
        copy.save()?;
        Ok(copy)
    }

    /// Renames the bottle (its folder and its config), after stopping its
    /// programs. Nothing else holds on to its name: Windows and Steam only
    /// know `C:\…` paths, which don't change.
    pub fn rename(&mut self, name: &str) -> Result<()> {
        let root = Self::path_for(name)?;
        // APFS is case-insensitive: "steam" → "Steam" targets the same
        // folder, which is allowed.
        let same_dir = |a: &Path, b: &Path| match (fs::metadata(a), fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.ino() == b.ino() && a.dev() == b.dev(),
            _ => false,
        };
        if root.exists() && !same_dir(&root, &self.root) {
            bail!(tr!("la bouteille « {name} » existe déjà", "bottle “{name}” already exists"));
        }
        if self.is_running() {
            self.kill()?;
            self.wait_stopped()?;
        }
        fs::rename(&self.root, &root)
            .with_context(|| tr!("renommage de {}", "renaming {}", self.root.display()))?;
        self.root = root;
        self.config.name = name.to_string();
        // Cached icons are keyed by their program's path, which has just
        // changed: they will be redone.
        let _ = fs::remove_dir_all(self.root.join("icons"));
        self.save()
    }

    /// Deletes the bottle and everything installed in it, after stopping
    /// its programs.
    pub fn delete(self) -> Result<()> {
        if self.is_running() {
            self.kill()?;
            self.wait_stopped()?;
        }
        fs::remove_dir_all(&self.root)
            .with_context(|| tr!("suppression de {}", "deleting {}", self.root.display()))
    }

    /// Full set of environment variables to run Wine in this bottle.
    /// The bottle's own come last: they win.
    pub fn env_vars(&self, runtime: &Runtime) -> Result<Vec<(String, String)>> {
        let mut vars = runtime.env_vars(self.config.graphics)?;
        vars.push((
            "WINEPREFIX".to_string(),
            self.prefix().to_string_lossy().to_string(),
        ));
        vars.extend(self.config.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        Ok(vars)
    }
}

/// Clones a whole folder in a single system call (clonefile, APFS).
pub(crate) fn clone_dir(src: &Path, dst: &Path) -> io::Result<()> {
    extern "C" {
        fn clonefile(src: *const std::ffi::c_char, dst: *const std::ffi::c_char, flags: u32) -> i32;
    }
    let src = CString::new(src.as_os_str().as_bytes())?;
    let dst = CString::new(dst.as_os_str().as_bytes())?;
    if unsafe { clonefile(src.as_ptr(), dst.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `D:\Games\Steam` → <prefix>/dosdevices/d:/Games/Steam: each Windows
/// drive is a link in dosdevices. C: always leads to drive_c: we go there
/// directly, so that a given file always has the same path.
fn windows_to_unix(prefix: &Path, path: &str) -> Option<PathBuf> {
    let (drive, rest) = path.split_once(':')?;
    if drive.len() != 1 {
        return None;
    }
    let mut out = match drive.to_lowercase().as_str() {
        "c" => prefix.join("drive_c"),
        d => prefix.join("dosdevices").join(format!("{d}:")),
    };
    for part in rest.split('\\').filter(|p| !p.is_empty()) {
        out.push(part);
    }
    Some(out)
}

/// <prefix>/drive_c/… → `C:\…`, <prefix>/dosdevices/d:/… → `D:\…`.
fn unix_to_windows(prefix: &Path, path: &Path) -> Option<String> {
    let mut parts = path
        .strip_prefix(prefix)
        .ok()?
        .iter()
        .map(|p| p.to_string_lossy().into_owned());
    let drive = match parts.next()?.as_str() {
        "drive_c" => "C:".to_string(),
        "dosdevices" => Some(parts.next()?)
            .filter(|d| d.len() == 2 && d.ends_with(':'))?
            .to_uppercase(),
        _ => return None,
    };
    Some(format!("{drive}\\{}", parts.collect::<Vec<_>>().join("\\")))
}

fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!(tr!(
            "nom invalide « {name} » : lettres, chiffres, - et _ uniquement",
            "invalid name “{name}”: letters, digits, - and _ only"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_paths_through_dosdevices() {
        let prefix = Path::new("/p");
        assert_eq!(
            windows_to_unix(prefix, r"C:\Program Files (x86)\Steam"),
            Some(PathBuf::from("/p/drive_c/Program Files (x86)/Steam"))
        );
        assert_eq!(
            windows_to_unix(prefix, r"D:\Games"),
            Some(PathBuf::from("/p/dosdevices/d:/Games"))
        );
        assert_eq!(windows_to_unix(prefix, "no drive"), None);
    }

    #[test]
    fn maps_macos_paths_back_to_windows() {
        let prefix = Path::new("/p");
        for windows in [r"C:\Program Files (x86)\Steam", r"D:\Games"] {
            let unix = windows_to_unix(prefix, windows).unwrap();
            assert_eq!(unix_to_windows(prefix, &unix).as_deref(), Some(windows));
        }
        assert_eq!(unix_to_windows(prefix, Path::new("/p/drive_c")).as_deref(), Some(r"C:\"));
        assert_eq!(unix_to_windows(prefix, Path::new("/elsewhere/x")), None);
        assert_eq!(unix_to_windows(prefix, Path::new("/p/logs/x")), None);
    }

    #[test]
    fn clone_dir_copies_a_whole_tree() {
        let base = std::env::temp_dir().join(format!("cave-clone-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let src = base.join("src");
        fs::create_dir_all(src.join("prefix/drive_c")).unwrap();
        fs::write(src.join("prefix/drive_c/file.txt"), b"content").unwrap();

        clone_dir(&src, &base.join("dst")).unwrap();

        let copied = fs::read(base.join("dst/prefix/drive_c/file.txt")).unwrap();
        assert_eq!(copied, b"content");
        assert!(clone_dir(&src, &base.join("dst")).is_err(), "the destination already exists");
    }
}
