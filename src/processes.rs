//! Running Windows programs, as seen from macOS.
//!
//! Wine replaces each process's name with its Windows command line
//! (`C:\Program Files\…\prog.exe args`), which `ps` shows. Only the
//! program started by cave keeps the macOS path we gave to wine.
//! Two bottles have the same Windows paths: it is up to the caller to look
//! only at the running bottles.

use crate::bottle::Bottle;
use crate::runner::run_in_bottle;
use crate::tr;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Command lines of all processes, lowercased (Windows paths are
/// case-insensitive).
pub struct Processes(Vec<String>);

impl Processes {
    pub fn list() -> Result<Self> {
        let out = Command::new("ps")
            .args(["-axww", "-o", "command="])
            .output()
            .with_context(|| tr!("lancement de ps", "starting ps"))?;
        Ok(Self::from_lines(&String::from_utf8_lossy(&out.stdout)))
    }

    fn from_lines(text: &str) -> Self {
        Self(text.lines().map(|l| l.trim().trim_start_matches('"').to_lowercase()).collect())
    }

    /// Is a process running this program of the bottle?
    pub fn runs(&self, bottle: &Bottle, exe: &Path) -> bool {
        // After the program's path: nothing, or its arguments.
        self.rests(bottle, exe, "")
            .any(|rest| rest.is_empty() || rest.starts_with([' ', '"']))
    }

    /// Is a process running a program from this folder (or from its
    /// subfolders)?
    pub fn runs_from(&self, bottle: &Bottle, dir: &Path) -> bool {
        self.rests(bottle, dir, "/").next().is_some()
    }

    /// Names of the running programs that come from this folder (`game.exe`…).
    pub fn exes_from(&self, bottle: &Bottle, dir: &Path) -> Vec<String> {
        let mut exes: Vec<String> = self
            .rests(bottle, dir, "/")
            .filter_map(|rest| {
                let end = rest.find(".exe")? + ".exe".len();
                rest[..end].rsplit(['\\', '/']).next().map(str::to_string)
            })
            .collect();
        exes.sort();
        exes.dedup();
        exes
    }

    /// For each line that starts with `path` (macOS or Windows form,
    /// followed by `separator`), what comes after it.
    fn rests<'a>(&'a self, bottle: &Bottle, path: &Path, separator: &str) -> impl Iterator<Item = &'a str> {
        let mut forms = vec![format!("{}{separator}", path.to_string_lossy())];
        if let Some(windows) = bottle.windows_path(path) {
            forms.push(format!("{windows}{}", separator.replace('/', "\\")));
        }
        let forms: Vec<String> = forms.into_iter().map(|f| f.to_lowercase()).collect();
        self.0
            .iter()
            .filter_map(move |line| forms.iter().find_map(|f| line.strip_prefix(f.as_str())))
    }
}

/// Process IDs of the running Windows programs, across all bottles: those
/// whose command line starts with a drive (`C:\…`) or with a macOS path
/// inside a drive_c. Wine's services (C:\windows\…, such as explorer.exe or
/// rpcss.exe, which it starts on demand and keeps while the bottle is
/// running) do not count.
pub fn windows_pids() -> Result<HashSet<u32>> {
    let out = Command::new("ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .with_context(|| tr!("lancement de ps", "starting ps"))?;
    Ok(windows_pids_in(&String::from_utf8_lossy(&out.stdout)))
}

fn windows_pids_in(ps: &str) -> HashSet<u32> {
    ps.lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            let command = command.trim_start().trim_start_matches('"').to_lowercase();
            let drive = command.as_bytes();
            let windows = drive.len() > 2 && drive[0].is_ascii_alphabetic() && drive[1] == b':' && drive[2] == b'\\';
            let unix = command.starts_with('/') && command.contains("/drive_c/");
            let wine_service = command.starts_with("c:\\windows\\") || command.contains("/drive_c/windows/");
            ((windows || unix) && !wine_service).then(|| pid.parse().ok())?
        })
        .collect()
}

/// Asks the program `image` (`game.exe`) to close, as Windows does at
/// shutdown: taskkill without /f sends WM_CLOSE to its windows, and it can
/// still offer to save. For a program that does not respond:
/// Bottle::kill.
pub fn close(bottle: &Bottle, image: &str) -> Result<()> {
    let res = run_in_bottle(&bottle.wine()?, bottle, "taskkill", &["/im".into(), image.into()], true)?;
    if !res.status.success() {
        bail!(tr!(
            "{image} n'a pas pu être fermé ; essaie « Forcer à quitter » (voir {})",
            "{image} could not be closed; try “Force Quit” (see {})",
            res.log_file.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bottle::BottleConfig;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn bottle() -> Bottle {
        Bottle {
            root: PathBuf::from("/b"),
            config: BottleConfig {
                name: "b".into(),
                windows_version: "win10".into(),
                created_at: String::new(),
                wine: None,
                graphics: None,
                env: BTreeMap::new(),
                apps: BTreeMap::new(),
                launched: BTreeMap::new(),
            },
        }
    }

    const PS: &str = "\
/b/prefix/drive_c/Program Files (x86)/Steam/steam.exe -noverifyfiles
C:\\Program Files (x86)\\Steam\\bin\\cef\\cef.win64\\steamwebhelper.exe --type=renderer
C:\\windows\\system32\\services.exe
C:\\Program Files (x86)\\Steam\\steamapps\\common\\Graveyard Keeper 2\\Game.exe
";

    #[test]
    fn finds_programs_by_macos_or_windows_path() {
        let ps = Processes::from_lines(PS);
        let b = bottle();
        let steam = Path::new("/b/prefix/drive_c/Program Files (x86)/Steam");
        assert!(ps.runs(&b, &steam.join("steam.exe")));
        assert!(ps.runs(&b, &steam.join("bin/cef/cef.win64/STEAMWEBHELPER.exe")));
        assert!(!ps.runs(&b, &steam.join("bin/cef/cef.win64/steamwebhelper")), "prefix of another name");
        assert!(ps.runs_from(&b, &steam.join("steamapps/common/Graveyard Keeper 2")));
        assert!(!ps.runs_from(&b, &steam.join("steamapps/common/Graveyard Keeper")));
        assert_eq!(ps.exes_from(&b, &steam.join("steamapps/common/Graveyard Keeper 2")), vec!["game.exe"]);
        assert!(ps.exes_from(&b, &steam.join("steamapps/common/Other Game")).is_empty());
    }

    #[test]
    fn finds_windows_programs_by_pid() {
        let ps = "  101 /usr/bin/ssh-agent -l\n  202 C:\\Program Files\\WinRAR\\uninstall.exe /setup\n \
                  303 /b/prefix/drive_c/Program Files/7-Zip/7zFM.exe\n  404 /bin/zsh\n \
                  505 C:\\windows\\system32\\explorer.exe /desktop\n  606 cave install /b/prefix/drive_c/x.exe\n";
        assert_eq!(windows_pids_in(ps), HashSet::from([202, 303]));
    }
}
