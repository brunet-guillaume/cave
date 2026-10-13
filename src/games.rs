//! Games installed by Steam in a bottle, read from Steam's files.
//!
//! - `steamapps/libraryfolders.vdf` lists the libraries (Windows paths);
//! - each library holds one `steamapps/appmanifest_<appid>.acf` per game;
//! - `appcache/librarycache/<appid>/` keeps the artwork (cover, icon…).
//!
//! These files are in Valve's VDF format: "key" "value" pairs and blocks
//! between braces. We only need the pairs.

use crate::bottle::Bottle;
use crate::processes::{self, Processes};
use crate::steam::{self, steam_dir};
use crate::tr;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Steam tools that show up as installed games.
const NOT_GAMES: &[u32] = &[
    228980, // Steamworks Common Redistributables
];

pub struct Game {
    pub appid: u32,
    pub name: String,
    /// The game's folder (steamapps/common/<installdir> of its library).
    pub dir: PathBuf,
    /// Vertical cover (300x450), if Steam has it cached.
    pub cover: Option<PathBuf>,
    /// Small square icon, if Steam has it cached.
    pub icon: Option<PathBuf>,
}

/// A game's last session and playtime, as recorded by Steam.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayStats {
    /// Date of the last session, in seconds since 1970 (0: never).
    pub last_played: i64,
    pub minutes: u32,
}

/// The bottle's play statistics, by appid. Steam keeps them for each
/// account in userdata/<account>/config/localconfig.vdf, under
/// Software/Valve/Steam/apps/<appid>. They also count sessions started from
/// Steam itself; several accounts add up.
pub fn play_stats(bottle: &Bottle) -> HashMap<u32, PlayStats> {
    let mut stats = HashMap::new();
    let Ok(users) = fs::read_dir(steam_dir(bottle).join("userdata")) else {
        return stats;
    };
    for user in users.flatten() {
        if let Ok(text) = fs::read_to_string(user.path().join("config/localconfig.vdf")) {
            add_play_stats(&mut stats, &text);
        }
    }
    stats
}

fn add_play_stats(stats: &mut HashMap<u32, PlayStats>, localconfig: &str) {
    for (blocks, key, value) in vdf_entries(localconfig) {
        let [.., apps, id] = blocks.as_slice() else { continue };
        if !apps.eq_ignore_ascii_case("apps") {
            continue;
        }
        let (Ok(appid), Ok(n)) = (id.parse::<u32>(), value.parse::<i64>()) else {
            continue;
        };
        let entry = stats.entry(appid).or_default();
        if key.eq_ignore_ascii_case("LastPlayed") {
            entry.last_played = entry.last_played.max(n);
        } else if key.eq_ignore_ascii_case("Playtime") {
            entry.minutes += u32::try_from(n).unwrap_or(0);
        }
    }
}

/// Games fully installed in the bottle, sorted by name.
pub fn installed(bottle: &Bottle) -> Result<Vec<Game>> {
    let steam = steam_dir(bottle);
    let mut games = vec![];
    for library in libraries(bottle, &steam) {
        let Ok(entries) = fs::read_dir(library.join("steamapps")) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !(file.starts_with("appmanifest_") && file.ends_with(".acf")) {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            if let Some(mut game) = parse_manifest(&text) {
                game.dir = library.join("steamapps/common").join(&game.dir);
                games.push(with_artwork(game, &steam));
            }
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    games.dedup_by_key(|g| g.appid);
    Ok(games)
}

/// Finds a game by appid or by name (case-insensitive).
pub fn find(bottle: &Bottle, query: &str) -> Result<Game> {
    let mut games = installed(bottle)?;
    if let Ok(appid) = query.parse::<u32>() {
        if let Some(i) = games.iter().position(|g| g.appid == appid) {
            return Ok(games.swap_remove(i));
        }
    }
    let q = query.to_lowercase();
    let mut matches: Vec<Game> = games
        .into_iter()
        .filter(|g| g.name.to_lowercase().contains(&q))
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!(tr!(
            "aucun jeu installé ne correspond à « {query} » (cave games {})",
            "no installed game matches “{query}” (cave games {})",
            bottle.config.name
        )),
        _ => bail!(tr!(
            "plusieurs jeux correspondent à « {query} » : {}",
            "several games match “{query}”: {}",
            matches.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Launches a game through Steam (which starts if needed); returns when
/// Steam exits, or right away if Steam was already running.
pub fn play(bottle: &Bottle, game: &Game, quiet: bool) -> Result<crate::runner::RunResult> {
    steam::launch(bottle, &["-applaunch".into(), game.appid.to_string()], quiet)
}

/// Asks the game to close: its running programs are those from its folder
/// (processes::close). Steam stays open.
pub fn quit(bottle: &Bottle, game: &Game) -> Result<()> {
    let exes = if bottle.is_running() {
        Processes::list()?.exes_from(bottle, &game.dir)
    } else {
        vec![]
    };
    if exes.is_empty() {
        bail!(tr!("{} n'est pas lancé", "{} is not running", game.name));
    }
    for exe in exes {
        processes::close(bottle, &exe)?;
    }
    Ok(())
}

/// Steam library folders, converted to macOS paths.
fn libraries(bottle: &Bottle, steam: &Path) -> Vec<PathBuf> {
    let vdf = fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = vdf_pairs(&vdf)
        .into_iter()
        .filter(|(k, _)| k == "path")
        .filter_map(|(_, v)| bottle.unix_path(&v))
        .collect();
    if dirs.is_empty() {
        dirs.push(steam.to_path_buf());
    }
    dirs
}

fn parse_manifest(text: &str) -> Option<Game> {
    let pairs = vdf_pairs(text);
    let get = |key: &str| pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str());
    let appid: u32 = get("appid")?.parse().ok()?;
    // StateFlags: bit 4 means "fully installed".
    let flags: u32 = get("StateFlags")?.parse().ok()?;
    if flags & 4 == 0 || NOT_GAMES.contains(&appid) {
        return None;
    }
    Some(Game {
        appid,
        name: get("name")?.to_string(),
        // Relative to the library: completed by installed().
        dir: PathBuf::from(get("installdir")?),
        cover: None,
        icon: None,
    })
}

/// Names of the vertical cover in Steam's cache, depending on the client
/// version and the game's age.
const COVERS: &[&str] = &["library_capsule.jpg", "library_600x900.jpg"];

/// Artwork cached in librarycache/<appid>/: the cover, stored in a subfolder
/// named after a hash or directly at the root depending on the game; the
/// icon, an image at the root named after its hash (40 hex characters), not
/// to be confused with header.jpg and the others.
fn with_artwork(mut game: Game, steam: &Path) -> Game {
    let dir = steam.join("appcache/librarycache").join(game.appid.to_string());
    let Ok(entries) = fs::read_dir(&dir) else {
        return game;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    let cover_in = |d: &Path| COVERS.iter().map(|name| d.join(name)).find(|p| p.is_file());
    game.cover = cover_in(&dir).or_else(|| entries.iter().filter(|p| p.is_dir()).find_map(|d| cover_in(d)));
    game.icon = entries.into_iter().find(|p| {
        let hashed = p
            .file_stem()
            .is_some_and(|s| s.len() == 40 && s.to_string_lossy().chars().all(|c| c.is_ascii_hexdigit()));
        hashed && p.extension().is_some_and(|e| e == "jpg") && p.is_file()
    });
    game
}

/// The "key" "value" pairs of a VDF file, in order (blocks are flattened).
fn vdf_pairs(text: &str) -> Vec<(String, String)> {
    vdf_entries(text).into_iter().map(|(_, k, v)| (k, v)).collect()
}

/// The "key" "value" pairs of a VDF file, in order, each with the names of
/// the blocks that contain it. A block is a name alone on its line, then
/// its content between braces. Strings may contain \" and \\.
fn vdf_entries(text: &str) -> Vec<(Vec<String>, String, String)> {
    let mut entries = vec![];
    let mut blocks: Vec<String> = vec![];
    let mut next_block = String::new();
    for line in text.lines() {
        let strings = quoted_strings(line);
        match strings.as_slice() {
            [key, value] => entries.push((blocks.clone(), key.clone(), value.clone())),
            [name] => next_block = name.clone(),
            _ => {}
        }
        let line = line.trim();
        if line == "{" || (strings.len() == 1 && line.ends_with('{')) {
            blocks.push(std::mem::take(&mut next_block));
        } else if line == "}" {
            blocks.pop();
        }
    }
    entries
}

fn quoted_strings(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => s.extend(chars.next()),
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#""AppState"
{
	"appid"		"4358690"
	"Universe"		"1"
	"LauncherPath"		"C:\\Program Files (x86)\\Steam\\steam.exe"
	"name"		"Graveyard Keeper 2"
	"StateFlags"		"4"
	"installdir"		"Graveyard Keeper 2"
	"InstalledDepots"
	{
		"4358691"
		{
			"manifest"		"123"
		}
	}
}"#;

    #[test]
    fn reads_an_installed_game() {
        let game = parse_manifest(MANIFEST).unwrap();
        assert_eq!(game.appid, 4358690);
        assert_eq!(game.name, "Graveyard Keeper 2");
    }

    #[test]
    fn skips_partial_installs_and_redistributables() {
        let downloading = MANIFEST.replace(r#""StateFlags"		"4""#, r#""StateFlags"		"1026""#);
        assert!(parse_manifest(&downloading).is_none());
        let redist = MANIFEST.replace("4358690", "228980");
        assert!(parse_manifest(&redist).is_none());
    }

    #[test]
    fn reads_play_stats_by_appid() {
        let localconfig = r#""UserLocalConfigStore"
{
	"Software"
	{
		"Valve"
		{
			"Steam"
			{
				"apps"
				{
					"4358690"
					{
						"LastPlayed"		"1791829681"
						"Playtime2wks"		"1065"
						"Playtime"		"1065"
						"cloud"
						{
							"last_sync_state"		"synchronized"
						}
					}
					"620"
					{
						"LastPlayed"		"1704760839"
						"Playtime"		"751"
					}
				}
			}
		}
	}
	"apptickets"
	{
		"4358690"		"3200000004"
	}
}"#;
        let mut stats = HashMap::new();
        add_play_stats(&mut stats, localconfig);
        add_play_stats(&mut stats, &localconfig.replace("1791829681", "1700000000"));

        assert_eq!(stats.len(), 2);
        assert_eq!(stats[&4358690], PlayStats { last_played: 1791829681, minutes: 2130 });
        assert_eq!(stats[&620].minutes, 1502);
    }

    /// A fake librarycache/<appid>/ cache, with these files.
    fn library_cache(name: &str, files: &[&str]) -> PathBuf {
        let steam = std::env::temp_dir().join(format!("cave-artwork-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&steam);
        for file in files {
            let path = steam.join("appcache/librarycache/1").join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"jpg").unwrap();
        }
        steam
    }

    fn artwork(steam: &Path) -> (Option<String>, Option<String>) {
        let game = Game { appid: 1, name: String::new(), dir: PathBuf::new(), cover: None, icon: None };
        let game = with_artwork(game, steam);
        let rel = |p: Option<PathBuf>| {
            p.map(|p| p.strip_prefix(steam.join("appcache/librarycache/1")).unwrap().to_string_lossy().into_owned())
        };
        (rel(game.cover), rel(game.icon))
    }

    #[test]
    fn finds_covers_in_every_steam_layout() {
        let icon = "9b9e1838838c21ee443359efe30b6a56cf0531dd.jpg";
        // Graveyard Keeper 2, BALL x PIT, Baba Is You.
        for (name, cover) in [
            ("capsule", "7f05/library_capsule.jpg"),
            ("600x900", "b6ca/library_600x900.jpg"),
            ("root", "library_600x900.jpg"),
        ] {
            let steam = library_cache(name, &[cover, icon, "header.jpg", "1d72/library_hero.jpg"]);
            assert_eq!(artwork(&steam), (Some(cover.to_string()), Some(icon.to_string())), "{name}");
        }
        let steam = library_cache("nothing", &["header.jpg"]);
        assert_eq!(artwork(&steam), (None, None));
    }

    #[test]
    fn unescapes_vdf_strings() {
        let pairs = vdf_pairs(r#"		"path"		"C:\\Program Files (x86)\\Steam""#);
        assert_eq!(pairs, vec![("path".into(), r"C:\Program Files (x86)\Steam".into())]);
    }
}
