//! Compiles shim/steamwebhelper.rs into a Windows exe, which src/steam.rs then embeds.
//!
//! No need for mingw: rustc can target Windows and rust-lld, shipped with
//! rustup, can link a PE. Only the target's library is needed:
//!   rustup target add x86_64-pc-windows-msvc

use std::env;
use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "x86_64-pc-windows-msvc";
const SRC: &str = "shim/steamwebhelper.rs";

fn main() {
    println!("cargo:rerun-if-changed={SRC}");

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let print = |what: &str| -> PathBuf {
        let out = Command::new(&rustc)
            .args(["--print", what, "--target", TARGET])
            .output()
            .expect("cannot run rustc");
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
    };

    if !print("target-libdir").exists() {
        panic!(
            "\n\nThe {TARGET} target is missing (needed to compile the Steam shim).\n\
             Install it with:  rustup target add {TARGET}\n\n"
        );
    }

    let lld = print("sysroot")
        .join("lib/rustlib")
        .join(env::var("HOST").unwrap())
        .join("bin/rust-lld");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("steamwebhelper.exe");

    let status = Command::new(&rustc)
        .args(["--edition", "2021", "--crate-type", "bin", "--target", TARGET])
        .args(["-C", "panic=abort", "-C", "opt-level=s"])
        .arg(format!("-Clinker={}", lld.display()))
        .args(["-C", "linker-flavor=lld-link"])
        .args(["-C", "link-arg=/ENTRY:shim_main"])
        .args(["-C", "link-arg=/SUBSYSTEM:WINDOWS"])
        .args(["-C", "link-arg=/NODEFAULTLIB"])
        .arg("-o")
        .arg(&out)
        .arg(SRC)
        .status()
        .expect("cannot run rustc");
    if !status.success() {
        panic!("compiling {SRC} failed");
    }
}
