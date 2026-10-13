//! Runtimes installed by cave, in `<data>/runtimes/<name>`: listing,
//! building and adding graphics layers.
//!
//! Building goes through the scripts in scripts/ and the patches in
//! patches/wine, embedded into cave at compile time: `cave runtime build`
//! therefore also works from a cave installed with `cargo install`.

use crate::config::data_dir;
use crate::runtime::Graphics;
use crate::tr;
use anyhow::{bail, Context, Result};
use directories::BaseDirs;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Embedded scripts and patches, with their relative path in the repository.
/// A new patch in patches/wine must be added here (a test checks it).
const FILES: &[(&str, &str)] = &[
    ("scripts/build-wine.sh", include_str!("../scripts/build-wine.sh")),
    ("scripts/add-dxmt.sh", include_str!("../scripts/add-dxmt.sh")),
    ("scripts/add-d3dmetal.sh", include_str!("../scripts/add-d3dmetal.sh")),
    ("scripts/add-notices.sh", include_str!("../scripts/add-notices.sh")),
    ("scripts/add-winetricks.sh", include_str!("../scripts/add-winetricks.sh")),
    (
        "patches/wine/0001-ntdll-WINEDLLPATH_PREPEND.patch",
        include_str!("../patches/wine/0001-ntdll-WINEDLLPATH_PREPEND.patch"),
    ),
    (
        "patches/wine/0002-ntdll-app-names.patch",
        include_str!("../patches/wine/0002-ntdll-app-names.patch"),
    ),
];

pub fn runtimes_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("runtimes"))
}

/// Build folder shared with the scripts (`$CAVE_BUILD`).
fn build_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CAVE_BUILD") {
        return Ok(PathBuf::from(dir));
    }
    let dirs = BaseDirs::new().with_context(|| tr!("impossible de déterminer le dossier de cache", "could not find the cache folder"))?;
    Ok(dirs.cache_dir().join("cave-build"))
}

/// A runtime installed in `runtimes/`.
pub struct InstalledRuntime {
    pub name: String,
    pub root: PathBuf,
}

impl InstalledRuntime {
    pub fn wine(&self) -> PathBuf {
        self.root.join("bin/wine")
    }

    pub fn version(&self) -> Option<String> {
        let out = Command::new(self.wine()).arg("--version").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Best renderer available: D3DMetal (DirectX 11 and 12), otherwise DXMT
    /// (DirectX 10 and 11).
    pub fn best_graphics(&self) -> Option<Graphics> {
        if self.root.join("lib/apple_gptk/wine").is_dir() {
            Some(Graphics::D3dmetal)
        } else if self.root.join("lib/dxmt").is_dir() {
            Some(Graphics::Dxmt)
        } else {
            None
        }
    }

    /// Layers added on top of the base Wine.
    pub fn layers(&self) -> Vec<&'static str> {
        [
            ("lib/dxmt", "dxmt"),
            ("lib/apple_gptk", "d3dmetal"),
            ("lib/gstreamer-1.0", "gstreamer"),
        ]
        .into_iter()
        .filter(|(dir, _)| self.root.join(dir).is_dir())
        .map(|(_, layer)| layer)
        .collect()
    }
}

pub fn list() -> Result<Vec<InstalledRuntime>> {
    let dir = runtimes_dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for entry in fs::read_dir(&dir)? {
        let root = entry?.path();
        if root.join("bin/wine").is_file() {
            let name = root.file_name().unwrap().to_string_lossy().into_owned();
            out.push(InstalledRuntime { name, root });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Accepts the name of an installed runtime ("wine-cx-26.3.0") or a path
/// to a wine binary, and returns the path of the binary.
pub fn resolve(wine: &Path) -> Result<PathBuf> {
    if wine.components().count() == 1 {
        let candidate = runtimes_dir()?.join(wine).join("bin/wine");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(wine.to_path_buf())
}

/// The runtime used for new bottles: the most recent one built by cave
/// (wine-cx-26.10.0 comes after wine-cx-26.3.0).
pub fn default() -> Result<Option<InstalledRuntime>> {
    Ok(list()?
        .into_iter()
        .max_by(|a, b| version_key(&a.name).cmp(&version_key(&b.name))))
}

/// Update feed of the CrossOver application (Sparkle format): it announces
/// every version, from the most recent to the oldest.
const CROSSOVER_FEED: &str = "https://www.codeweavers.com/xml/versions/cxmac.xml";

/// Latest CrossOver version announced by CodeWeavers.
pub fn latest_crossover() -> Result<String> {
    let out = Command::new("curl")
        .args(["-fsSL", CROSSOVER_FEED])
        .output()
        .context("starting curl")?;
    if !out.status.success() {
        bail!(tr!("flux des versions de CrossOver inaccessible ({})", "CrossOver version feed unreachable ({})", out.status));
    }
    latest_version(&String::from_utf8_lossy(&out.stdout))
        .with_context(|| tr!("aucune version dans le flux de CrossOver", "no version in the CrossOver feed"))
}

fn latest_version(feed: &str) -> Option<String> {
    feed.split("sparkle:shortVersionString=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .max_by_key(|v| version_key(v))
        .map(str::to_string)
}

/// URL of the sources of a CrossOver version.
pub fn sources_url(version: &str) -> String {
    format!("https://media.codeweavers.com/pub/crossover/source/crossover-sources-{version}.tar.gz")
}

/// Are the sources of this version published? They sometimes arrive a few
/// days after the application.
pub fn sources_published(version: &str) -> bool {
    Command::new("curl")
        .args(["-fsI", &sources_url(version)])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Is `candidate` a more recent version than `than`? The names may carry a
/// prefix ("wine-cx-26.3.0" versus "26.10.0").
pub fn is_newer(candidate: &str, than: &str) -> bool {
    version_key(candidate) > version_key(than)
}

/// The numbers in a name, for comparing versions: "wine-cx-26.10.0" → [26, 10, 0].
fn version_key(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect()
}

/// Installs a runtime from its archive (the .tar.xz published with cave,
/// made by scripts/package-runtime.sh). The archive is extracted alongside,
/// then the runtime is renamed into place: an interrupted installation
/// doesn't leave a half-copied runtime behind.
pub fn install_archive(archive: &Path) -> Result<InstalledRuntime> {
    let dir = runtimes_dir()?;
    let tmp = dir.join(format!(".installation-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let result = extract_runtime(archive, &tmp, &dir);
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn extract_runtime(archive: &Path, tmp: &Path, dir: &Path) -> Result<InstalledRuntime> {
    // tar refuses to write outside -C (absolute paths, "..").
    let status = Command::new("/usr/bin/tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(tmp)
        .status()
        .with_context(|| tr!("lancement de tar", "starting tar"))?;
    if !status.success() {
        bail!(tr!("{} n'a pas pu être extraite ({status})", "{} could not be extracted ({status})", archive.display()));
    }
    let entries: Vec<PathBuf> = fs::read_dir(tmp)?.flatten().map(|e| e.path()).collect();
    let [extracted] = entries.as_slice() else {
        bail!(tr!(
            "archive inattendue : elle doit contenir un seul dossier de runtime",
            "unexpected archive: it must contain a single runtime folder"
        ));
    };
    if !extracted.join("bin/wine").is_file() {
        bail!(tr!("archive inattendue : pas de bin/wine dans {}", "unexpected archive: no bin/wine in {}", extracted.display()));
    }
    let name = extracted.file_name().unwrap().to_string_lossy().into_owned();
    let root = dir.join(&name);
    if root.exists() {
        bail!(tr!("le runtime {name} est déjà installé", "runtime {name} is already installed"));
    }
    // An archive downloaded by a browser is "quarantined", and tar passes
    // the mark on to the extracted files: macOS would then refuse to run
    // wine. The user is the one who chose this runtime.
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(extracted)
        .status();
    fs::rename(extracted, &root)?;
    Ok(InstalledRuntime { name, root })
}

/// Writes the embedded scripts into the build folder and runs one of them
/// ("scripts/build-wine.sh"…), with output shown in the terminal.
pub fn run_script(script: &str, args: &[String]) -> Result<()> {
    let root = build_dir()?.join("cave");
    for (rel, content) in FILES {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, content)?;
        if rel.ends_with(".sh") {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
    }
    let status = Command::new("/bin/bash")
        .arg(root.join(script))
        .args(args)
        .env("CAVE_RUNTIMES", runtimes_dir()?)
        .status()
        .with_context(|| tr!("lancement de {script}", "starting {script}"))?;
    if !status.success() {
        bail!(tr!("{script} a échoué ({status})", "{script} failed ({status})"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(version_key("wine-cx-26.10.0") > version_key("wine-cx-26.3.0"));
        assert!(version_key("wine-cx-27.0.0") > version_key("wine-cx-26.10.1"));
    }

    #[test]
    fn reads_the_latest_crossover_version() {
        let feed = r#"<item><enclosure sparkle:version="26.3.0.39832" sparkle:shortVersionString="26.3.0" /></item>
            <item><enclosure sparkle:shortVersionString="26.10.0" /></item>
            <item><enclosure sparkle:shortVersionString="25.1.1" /></item>"#;
        assert_eq!(latest_version(feed).as_deref(), Some("26.10.0"));
        assert_eq!(latest_version("<rss></rss>"), None);
        assert!(is_newer("26.10.0", "wine-cx-26.3.0"));
        assert!(!is_newer("26.3.0", "wine-cx-26.3.0"));
    }

    #[test]
    fn every_wine_patch_is_embedded() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("patches/wine");
        for entry in fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            let rel = format!("patches/wine/{name}");
            assert!(
                FILES.iter().any(|(path, _)| *path == rel),
                "{rel} is not embedded in runtimes::FILES"
            );
        }
    }
}
