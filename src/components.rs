//! Microsoft components (Visual C++, .NET, fonts…) that many Windows
//! applications need. Most are installed by winetricks, which ships with the
//! runtime along with cabextract to extract Microsoft's packages
//! (scripts/add-winetricks.sh). Those winetricks doesn't know yet (.NET 10)
//! are installed by cave, which runs the official installers silently, the
//! way winetricks does.

use crate::bottle::Bottle;
use crate::config::{cache_dir, download, wineserver_for};
use crate::runner::run_in_bottle;
use crate::runtime::Runtime;
use crate::tr;
use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::process::{Command, Stdio};

pub struct Component {
    /// Component name, the winetricks one (its "verb") when winetricks knows it.
    pub verb: &'static str,
    /// Name and description, in French then English (see name, description).
    pub names: [&'static str; 2],
    pub descriptions: [&'static str; 2],
    pub source: Source,
}

impl Component {
    pub fn name(&self) -> &'static str {
        self.names[lang_index()]
    }

    pub fn description(&self) -> &'static str {
        self.descriptions[lang_index()]
    }
}

fn lang_index() -> usize {
    match crate::i18n::get() {
        crate::i18n::Lang::Fr => 0,
        crate::i18n::Lang::En => 1,
    }
}

pub enum Source {
    Winetricks,
    /// Official installers, run one after the other with `args`.
    Installers {
        /// URL and SHA-512 checksum published by the vendor.
        files: &'static [(&'static str, &'static str)],
        args: &'static [&'static str],
        /// Proof of installation: a folder (relative to drive_c) that
        /// contains an entry starting with this version prefix.
        installed: (&'static str, &'static str),
    },
}

/// The components offered in the interface; the command line accepts any
/// winetricks verb.
pub const COMPONENTS: &[Component] = &[
    Component {
        verb: "vcrun2022",
        names: ["Visual C++ 2015-2022", "Visual C++ 2015-2022"],
        descriptions: [
            "Bibliothèques demandées par la plupart des applications et des jeux récents.",
            "Libraries required by most recent applications and games.",
        ],
        source: Source::Winetricks,
    },
    Component {
        verb: "dotnetdesktop10",
        names: [".NET 10 (bureau)", ".NET 10 (desktop)"],
        descriptions: [
            "Pour les applications .NET les plus récentes (depuis fin 2025).",
            "For the most recent .NET applications (since late 2025).",
        ],
        source: Source::Installers {
            // https://builds.dotnet.microsoft.com/dotnet/release-metadata/10.0/releases.json
            files: &[
                (
                    "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/10.0.12/windowsdesktop-runtime-10.0.12-win-x64.exe",
                    "0b907e9312867172a4eb82f4b5ab3f7c2d25e27d8349546d77eeb5d5b8cbecab9ebebfed189b13e9669dc578d892756c548366da539d47b3ddac5efcb7ae72fe",
                ),
                (
                    "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/10.0.12/windowsdesktop-runtime-10.0.12-win-x86.exe",
                    "e3581e55c5dd345df242af978bd5bd22871975af7cb3d7664567fbfc774e1809e2cd4ee36c42c47c928cbf11dac5f6cf475fdf6ed3b6aed45867ba93421cb6de",
                ),
            ],
            args: &["/install", "/quiet", "/norestart"],
            installed: ("Program Files/dotnet/shared/Microsoft.WindowsDesktop.App", "10."),
        },
    },
    Component {
        verb: "dotnetdesktop8",
        names: [".NET 8 (bureau)", ".NET 8 (desktop)"],
        descriptions: ["Pour les applications .NET de 2023 à 2025.", "For .NET applications from 2023 to 2025."],
        source: Source::Winetricks,
    },
    Component {
        verb: "dotnet48",
        names: [".NET Framework 4.8", ".NET Framework 4.8"],
        descriptions: [
            "Pour les applications .NET plus anciennes. Remplace Wine Mono ; une dizaine de minutes.",
            "For older .NET applications. Replaces Wine Mono; about ten minutes.",
        ],
        source: Source::Winetricks,
    },
    Component {
        verb: "corefonts",
        names: ["Polices Microsoft", "Microsoft fonts"],
        descriptions: [
            "Arial, Times New Roman, Verdana… pour que le texte s'affiche comme prévu.",
            "Arial, Times New Roman, Verdana… so that text looks as intended.",
        ],
        source: Source::Winetricks,
    },
];

/// Readable name of a verb (the verb itself if it isn't in the list).
pub fn name_of(verb: &str) -> &str {
    COMPONENTS.iter().find(|c| c.verb == verb).map_or(verb, |c| c.name())
}

/// Components already installed in the bottle: winetricks records them, one
/// per line, in winetricks.log at the root of the prefix; the others are
/// recognized by their files.
pub fn installed(bottle: &Bottle) -> Vec<String> {
    let mut verbs: Vec<String> = fs::read_to_string(bottle.prefix().join("winetricks.log"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    for c in COMPONENTS {
        if let Source::Installers { installed: (dir, version), .. } = c.source {
            let dir = bottle.prefix().join("drive_c").join(dir);
            let present = fs::read_dir(dir)
                .map(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with(version)))
                .unwrap_or(false);
            if present {
                verbs.push(c.verb.to_string());
            }
        }
    }
    verbs
}

/// Installs components, one after the other; `progress` receives each step.
/// winetricks waits for the bottle to be idle (wineserver -w): open programs
/// are closed first.
pub fn install(bottle: &Bottle, verbs: &[String], progress: &dyn Fn(&str)) -> Result<()> {
    if let Some(bad) = verbs
        .iter()
        .find(|v| v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        bail!(tr!("composant inconnu « {bad} »", "unknown component “{bad}”"));
    }
    let wine = bottle.wine()?;
    let runtime = Runtime::for_wine(&wine);
    let tools = runtime.root().join("lib/winetricks");
    let script = tools.join("winetricks");
    if !script.is_file() {
        bail!(tr!(
            "ce runtime n'a pas winetricks : cave runtime add-winetricks {}",
            "this runtime has no winetricks: cave runtime add-winetricks {}",
            runtime.root().file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    if bottle.is_running() {
        bottle.kill()?;
        bottle.wait_stopped()?;
    }

    // winetricks finds wine, wineserver and cabextract through these variables
    // and the PATH; its cache goes into cave's.
    let mut env = bottle.env_vars(&runtime)?;
    let bin = wine.parent().with_context(|| tr!("binaire wine sans dossier", "wine binary without a folder"))?;
    let text = |p: &std::path::Path| p.to_string_lossy().into_owned();
    env.extend([
        ("WINE".to_string(), text(&wine)),
        ("WINESERVER".to_string(), text(&wineserver_for(&wine))),
        ("PATH".to_string(), format!("{}:{}:/usr/bin:/bin:/usr/sbin:/sbin", text(&tools), text(bin))),
        ("W_CACHE".to_string(), text(&cache_dir()?.join("winetricks"))),
        ("WINETRICKS_LATEST_VERSION_CHECK".to_string(), "disabled".to_string()),
    ]);

    fs::create_dir_all(bottle.logs_dir())?;
    for verb in verbs {
        progress(&tr!("Installation de {}", "Installing {}", name_of(verb)));
        let source = COMPONENTS.iter().find(|c| c.verb == verb).map(|c| &c.source);
        if let Some(Source::Installers { files, args, .. }) = source {
            run_installers(bottle, &wine, files, args)?;
            continue;
        }
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let log_file = bottle.logs_dir().join(format!("{stamp}-winetricks-{verb}.log"));
        let log = File::create(&log_file)?;
        let status = Command::new("/bin/bash")
            .arg(&script)
            .args(["-q", verb])
            .envs(env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()
            .with_context(|| tr!("lancement de winetricks", "starting winetricks"))?;
        if !status.success() {
            bail!(tr!(
                "échec de l'installation de {}, voir {}",
                "installing {} failed, see {}",
                name_of(verb),
                log_file.display()
            ));
        }
    }
    Ok(())
}

/// Downloads (once, into the cache), verifies and silently runs a
/// component's official installers.
fn run_installers(bottle: &Bottle, wine: &std::path::Path, files: &[(&str, &str)], args: &[&str]) -> Result<()> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    for (url, sha512) in files {
        let name = url.rsplit('/').next().with_context(|| tr!("adresse sans nom de fichier", "URL without a file name"))?;
        let file = cache_dir()?.join("components").join(name);
        if !file.is_file() {
            download(url, &file)?;
        }
        if sha512_of(&file)? != *sha512 {
            let _ = fs::remove_file(&file);
            bail!(tr!(
                "empreinte incorrecte pour {name} : téléchargement corrompu, réessaie",
                "wrong checksum for {name}: corrupted download, try again"
            ));
        }
        let res = run_in_bottle(wine, bottle, &file.to_string_lossy(), &args, true)?;
        // 3010: installed, restart requested (irrelevant under Wine).
        if !res.status.success() && res.status.code() != Some(3010) {
            bail!(tr!("échec de l'installation de {name}, voir {}", "installing {name} failed, see {}", res.log_file.display()));
        }
    }
    Ok(())
}

fn sha512_of(file: &std::path::Path) -> Result<String> {
    let out = Command::new("/usr/bin/shasum").args(["-a", "512"]).arg(file).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_known_components_only() {
        assert_eq!(name_of("vcrun2022"), "Visual C++ 2015-2022");
        assert_eq!(name_of("d3dx9"), "d3dx9");
    }
}
