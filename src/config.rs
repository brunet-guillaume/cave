//! Global configuration: where bottles live and which Wine binary to use.
//!
//! On macOS, the data folder is `~/Library/Application Support/cave`.
//! It contains:
//!   - `config.toml`    → global settings (path to Wine)
//!   - `bottles/<name>` → one bottle per folder

use crate::tr;
use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Explicit path to the `wine` binary. If absent, we look for
    /// `$CAVE_WINE`, then `wine` / `wine64` in the PATH.
    pub wine_bin: Option<PathBuf>,
}

pub fn data_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CAVE_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let dirs = ProjectDirs::from("fr", "umebrunet", "cave")
        .with_context(|| tr!("impossible de déterminer le dossier de données", "could not find the data folder"))?;
    Ok(dirs.data_dir().to_path_buf())
}

/// Reusable downloaded files (Steam installer…).
pub fn cache_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CAVE_HOME") {
        return Ok(PathBuf::from(dir).join("cache"));
    }
    let dirs = ProjectDirs::from("fr", "umebrunet", "cave")
        .with_context(|| tr!("impossible de déterminer le dossier de cache", "could not find the cache folder"))?;
    Ok(dirs.cache_dir().to_path_buf())
}

/// Downloads `url` to `dest`, going through a .part file: an interrupted
/// download does not leave a truncated file behind.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest.parent().with_context(|| tr!("destination sans dossier", "destination without a folder"))?)?;
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".part");
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .with_context(|| tr!("lancement de curl", "starting curl"))?;
    if !status.success() {
        bail!(tr!("téléchargement de {url} impossible ({status})", "could not download {url} ({status})"));
    }
    fs::rename(&tmp, dest)?;
    Ok(())
}

pub fn bottles_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("bottles"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = data_dir()?.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| tr!("lecture de {}", "reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| tr!("config invalide : {}", "invalid config: {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let dir = data_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("config.toml"), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Finds the Wine binary for a bottle: the bottle's own if it is set,
    /// otherwise the global resolution.
    pub fn resolve_wine_for(&self, bottle_wine: Option<&PathBuf>) -> Result<PathBuf> {
        match bottle_wine {
            Some(p) => check_exec(p.clone()),
            None => self.resolve_wine(),
        }
    }

    /// Finds the global Wine binary.
    pub fn resolve_wine(&self) -> Result<PathBuf> {
        if let Ok(p) = std::env::var("CAVE_WINE") {
            return check_exec(PathBuf::from(p));
        }
        if let Some(p) = &self.wine_bin {
            return check_exec(p.clone());
        }
        for name in ["wine", "wine64"] {
            if let Some(p) = which(name) {
                return Ok(p);
            }
        }
        bail!(tr!(
            "Wine introuvable. Installe-le (ex. `brew install --cask --no-quarantine wine-stable`) \
             ou indique son chemin avec `cave config --wine /chemin/vers/wine`.",
            "Wine not found. Install it (e.g. `brew install --cask --no-quarantine wine-stable`) \
             or give its path with `cave config --wine /path/to/wine`."
        ))
    }
}

/// `wineserver` always ships next to `wine`, in the same bin/ folder.
pub fn wineserver_for(wine: &Path) -> PathBuf {
    wine.with_file_name("wineserver")
}

fn check_exec(p: PathBuf) -> Result<PathBuf> {
    if p.is_file() {
        Ok(p)
    } else {
        bail!(tr!("le binaire Wine indiqué n'existe pas : {}", "the given Wine binary does not exist: {}", p.display()))
    }
}

/// Minimal equivalent of the `which` command.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}
