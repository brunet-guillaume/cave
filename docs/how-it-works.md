# How Cave works

Cave is a Rust library (`src/`) shared by a command line (`src/main.rs`) and a Tauri interface (`ui/`). This page explains the problems it solves and how, for anyone who wants to understand or change the code.

- [Steam](#steam)
- [The wine-cx runtime](#the-wine-cx-runtime)
- [Applications](#applications)
- [Microsoft components](#microsoft-components)
- [Program names in the Dock](#program-names-in-the-dock)
- [Bringing programs to the front](#bringing-programs-to-the-front)
- [Games and bottle state](#games-and-bottle-state)
- [Code layout](#code-layout)

## Steam

Started as is under Wine, Steam shows a **black** window. Its interface is made with CEF (Chromium): a separate process renders it and draws it into the main process's window, which Wine cannot do from one process to another. The fix is `--in-process-gpu`, which runs that rendering inside the main process, but Steam does not pass this flag on to CEF.

So Cave replaces `bin/cef/cef.win*/steamwebhelper.exe` with an 8 KB shim (`shim/steamwebhelper.rs`), which restarts the real binary (set aside as `steamwebhelper_real.exe`) with `--no-sandbox --in-process-gpu --disable-gpu`, the same flags as CrossOver. Steam is started with `-noverifyfiles`, otherwise its startup check would put the original back; after a Steam update, the shim is reinstalled on the next start. The shim is a Windows exe without std or CRT, built by `build.rs` with `rustc --target x86_64-pc-windows-msvc` and linked by `rust-lld`.

Steam also registers itself to start with Windows (the `Run` registry key), which Wine runs every time the bottle starts: even a plain `winecfg` restarted Steam. Cave removes that entry from `user.reg` before starting Steam and after it closes.

`cave install-steam` downloads the official installer, runs it silently (`/S`), then starts Steam once in the background so it downloads its client: without that client, there is no `steamwebhelper.exe` to replace. Installed silently, Steam would start in English: before that first start, Cave writes into the registry (`HKCU\Software\Valve\Steam`, value `Language`) the first macOS language that Steam knows (`french`, `german`, `brazilian`…). It can still be changed in Steam afterwards.

## The wine-cx runtime

`cave runtime build` (`scripts/build-wine.sh`) builds Wine from the CrossOver sources: Wine with CodeWeavers' patches, including the D3DMetal/DXMT hooks of the macOS driver and MSync. Lessons learned along the way:

- **The Windows side must be built by GCC (mingw-w64)**, as CodeWeavers does. With llvm-mingw (clang), Steam opened but never connected to its servers; a bisection of the DLLs pointed at `kernelbase.dll`.
- **The x86_64 libraries** (gnutls, freetype, SDL2…) come from Wine Devel.app (Gcenx), renamed to `@rpath/<file>` so that `configure` keeps loadable names. The build targets macOS 10.15, with inotify.
- **Graphics are chosen per bottle.** DXMT lives in `<runtime>/lib/dxmt` and D3DMetal in `<runtime>/lib/apple_gptk`. Our `ntdll` patch (`patches/wine/0001`) reads `WINEDLLPATH_PREPEND` and puts these folders before Wine's DLLs, through the `prepend_dll_path` hook of the CrossOver sources.
- **D3DMetal**: `ntdll` loads `libd3dshared.dylib` from `CX_APPLEGPTK_LIBD3DSHARED_PATH` (the variable name used by the sources); `cave runtime add-d3dmetal` copies the `redist/lib` folder of the Game Porting Toolkit, as Apple's Read Me does for CrossOver.
- **GStreamer** (game videos through Media Foundation) comes from the official, universal macOS package: it is extracted without being installed, checked against its SHA-256 checksum (it is not signed), then its libraries and 17 plugins are bundled in the runtime.

Runtimes have their own releases on GitHub, `wine-cx-<version>`, built by the runtime workflow (`.github/workflows/runtime.yml`): every Monday, it reads CodeWeavers' version feed and, once the sources of a new CrossOver version are published, builds the runtime and drafts its release, with CrossOver's changes (`scripts/crossover-changelog.py`). The draft is tried, then published by hand. App releases (`v<version>`, `.github/workflows/release.yml`) only carry `Cave.app`.

When it has no runtime, the interface downloads one as soon as it opens (its "Cancel" button stops it, keeping what was received); `cave runtime install` without an archive does the same. Both ask GitHub's API for the releases, pick the `wine-cx-*.tar.xz` with the highest version, download it with `curl` (an interrupted download resumes), check the SHA-256 digest GitHub computed when the file was uploaded, then install it (`runtimes::install_published`).

With a runtime, the interface checks at startup whether a more recent one is published, and offers it; `cave runtime update` does the same. The update (`runtimes::update`) installs the new runtime, stops the bottles' open programs and moves the bottles to it (Wine updates each prefix at its next launch), then prepares its bottle template so that new bottles stay instant. The runtime the bottles used stays installed as a fallback (`cave wine <bottle> <runtime>` goes back to it); older ones that no bottle uses are deleted, with their templates.

The `cave runtime` commands run the scripts in `scripts/`, embedded in Cave along with the patches in `patches/wine`: they also work from a `cave` installed with `cargo install`. `cave runtime check` reads CodeWeavers' release feed and tells whether a newer CrossOver version (and its sources) is available.

## Applications

An installed application is found the way Windows does it, through the shortcuts its installer puts in the bottle's Start Menu and Desktop (`src/apps.rs`). Cave reads these `.lnk` files itself (`src/lnk.rs`, following Microsoft's specification) to learn the target program, its arguments and its icon. It skips uninstallers and shortcuts to anything other than an `.exe` (help, web page…). Some installers (WinRAR) exit before creating their shortcuts, leaving a second program to show the options: so Cave waits until shortcuts appear or the bottle stops.

A portable program has no shortcut: Cave copies it into `C:\Program Files` and records it in `bottle.toml`. As it often needs the files next to it (`.dll`, data), its whole folder is copied (an APFS clone, with no extra disk space), unless it sits alone in a shared folder such as Downloads or the Desktop.

The icon is extracted from the `.exe` (`src/icon.rs`): it is a resource of the PE file, available in several sizes, of which Cave keeps the largest. It is a PNG (usually 256×256) or a Windows bitmap, which Cave wraps in an `.ico` file that macOS can read. Icons are kept in `bottles/<name>/icons/`.

Each application has its own bottle: its settings only concern it, and uninstalling it means deleting its bottle, without leaving a trace. To make this fast, a new bottle is an APFS copy (`clonefile`) of a template prepared once per runtime in `templates/`: the very first creation takes a little under two minutes (`wineboot`), the following ones a fraction of a second, with no extra disk space at first.

A program can also join another one's bottle (`apps::install_into`), when it has to live alongside it: it then shares its settings. If that bottle is already running, its shutdown can no longer signal the end of the installation: Cave waits until the Windows programs that appeared since the start have closed, not counting Wine's services (`C:\windows\…`), which start on demand. And "Keep Nothing", after an installation without shortcut, never deletes a bottle that existed before.

## Microsoft components

Many Windows applications assume that Visual C++, .NET or Microsoft's fonts are installed. Cave installs them with [winetricks](https://github.com/Winetricks/winetricks), which downloads them from Microsoft and knows how to set them up under Wine (versions, DLL overrides).

winetricks needs `cabextract` to extract some packages, and macOS lacks it: `scripts/add-winetricks.sh` puts both in the runtime (`lib/winetricks`), at pinned versions with verified checksums, `cabextract` being built from its sources. Cave runs winetricks with the bottle's environment, its own cache (`~/Library/Caches/fr.umebrunet.cave/winetricks`) and no update check; `winetricks.log`, in the prefix, tells what is already installed (`src/components.rs`).

winetricks stops at .NET 9: for .NET 10, which recent applications require (ILSpy 11, for example), Cave does as winetricks does and silently runs Microsoft's official installers, 32 and 64 bits, after checking their SHA-512 hash.

## Program names in the Dock

A Windows program is a `wine` process, and the Dock called it that. The CrossOver sources already contain the workaround ("CW HACK 22144"): each program is started through a link that carries its name, created in a temporary folder (`$TMPDIR/winetemp-…/`), and that name is what macOS shows, in the Dock as in the application menu. It takes two conditions on macOS. The `WINEDLLPATH` variable must be set: Cave sets it to the runtime's DLL folder, where Wine looks for them anyway. And only programs started by another Windows program benefit from it: so Cave starts applications, installers and Steam with `start /wait /unix <exe>` (`runner::run_exe`), which makes it a separate process and waits for it to end.

Our second Wine patch (`patches/wine/0002`) picks that name. It reads `CAVE_APP_NAMES`, lines of "Windows path prefix, name" that Cave fills in at launch: the tile's name for an application ("7-Zip File Manager"), "Steam" for the whole Steam folder (its windows come from its webhelper), and each game's name for its folder. The longest prefix wins, and programs started afterwards inherit it: a game started by Steam is called "Baba Is You". Elsewhere, it is the name of the `.exe`, without the extension that CrossOver's code forgot to remove.

## Bringing programs to the front

Since macOS 14, an application can no longer bring itself in front of the others: the active application has to yield to it. An installer or a game started from Cave therefore stayed behind its window. After each launch, the interface watches for one minute the new Wine programs that open a window (`NSWorkspace`) and brings them to the front by yielding to them (`yieldActivationToApplication`).

Steam itself is not new when a game starts, yet it sometimes asks a question before launching it ("better with a controller", a license…), in its own window, which may be hidden: the game then seemed not to start. After "Play", the interface follows Steam's console log (`logs/console_log.txt`, `steam::LaunchWatch`). When Steam has been waiting for an answer for two seconds, Cave runs `steam.exe` again so that Steam shows its window, and brings it to the front.

## Games and bottle state

Games are read from Steam's files (`src/games.rs`, VDF format): `libraryfolders.vdf` for the libraries (including on another drive, through the `dosdevices` links), one `appmanifest_<appid>.acf` per game (bit 4 of `StateFlags` means "installed") and the covers cached in `appcache/librarycache/<appid>/`. The last session and the playtime come from `userdata/<account>/config/localconfig.vdf`, also for games started from Steam. "Play" runs `steam.exe -applaunch <appid>`.

To know whether a bottle is running, Cave connects to the socket of its `wineserver`, `/tmp/.wine-<uid>/server-<dev>-<inode>/socket` (a folder named after the device and inode of the prefix). The folder alone is not enough: it sometimes outlives the server.

A running bottle does not necessarily have all its programs open: Steam can run without the game. To know which game or application is open, and offer "Quit" on the right tile, Cave looks at the processes (`src/processes.rs`): Wine replaces each process's name with its Windows command line (`C:\…\steamapps\common\<game>\game.exe`), which `ps` shows. An application is open if a process runs its `.exe`, a game if a process comes from its folder. "Quit" asks the program to close the way Windows would, so it can still offer to save.

## Code layout

| Path | Role |
|---|---|
| `src/lib.rs` | the library: all the logic, shared by the CLI and the interface |
| `src/main.rs` | the command line (clap) |
| `src/bottle.rs` | create (copy of a template), load, clone, stop a bottle; "running" state |
| `src/apps.rs` | installed applications (shortcuts), installation, portable programs |
| `src/lnk.rs` | reading Windows shortcuts (`.lnk`) |
| `src/icon.rs` | extracting the icon of an `.exe` |
| `src/processes.rs` | running Windows programs, from `ps` |
| `src/components.rs` | Microsoft components (Visual C++, .NET, fonts) through winetricks |
| `src/runner.rs` | starting Wine with the right environment, stdout and stderr logs |
| `src/runtime.rs` | runtime environment: chosen graphics, GStreamer |
| `src/runtimes.rs` | installed runtimes, default runtime, embedded scripts |
| `src/steam.rs` | Steam shim, Steam installation, autostart |
| `src/games.rs` | installed Steam games (VDF files), launching |
| `src/config.rs` | data and cache folders, global Wine |
| `src/i18n.rs` | message language (English or French) and the `tr!` macro |
| `shim/steamwebhelper.rs` | the shim, a Windows exe (`no_std`) |
| `build.rs` | builds the shim, which is then embedded in Cave |
| `ui/` | Tauri interface: `src/main.rs` (commands), `front/` (HTML, CSS and JavaScript page) |
| `scripts/` | building the runtime, adding DXMT, D3DMetal and winetricks, legal notices, release archive |
| `patches/wine/` | our Wine patches (`WINEDLLPATH_PREPEND`, program names in the Dock) |

Data lives in `~/Library/Application Support/fr.umebrunet.cave` (bottles, runtimes, templates). Useful variables during development: `CAVE_HOME` (another data folder) and `CAVE_BUILD` (another build folder).
