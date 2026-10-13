use anyhow::{bail, Context, Result};
use cave::bottle::Bottle;
use cave::config::{self, Config};
use cave::i18n::{self, Lang};
use cave::runtime::{Graphics, Runtime};
use cave::{apps, components, games, runner, runtimes, steam};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

// The help of each command comes from the `///` comments below: it is in
// English, like every message of the command line.

#[derive(Parser)]
#[command(name = "cave", version, about = "Wine bottle manager for macOS")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Checks the setup: Wine binary, version, data folder
    Doctor,
    /// Sets the global configuration
    Config {
        /// Path to the wine binary to use
        #[arg(long)]
        wine: Option<PathBuf>,
    },
    /// Creates a new bottle. Without options: instant copy of a template
    /// (newest runtime built by cave, Windows 10, best graphics)
    Create {
        name: String,
        /// Emulated Windows version (win10, win7, winxp…)
        #[arg(long)]
        windows: Option<String>,
        /// Runtime of this bottle: name (cave runtime list) or path to a wine
        /// binary
        #[arg(long)]
        wine: Option<PathBuf>,
    },
    /// Asks an application or a game (part of its name) to close
    Quit { name: String, app: String },
    /// Installs a program (.exe or .msi) in a new bottle
    Install {
        file: PathBuf,
        /// Program without an installer: copied as is into the bottle
        #[arg(long)]
        portable: bool,
        /// In an existing bottle, next to its programs (plugin, mod tool…)
        #[arg(long, value_name = "BOTTLE")]
        into: Option<String>,
    },
    /// Microsoft components of a bottle (Visual C++, .NET, fonts…); with
    /// component names (winetricks verbs), installs them
    Components { name: String, verbs: Vec<String> },
    /// Lists the applications of a bottle (Start Menu shortcuts…)
    Apps { name: String },
    /// Starts an application (part of its name) and waits until it closes
    Launch {
        name: String,
        app: String,
        /// Don't print the output (it stays in the log)
        #[arg(long, short)]
        quiet: bool,
    },
    /// Renames a bottle (its open programs are closed)
    Rename { name: String, new_name: String },
    /// Copies a bottle (APFS copy: instant, no extra disk space)
    Clone { name: String, new_name: String },
    /// Changes (or removes with --global) the Wine runtime of a bottle
    Wine {
        name: String,
        /// Runtime name (cave runtime list) or path to a wine binary
        path: Option<PathBuf>,
        /// Go back to the Wine of the global config
        #[arg(long, conflicts_with = "path")]
        global: bool,
    },
    /// Chooses the Direct3D renderer of a bottle (without argument: shows it)
    Graphics {
        name: String,
        #[arg(value_enum)]
        backend: Option<Graphics>,
    },
    /// Stops every Wine process of a bottle (wineserver -k)
    Kill { name: String },
    /// Lists the bottles
    List,
    /// Details of a bottle
    Info { name: String },
    /// Runs an .exe or a Wine tool (winecfg, regedit, cmd…) in a bottle
    Run {
        name: String,
        /// Path to an .exe, or name of a tool built into Wine
        program: String,
        /// Arguments passed to the program
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Don't print the output (it stays in the log)
        #[arg(long, short)]
        quiet: bool,
    },
    /// Starts Steam in a bottle, with the black-window workaround
    Steam {
        name: String,
        /// Arguments passed to steam.exe
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Don't print the output (it stays in the log)
        #[arg(long, short)]
        quiet: bool,
    },
    /// Lists the Steam games installed in a bottle
    Games { name: String },
    /// Starts a Steam game (appid or part of its name), starting Steam if needed
    Play {
        name: String,
        game: String,
        /// Don't print the output (it stays in the log)
        #[arg(long, short)]
        quiet: bool,
    },
    /// Installs Steam in a bottle (official installer, then first start)
    InstallSteam { name: String },
    /// Sets (KEY=VALUE) or removes (KEY=) an environment variable of the bottle
    Set { name: String, var: String },
    /// Prints the `export` lines to use the bottle by hand in a shell
    Env { name: String },
    /// Wine runtimes built by cave: list, build, graphics layers
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
    /// Deletes a bottle and everything in it
    Delete {
        name: String,
        /// Confirms the deletion
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum RuntimeCmd {
    /// Lists the installed runtimes, their layers and the bottles using them
    List,
    /// Builds Wine from the CrossOver sources published by CodeWeavers (~20 min)
    Build {
        /// Version of the CrossOver sources
        #[arg(default_value = "26.3.0")]
        version: String,
    },
    /// Looks for a newer CrossOver version to build the runtime from
    Check,
    /// Installs the latest runtime published with Cave, if it is newer, and
    /// moves the bottles to it (their open programs are closed)
    Update,
    /// Installs a runtime from its archive (.tar.xz published with cave);
    /// without an archive, downloads the latest one from Cave's releases
    Install { archive: Option<PathBuf> },
    /// Adds winetricks (Microsoft components) and cabextract to a runtime
    AddWinetricks { runtime: String },
    /// Adds DXMT (Direct3D 10/11 → Metal) to a runtime
    AddDxmt {
        runtime: String,
        /// DXMT version
        #[arg(default_value = "v0.80")]
        version: String,
    },
    /// Adds D3DMetal (Direct3D 11/12 → Metal) to a runtime, from the .dmg of
    /// the Game Porting Toolkit downloaded from Apple (or a .tar.xz of a
    /// runtime's lib/apple_gptk folder)
    AddD3dmetal { runtime: String, dmg: PathBuf },
}

/// Downloads and installs the latest runtime published with Cave.
fn download_runtime() -> Result<runtimes::InstalledRuntime> {
    let rt = runtimes::latest_published()?;
    println!("Downloading {} ({} MB)…", rt.name, rt.size / 1_000_000);
    let never = std::sync::atomic::AtomicBool::new(false);
    runtimes::install_published(&rt, &install_progress(), &never)?.context("download cancelled")
}

/// Shows a runtime installation: the download's progress on one line, then
/// each step.
fn install_progress() -> impl Fn(runtimes::InstallStep) {
    use runtimes::InstallStep;
    use std::io::Write;
    let shown = std::cell::Cell::new(None);
    move |step| match step {
        InstallStep::Download { received, total } => {
            let percent = received * 100 / total.max(1);
            if shown.replace(Some(percent)) != Some(percent) {
                print!("\r  {percent} % ({} / {} MB)", received / 1_000_000, total / 1_000_000);
                let _ = std::io::stdout().flush();
            }
        }
        InstallStep::Check => println!("\nChecking the SHA-256 checksum…"),
        InstallStep::Extract => println!("Installing…"),
        InstallStep::Switch => println!("Moving the bottles to the new runtime…"),
        InstallStep::Prepare => println!("Preparing Windows for new bottles (about a minute)…"),
    }
}

fn main() {
    // The command line speaks English, library messages included.
    i18n::set(Lang::En);
    if let Err(e) = run(Cli::parse()) {
        eprintln!("✗ {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Doctor => doctor(),
        Cmd::Config { wine } => {
            let mut cfg = Config::load()?;
            if let Some(w) = wine {
                cfg.wine_bin = Some(w);
                cfg.save()?;
            }
            println!("{}", toml::to_string_pretty(&cfg)?);
            Ok(())
        }
        Cmd::Create { name, windows, wine } => {
            let progress = |step: &str| println!("{step}…");
            let b = match (wine, windows) {
                (None, None) => Bottle::create_ready(&name, &progress)?,
                (wine, windows) => {
                    let wine = match wine {
                        Some(w) => runtimes::resolve(&w)?,
                        None => runtimes::default()?
                            .context("no runtime installed: cave runtime build")?
                            .wine(),
                    };
                    let windows = windows.as_deref().unwrap_or("win10");
                    Bottle::create_and_init(&name, windows, Some(wine), &progress)?
                }
            };
            println!("✓ Bottle “{name}” ready: {}", b.root.display());
            println!("  Try: cave run {name} winecfg");
            Ok(())
        }
        Cmd::Install { file, portable, into } => {
            let progress = |step: &str| println!("{step}…");
            if let Some(name) = into {
                let mut b = Bottle::load(&name)?;
                let installed = if portable {
                    vec![apps::add_portable(&mut b, &file)?]
                } else {
                    apps::install_into(&b, &file, &progress)?
                };
                if installed.is_empty() {
                    bail!(
                        "no new application in “{name}” (installation cancelled?). \
                         For a program without an installer: cave install --portable --into {name} {}",
                        file.display()
                    );
                }
                println!("✓ Installed in the “{name}” bottle:");
                for app in installed {
                    println!("  {}  →  cave launch {name} {:?}", app.name, app.name);
                }
                return Ok(());
            }
            let name = apps::bottle_name_for(&file)?;
            let installed = if portable {
                let mut b = Bottle::create_ready(&name, &progress)?;
                vec![apps::add_portable(&mut b, &file)?]
            } else {
                let (b, installed) = apps::install(&name, &file, &progress)?;
                if installed.is_empty() {
                    b.delete()?;
                    bail!(
                        "no application found after the installation (cancelled?). \
                         For a program without an installer: cave install --portable {}",
                        file.display()
                    );
                }
                installed
            };
            println!("✓ Installed in the “{name}” bottle:");
            for app in installed {
                println!("  {}  →  cave launch {name} {:?}", app.name, app.name);
            }
            Ok(())
        }
        Cmd::Components { name, verbs } => {
            let b = Bottle::load(&name)?;
            if verbs.is_empty() {
                let installed = components::installed(&b);
                for c in components::COMPONENTS {
                    let mark = if installed.iter().any(|v| v == c.verb) { "✓" } else { " " };
                    println!("{mark} {:<16} {:<22} {}", c.verb, c.name(), c.description());
                }
                println!("Install: cave components {name} vcrun2022 (or any other winetricks component)");
                return Ok(());
            }
            components::install(&b, &verbs, &|step| println!("{step}…"))?;
            println!("✓ Installed in “{name}”");
            Ok(())
        }
        Cmd::Apps { name } => {
            let installed = apps::installed(&Bottle::load(&name)?)?;
            if installed.is_empty() {
                println!("No application in “{name}”.");
            }
            for app in installed {
                println!("{:<30} {}", app.name, app.exe.display());
            }
            Ok(())
        }
        Cmd::Quit { name, app } => {
            let b = Bottle::load(&name)?;
            let closed = match apps::find(&b, &app) {
                Ok(found) => apps::quit(&b, &found).map(|_| found.name),
                Err(not_an_app) => match games::find(&b, &app) {
                    Ok(game) => games::quit(&b, &game).map(|_| game.name),
                    Err(_) => Err(not_an_app),
                },
            }?;
            println!("✓ {closed}: asked to close");
            Ok(())
        }
        Cmd::Launch { name, app, quiet } => {
            let b = Bottle::load(&name)?;
            let app = apps::find(&b, &app)?;
            println!("▶ {}", app.name);
            finish(apps::launch(&b, &app, quiet)?)
        }
        Cmd::Rename { name, new_name } => {
            Bottle::load(&name)?.rename(&new_name)?;
            println!("✓ “{name}” renamed to “{new_name}”");
            Ok(())
        }
        Cmd::Clone { name, new_name } => {
            let copy = Bottle::load(&name)?.clone_as(&new_name)?;
            println!("✓ “{name}” copied to “{new_name}”: {}", copy.root.display());
            Ok(())
        }
        Cmd::Runtime { cmd } => runtime_cmd(cmd),
        Cmd::Wine { name, path, global } => {
            let mut b = Bottle::load(&name)?;
            if global {
                b.config.wine = None;
            } else if let Some(p) = path {
                b.config.wine = Some(runtimes::resolve(&p)?);
            }
            let resolved = b.wine()?;
            b.save()?;
            let source = if b.config.wine.is_some() { "bottle" } else { "global" };
            println!("{name} → {} ({source})", resolved.display());
            Ok(())
        }
        Cmd::Graphics { name, backend } => {
            let mut b = Bottle::load(&name)?;
            if let Some(g) = backend {
                b.set_graphics(g)?;
            }
            match b.config.graphics {
                Some(g) => println!("{name} → {g}"),
                None => println!("{name} → the runtime's default renderer"),
            }
            Ok(())
        }
        Cmd::Kill { name } => {
            let status = Bottle::load(&name)?.kill()?;
            println!("wineserver -k → {status}");
            Ok(())
        }
        Cmd::List => {
            let bottles = Bottle::list()?;
            if bottles.is_empty() {
                println!("No bottle. Create one: cave create steam");
            }
            for b in bottles {
                let wine = b
                    .config
                    .wine
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(global)".into());
                let state = if b.is_running() { "● running" } else { "" };
                println!("{:<20} {:<8} {:<10} {}", b.config.name, b.config.windows_version, state, wine);
            }
            Ok(())
        }
        Cmd::Info { name } => {
            let b = Bottle::load(&name)?;
            println!("Folder  : {}", b.root.display());
            println!("Prefix  : {}", b.prefix().display());
            println!("drive_c : {}", b.prefix().join("drive_c").display());
            println!("Logs    : {}", b.logs_dir().display());
            match b.wine() {
                Ok(w) => println!("Wine    : {}", w.display()),
                Err(e) => println!("Wine    : ✗ {e}"),
            }
            println!("\n{}", toml::to_string_pretty(&b.config)?);
            Ok(())
        }
        Cmd::Run { name, program, args, quiet } => {
            let b = Bottle::load(&name)?;
            let wine = b.wine()?;
            let res = runner::run_in_bottle(&wine, &b, &program, &args, quiet)?;
            finish(res)
        }
        Cmd::Steam { name, args, quiet } => {
            let res = steam::launch(&Bottle::load(&name)?, &args, quiet)?;
            finish(res)
        }
        Cmd::Games { name } => {
            let installed = games::installed(&Bottle::load(&name)?)?;
            if installed.is_empty() {
                println!("No game installed in “{name}”.");
            }
            for g in installed {
                println!("{:<10} {}", g.appid, g.name);
            }
            Ok(())
        }
        Cmd::Play { name, game, quiet } => {
            let b = Bottle::load(&name)?;
            let g = games::find(&b, &game)?;
            println!("▶ {} ({})", g.name, g.appid);
            finish(games::play(&b, &g, quiet)?)
        }
        Cmd::InstallSteam { name } => {
            steam::install(&Bottle::load(&name)?, &|step| println!("{step}…"))?;
            println!("✓ Steam installed. Start it: cave steam {name}");
            Ok(())
        }
        Cmd::Set { name, var } => {
            let mut b = Bottle::load(&name)?;
            let Some((k, v)) = var.split_once('=') else {
                bail!("expected format: KEY=VALUE (or KEY= to remove)");
            };
            if v.is_empty() {
                b.config.env.remove(k);
            } else {
                b.config.env.insert(k.to_string(), v.to_string());
            }
            b.save()?;
            println!("{}", toml::to_string_pretty(&b.config.env)?);
            Ok(())
        }
        Cmd::Env { name } => {
            let b = Bottle::load(&name)?;
            let wine = b.wine()?;
            for (k, v) in b.env_vars(&Runtime::for_wine(&wine))? {
                println!("export {k}={}", shell_quote(&v));
            }
            Ok(())
        }
        Cmd::Delete { name, yes } => {
            let b = Bottle::load(&name)?;
            if !yes {
                bail!(
                    "this will delete {} and everything installed in it. \
                     Run again with --yes to confirm.",
                    b.root.display()
                );
            }
            b.delete()?;
            println!("Bottle “{name}” deleted.");
            Ok(())
        }
    }
}

fn runtime_cmd(cmd: RuntimeCmd) -> Result<()> {
    match cmd {
        RuntimeCmd::List => {
            let bottles = Bottle::list()?;
            let installed = runtimes::list()?;
            if installed.is_empty() {
                println!("No runtime. Build one: cave runtime build");
            }
            for rt in &installed {
                let users: Vec<&str> = bottles
                    .iter()
                    .filter(|b| b.config.wine.as_ref().is_some_and(|w| w.starts_with(&rt.root)))
                    .map(|b| b.config.name.as_str())
                    .collect();
                println!(
                    "{:<20} {:<12} layers: {:<26} bottles: {}",
                    rt.name,
                    rt.version().unwrap_or_else(|| "?".into()),
                    rt.layers().join(" "),
                    if users.is_empty() { "-".into() } else { users.join(" ") }
                );
            }
            Ok(())
        }
        RuntimeCmd::Update => {
            let Some(rt) = runtimes::update_available()? else {
                let current = runtimes::default()?.map(|rt| rt.name);
                println!("✓ Up to date: {}", current.as_deref().unwrap_or("no runtime installed (cave runtime install)"));
                return Ok(());
            };
            let previous = runtimes::default()?.map(|rt| rt.name);
            println!("Updating to {} ({} MB)…", rt.name, rt.size / 1_000_000);
            let never = std::sync::atomic::AtomicBool::new(false);
            let new = runtimes::update(&rt, &install_progress(), &never)?.context("download cancelled")?;
            println!("✓ The bottles now use {}.", new.name);
            if let Some(previous) = previous {
                println!("  {previous} stays installed: cave wine <bottle> {previous} goes back to it.");
            }
            Ok(())
        }
        RuntimeCmd::Check => {
            let latest = runtimes::latest_crossover()?;
            let installed = runtimes::default()?.map(|rt| rt.name);
            println!("Latest CrossOver  : {latest}");
            println!("Installed runtime : {}", installed.as_deref().unwrap_or("none"));
            if installed.as_deref().is_some_and(|name| !runtimes::is_newer(&latest, name)) {
                println!("✓ Up to date.");
            } else if runtimes::sources_published(&latest) {
                println!("→ New version: cave runtime build {latest}");
            } else {
                println!("The sources of {latest} are not published yet: {}", runtimes::sources_url(&latest));
            }
            Ok(())
        }
        RuntimeCmd::Install { archive } => {
            let rt = match archive {
                Some(archive) => runtimes::install_archive(&archive)?,
                None => download_runtime()?,
            };
            println!("✓ Runtime {} installed: {}", rt.name, rt.root.display());
            Ok(())
        }
        RuntimeCmd::Build { version } => runtimes::run_script("scripts/build-wine.sh", &[version]),
        RuntimeCmd::AddWinetricks { runtime } => {
            runtimes::run_script("scripts/add-winetricks.sh", &[runtime])
        }
        RuntimeCmd::AddDxmt { runtime, version } => {
            runtimes::run_script("scripts/add-dxmt.sh", &[runtime, version])
        }
        RuntimeCmd::AddD3dmetal { runtime, dmg } => runtimes::run_script(
            "scripts/add-d3dmetal.sh",
            &[dmg.to_string_lossy().into_owned(), runtime],
        ),
    }
}

fn doctor() -> Result<()> {
    println!("Data    : {}", config::data_dir()?.display());
    let wine = Config::load()?.resolve_wine()?;
    println!("Wine    : {}", wine.display());
    let out = Command::new(&wine).arg("--version").output()?;
    println!("Version : {}", String::from_utf8_lossy(&out.stdout).trim());
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        let rosetta = Command::new("/usr/bin/arch")
            .args(["-x86_64", "/usr/bin/true"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        println!(
            "Rosetta : {}",
            if rosetta { "OK" } else { "missing → softwareupdate --install-rosetta" }
        );
    }
    Ok(())
}

/// Prints the outcome of a run and passes on its exit code.
fn finish(res: runner::RunResult) -> Result<()> {
    println!("\n→ done ({}) — log: {}", res.status, res.log_file.display());
    if !res.status.success() {
        std::process::exit(res.status.code().unwrap_or(1));
    }
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
