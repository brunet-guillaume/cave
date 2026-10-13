//! Runtimes installed by cave, in `<data>/runtimes/<name>`: listing,
//! building and adding graphics layers.
//!
//! Building goes through the scripts in scripts/ and the patches in
//! patches/wine, embedded into cave at compile time: `cave runtime build`
//! therefore also works from a cave installed with `cargo install`.

use crate::bottle::Bottle;
use crate::config::data_dir;
use crate::runtime::Graphics;
use crate::tr;
use anyhow::{bail, Context, Result};
use directories::BaseDirs;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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

/// Cave's releases on GitHub: each one carries a runtime, wine-cx-<version>.tar.xz.
const RELEASES_API: &str = "https://api.github.com/repos/brunet-guillaume/cave/releases?per_page=30";

/// A runtime published in a release of Cave.
#[derive(Debug, PartialEq)]
pub struct PublishedRuntime {
    /// wine-cx-26.3.0
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// Tag of the release that carries it (v0.3.0, wine-cx-26.10.0…).
    pub release: String,
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    /// "2026-10-13T11:03:38Z": comparable as text. None for a draft.
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// "sha256:<hex>", computed by GitHub when the file was uploaded.
    digest: Option<String>,
}

/// The most recent runtime published with Cave.
pub fn latest_published() -> Result<PublishedRuntime> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json", RELEASES_API])
        .output()
        .context("starting curl")?;
    if !out.status.success() {
        bail!(tr!(
            "les versions de Cave sur GitHub sont inaccessibles ({})",
            "Cave's releases on GitHub are unreachable ({})",
            out.status
        ));
    }
    latest_runtime(&String::from_utf8_lossy(&out.stdout))?.with_context(|| {
        tr!("aucun runtime publié sur GitHub", "no runtime published on GitHub")
    })
}

/// In the list of releases (GitHub API), the runtime with the highest
/// version, outside drafts and pre-releases. Between two releases with the
/// same runtime version, the last published one (GitHub's list is not in
/// publication order).
fn latest_runtime(releases: &str) -> Result<Option<PublishedRuntime>> {
    let releases: Vec<Release> = serde_json::from_str(releases).context("GitHub releases")?;
    Ok(releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .flat_map(|r| r.assets.iter().map(move |a| (r, a)))
        .filter_map(|(r, a)| {
            let name = a.name.strip_prefix("wine-cx-")?.strip_suffix(".tar.xz")?;
            let sha256 = a.digest.as_deref()?.strip_prefix("sha256:")?;
            Some(PublishedRuntime {
                name: format!("wine-cx-{name}"),
                url: a.browser_download_url.clone(),
                size: a.size,
                sha256: sha256.to_string(),
                release: r.tag_name.clone(),
            })
            .map(|rt| ((version_key(&rt.name), r.published_at.clone()), rt))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, rt)| rt))
}

/// The curl downloading a runtime, if any. Killed by stop_download when the
/// interface quits: a child process outlives its parent, and it would go on
/// downloading alone.
static DOWNLOAD: Mutex<Option<u32>> = Mutex::new(None);

/// Stops the runtime download in progress, if any; what was received stays
/// in the cache for the next download.
pub fn stop_download() {
    if let Some(pid) = DOWNLOAD.lock().ok().and_then(|mut pid| pid.take()) {
        let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
    }
}

/// Where a runtime installation from GitHub stands.
#[derive(Clone, Copy, Debug)]
pub enum InstallStep {
    Download { received: u64, total: u64 },
    Check,
    Extract,
    /// Update: the bottles move to the new runtime.
    Switch,
    /// Update: the new runtime's bottle template is prepared.
    Prepare,
}

/// Downloads the runtime into the cache, checks it and installs it. An
/// interrupted download resumes where it stopped. Setting `cancel` stops the
/// download, keeping what was received: the result is then None.
pub fn install_published(
    rt: &PublishedRuntime,
    progress: &dyn Fn(InstallStep),
    cancel: &AtomicBool,
) -> Result<Option<InstalledRuntime>> {
    if runtimes_dir()?.join(&rt.name).exists() {
        bail!(tr!("le runtime {} est déjà installé", "runtime {} is already installed", rt.name));
    }
    let dir = crate::config::cache_dir()?.join("runtimes");
    fs::create_dir_all(&dir)?;
    let archive = dir.join(format!("{}.tar.xz", rt.name));
    let part = dir.join(format!("{}.tar.xz.part", rt.name));
    let size = |path: &Path| fs::metadata(path).map_or(0, |m| m.len());
    if size(&part) < rt.size {
        let mut curl = Command::new("curl")
            .args(["-fsSL", "--retry", "3", "-C", "-", "-o"])
            .arg(&part)
            .arg(&rt.url)
            .spawn()
            .context("starting curl")?;
        *DOWNLOAD.lock().unwrap() = Some(curl.id());
        // curl writes as it receives: the file's size is the progress.
        let status = loop {
            if let Some(status) = curl.try_wait()? {
                break status;
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = curl.kill();
                let _ = curl.wait();
                *DOWNLOAD.lock().unwrap() = None;
                return Ok(None);
            }
            progress(InstallStep::Download { received: size(&part), total: rt.size });
            std::thread::sleep(std::time::Duration::from_millis(250));
        };
        *DOWNLOAD.lock().unwrap() = None;
        if !status.success() {
            bail!(tr!("téléchargement de {} impossible ({status})", "could not download {} ({status})", rt.url));
        }
    }
    progress(InstallStep::Download { received: rt.size, total: rt.size });
    progress(InstallStep::Check);
    if sha256(&part)? != rt.sha256 {
        let _ = fs::remove_file(&part);
        bail!(tr!(
            "empreinte incorrecte pour {} : téléchargement corrompu, réessaie",
            "wrong checksum for {}: corrupted download, try again",
            rt.name
        ));
    }
    fs::rename(&part, &archive)?;
    progress(InstallStep::Extract);
    let installed = install_archive(&archive);
    let _ = fs::remove_file(&archive);
    installed.map(Some)
}

/// The latest published runtime, if it is more recent than the installed
/// ones (without a runtime, it is an installation, not an update).
pub fn update_available() -> Result<Option<PublishedRuntime>> {
    let Some(current) = default()? else {
        return Ok(None);
    };
    let latest = latest_published()?;
    Ok(is_newer(&latest.name, &current.name).then_some(latest))
}

/// Bottles on one of Cave's runtimes, which an update moves to the new one.
/// The others (global Wine, a wine binary given by its path) stay as they are.
pub fn bottles_to_update() -> Result<Vec<Bottle>> {
    let dir = runtimes_dir()?;
    Ok(Bottle::list()?
        .into_iter()
        .filter(|b| b.config.wine.as_ref().is_some_and(|wine| wine.starts_with(&dir)))
        .collect())
}

/// Installs a newer runtime and moves the bottles to it, stopping their open
/// programs first. The runtime they used stays installed as a fallback
/// (`cave wine <bottle> <runtime>` goes back to it); older ones that no
/// bottle uses are deleted, with their templates. None if cancelled.
pub fn update(
    rt: &PublishedRuntime,
    progress: &dyn Fn(InstallStep),
    cancel: &AtomicBool,
) -> Result<Option<InstalledRuntime>> {
    let previous = default()?;
    let Some(new) = install_published(rt, progress, cancel)? else {
        return Ok(None);
    };
    progress(InstallStep::Switch);
    for mut bottle in bottles_to_update()? {
        if bottle.config.wine.as_deref() == Some(new.wine().as_path()) {
            continue;
        }
        // Stopped with the old runtime's wineserver, before changing it.
        if bottle.is_running() {
            bottle.kill()?;
            bottle.wait_stopped()?;
        }
        bottle.config.wine = Some(new.wine());
        bottle.save()?;
    }
    // Not fatal: the template is otherwise prepared with the next new bottle.
    progress(InstallStep::Prepare);
    let _ = Bottle::prepare_template(&new, &|_| {});
    prune(&[Some(new.name.as_str()), previous.as_ref().map(|p| p.name.as_str())])?;
    Ok(Some(new))
}

/// Deletes the runtimes that are neither kept nor used by a bottle, with
/// their templates.
fn prune(keep: &[Option<&str>]) -> Result<()> {
    let used: Vec<PathBuf> = Bottle::list()?.into_iter().filter_map(|b| b.config.wine).collect();
    for rt in list()? {
        if keep.contains(&Some(rt.name.as_str())) || used.iter().any(|wine| wine.starts_with(&rt.root)) {
            continue;
        }
        fs::remove_dir_all(&rt.root)?;
        let _ = fs::remove_dir_all(Bottle::template_dir(&rt.name)?);
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let out = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .context("starting shasum")?;
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().next() {
        Some(sum) if out.status.success() => Ok(sum.to_string()),
        _ => bail!("shasum {}: {}", path.display(), String::from_utf8_lossy(&out.stderr)),
    }
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
    fn finds_the_latest_published_runtime() {
        let asset = |name: &str, digest: Option<&str>| {
            format!(
                r#"{{"name":"{name}","browser_download_url":"https://example.com/{name}","size":272,"digest":{}}}"#,
                digest.map_or("null".to_string(), |d| format!("\"{d}\""))
            )
        };
        let release = |draft: bool, prerelease: bool, published: &str, assets: &[String]| {
            format!(
                r#"{{"tag_name":"v1","published_at":"{published}","draft":{draft},"prerelease":{prerelease},"assets":[{}]}}"#,
                assets.join(",")
            )
        };
        let day = |d: u32| format!("2026-10-{d:02}T10:00:00Z");
        let releases = format!(
            "[{},{},{},{}]",
            release(false, true, &day(4), &[asset("wine-cx-27.0.0.tar.xz", Some("sha256:beta"))]),
            release(false, false, &day(3), &[asset("Cave.zip", Some("sha256:app")), asset("wine-cx-26.10.0.tar.xz", Some("sha256:abc"))]),
            release(false, false, &day(2), &[asset("wine-cx-26.3.0.tar.xz", Some("sha256:old"))]),
            release(true, false, &day(5), &[asset("wine-cx-28.0.0.tar.xz", Some("sha256:draft"))]),
        );
        assert_eq!(
            latest_runtime(&releases).unwrap(),
            Some(PublishedRuntime {
                name: "wine-cx-26.10.0".into(),
                url: "https://example.com/wine-cx-26.10.0.tar.xz".into(),
                size: 272,
                sha256: "abc".into(),
                release: "v1".into(),
            })
        );
        // Same version in two releases: the last published, whatever the order.
        for (first, second) in [(day(1), day(2)), (day(2), day(1))] {
            let new = if first > second { "first" } else { "second" };
            let rebuilt = format!(
                "[{},{}]",
                release(false, false, &first, &[asset("wine-cx-26.3.0.tar.xz", Some("sha256:first"))]),
                release(false, false, &second, &[asset("wine-cx-26.3.0.tar.xz", Some("sha256:second"))]),
            );
            assert_eq!(latest_runtime(&rebuilt).unwrap().unwrap().sha256, new);
        }
        let unchecked = format!("[{}]", release(false, false, &day(1), &[asset("wine-cx-26.3.0.tar.xz", None)]));
        assert_eq!(latest_runtime(&unchecked).unwrap(), None, "no checksum, no download");
        assert!(latest_runtime("{\"message\":\"API rate limit exceeded\"}").is_err());
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
