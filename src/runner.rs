//! Running Wine in a bottle, with log capture.
//!
//! stdout and stderr are read in two threads: each line is shown in the
//! terminal AND written to `logs/<timestamp>.log`.
//! (Wine writes most of its diagnostics to stderr.)

use crate::bottle::Bottle;
use crate::runtime::Runtime;
use crate::tr;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub struct RunResult {
    pub status: ExitStatus,
    pub log_file: PathBuf,
}

/// Runs `wine <program> <args...>` in the bottle.
///
/// `program` can be a Unix path to a .exe (Wine knows how to translate it)
/// or a program built into Wine: winecfg, regedit, notepad, cmd, explorer…
pub fn run_in_bottle(
    wine: &Path,
    bottle: &Bottle,
    program: &str,
    args: &[String],
    quiet: bool,
) -> Result<RunResult> {
    // Many games look for their files relative to the current directory:
    // so we run from the exe's folder when the program is one.
    let as_path = Path::new(program);
    let dir = as_path
        .parent()
        .filter(|p| as_path.is_file() && !p.as_os_str().is_empty());
    let launch = Launch { dir, label: label(program), env: vec![] };
    run(wine, bottle, program, args, launch, quiet)
}

/// Runs a Windows program of the bottle (macOS path of a .exe) through
/// `start /wait /unix`, rather than directly. It thus becomes a separate
/// process, which Wine starts through a link bearing its name ("CW HACK
/// 22144" fix from the CrossOver sources): the Dock and the application menu
/// show "WinRAR" instead of "wine". /wait waits for it to end, like a
/// direct launch.
///
/// `names` chooses these names: pairs (Windows path prefix, name), passed
/// to Wine in CAVE_APP_NAMES (patches/wine/0002). The longest prefix wins,
/// and programs launched afterwards inherit it; elsewhere, it is the name
/// of the .exe.
pub fn run_exe(
    wine: &Path,
    bottle: &Bottle,
    exe: &Path,
    args: &[String],
    names: &[(String, String)],
    quiet: bool,
) -> Result<RunResult> {
    let mut start_args = vec!["/wait".to_string(), "/unix".to_string(), exe.to_string_lossy().into_owned()];
    start_args.extend_from_slice(args);
    let mut env = vec![];
    if !names.is_empty() {
        env.push(("CAVE_APP_NAMES".to_string(), app_names(names)));
    }
    let launch = Launch { dir: exe.parent(), label: label(&exe.to_string_lossy()), env };
    run(wine, bottle, "start", &start_args, launch, quiet)
}

/// Value of CAVE_APP_NAMES: one `prefix<TAB>name` line per pair, with no
/// tab or newline in the names.
fn app_names(names: &[(String, String)]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n'], " ");
    names
        .iter()
        .map(|(prefix, name)| format!("{}\t{}", clean(prefix), clean(name)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Log name: last component of the path, without extension. `program` can
/// be a Windows path (C:\…\steam.exe), which Path cannot split.
fn label(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem).to_string()
}

/// What sets a launch apart: current directory, log name, environment
/// variables added to the bottle's.
struct Launch<'a> {
    dir: Option<&'a Path>,
    label: String,
    env: Vec<(String, String)>,
}

fn run(wine: &Path, bottle: &Bottle, program: &str, args: &[String], launch: Launch, quiet: bool) -> Result<RunResult> {
    let Launch { dir, label, env: extra_env } = launch;
    let mut env = bottle.env_vars(&Runtime::for_wine(wine))?;
    env.extend(extra_env);
    let mut cmd = Command::new(wine);
    cmd.arg(program).args(args);
    cmd.envs(env.iter().cloned());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let log_file = bottle.logs_dir().join(format!("{stamp}-{label}.log"));
    std::fs::create_dir_all(bottle.logs_dir())?;
    let log = Arc::new(Mutex::new(File::create(&log_file)?));

    {
        let mut f = log.lock().unwrap();
        writeln!(f, "# cave run — {}", chrono::Local::now().to_rfc3339())?;
        writeln!(f, "# wine: {}", wine.display())?;
        writeln!(f, "# cmd: {program} {}", args.join(" "))?;
        for (k, v) in &env {
            writeln!(f, "# env: {k}={v}")?;
        }
        writeln!(f)?;
    }

    let mut child = cmd
        .spawn()
        .with_context(|| tr!("impossible de lancer {}", "could not start {}", wine.display()))?;

    let out = child.stdout.take().expect("stdout piped");
    let err = child.stderr.take().expect("stderr piped");
    let (done_tx, done_rx) = mpsc::channel();
    pump(out, "out", Arc::clone(&log), quiet, done_tx.clone());
    pump(err, "err", Arc::clone(&log), quiet, done_tx);

    let status = child.wait()?;

    // A program left in the background (e.g. Steam, started by wineboot
    // through the registry's Run key) inherits our pipes and keeps them
    // open: we give the last lines only a few seconds to arrive.
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..2 {
        let left = deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            break;
        }
    }

    writeln!(log.lock().unwrap(), "\n# exit: {status}")?;
    Ok(RunResult { status, log_file })
}

fn pump<R: Read + Send + 'static>(
    stream: R,
    tag: &'static str,
    log: Arc<Mutex<File>>,
    quiet: bool,
    done: Sender<()>,
) {
    thread::spawn(move || {
        // Wine can output non-UTF-8 bytes: we read raw bytes, then convert
        // them lossily rather than crash.
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        while let Ok(n) = reader.read_until(b'\n', &mut buf) {
            if n == 0 {
                break;
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end();
            if !quiet {
                if tag == "err" {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
            }
            if let Ok(mut f) = log.lock() {
                let _ = writeln!(f, "[{tag}] {line}");
            }
            buf.clear();
        }
        let _ = done.send(());
    });
}
