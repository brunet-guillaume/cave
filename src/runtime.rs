//! Environment to set up for running Wine with the chosen renderer.
//!
//! cave uses the runtime it builds itself (scripts/build-wine.sh, from the
//! CrossOver sources published by CodeWeavers). The Direct3D renderer is
//! chosen per bottle (`cave graphics`): our ntdll patch (patches/wine) reads
//! `WINEDLLPATH_PREPEND` and puts `<runtime>/lib/dxmt` or
//! `<runtime>/lib/apple_gptk/wine` (D3DMetal) ahead of Wine's DLLs.

use crate::tr;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Direct3D renderer of a bottle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Graphics {
    /// Wine translates Direct3D to OpenGL (limited to OpenGL 4.1 on macOS)
    Wined3d,
    /// DXMT: Direct3D 10/11 → Metal
    Dxmt,
    /// D3DMetal (Apple's Game Porting Toolkit): Direct3D 11/12 → Metal
    D3dmetal,
}

impl fmt::Display for Graphics {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Graphics::Wined3d => "wined3d",
            Graphics::Dxmt => "dxmt",
            Graphics::D3dmetal => "d3dmetal",
        })
    }
}

/// A Wine runtime; `root` is the folder that contains bin/ and lib/.
pub struct Runtime {
    root: PathBuf,
}

impl Runtime {
    /// The runtime a wine binary belongs to (`<root>/bin/wine`).
    pub fn for_wine(wine: &Path) -> Self {
        let root = wine
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Runtime { root }
    }

    /// Folder of the runtime (the one that contains bin/ and lib/).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Variables to add for running Wine with the requested renderer
    /// (`None`: the default one, wined3d).
    pub fn env_vars(&self, graphics: Option<Graphics>) -> Result<Vec<(String, String)>> {
        let var = |k: &str, v: String| (k.to_string(), v);
        let mut vars = vec![];
        let dlls = self.root.join("lib/wine");
        if dlls.is_dir() {
            // The Dock would name every program "wine", the launcher's name.
            // A fix in the CrossOver sources (CW HACK 22144) instead launches
            // each program through a link that bears its name
            // ("WinRAR.exe"), in a temporary folder; but on macOS, it only
            // kicks in when WINEDLLPATH is set. This folder is the one where
            // Wine already looks for its DLLs: nothing else changes.
            vars.push(var("WINEDLLPATH", dlls.to_string_lossy().into_owned()));
        }
        let plugins = self.root.join("lib/gstreamer-1.0");
        if plugins.is_dir() {
            // Plugins bundled by scripts/build-wine.sh: otherwise GStreamer
            // would look for the system's.
            vars.push(var(
                "GST_PLUGIN_SYSTEM_PATH",
                plugins.to_string_lossy().into_owned(),
            ));
            vars.push(var(
                "GST_REGISTRY",
                self.root
                    .join("gstreamer-registry.x86_64.bin")
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        vars.extend(graphics_vars(&self.root, graphics)?);
        Ok(vars)
    }
}

/// Variables specific to the chosen renderer.
fn graphics_vars(root: &Path, graphics: Option<Graphics>) -> Result<Vec<(String, String)>> {
    let var = |k: &str, v: String| (k.to_string(), v);
    match graphics {
        None | Some(Graphics::Wined3d) => Ok(vec![]),
        Some(Graphics::Dxmt) => {
            let dxmt = root.join("lib/dxmt");
            if !dxmt.is_dir() {
                bail!(tr!(
                    "DXMT n'est pas installé dans {} (cave runtime add-dxmt)",
                    "DXMT is not installed in {} (cave runtime add-dxmt)",
                    root.display()
                ));
            }
            Ok(vec![var(
                "WINEDLLPATH_PREPEND",
                dxmt.to_string_lossy().into_owned(),
            )])
        }
        Some(Graphics::D3dmetal) => {
            let gptk = root.join("lib/apple_gptk");
            if !gptk.join("wine").is_dir() {
                bail!(tr!(
                    "D3DMetal n'est pas installé dans {} (cave runtime add-d3dmetal)",
                    "D3DMetal is not installed in {} (cave runtime add-d3dmetal)",
                    root.display()
                ));
            }
            let path = |rel: &str| gptk.join(rel).to_string_lossy().into_owned();
            Ok(vec![
                var("WINEDLLPATH_PREPEND", path("wine")),
                // Variable name from the CrossOver sources: ntdll loads this
                // library at startup, D3DMetal's .so files depend on it.
                var(
                    "CX_APPLEGPTK_LIBD3DSHARED_PATH",
                    path("external/libd3dshared.dylib"),
                ),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn runtime_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cave-rt-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        dir
    }

    fn value<'a>(vars: &'a [(String, String)], key: &str) -> Option<&'a str> {
        vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn plain_runtime_uses_dxmt_from_its_lib_dir() {
        let root = runtime_dir("dxmt");
        fs::create_dir_all(root.join("lib/dxmt")).unwrap();
        let rt = Runtime::for_wine(&root.join("bin/wine"));

        let vars = rt.env_vars(Some(Graphics::Dxmt)).unwrap();

        let expected = root.join("lib/dxmt");
        assert_eq!(
            value(&vars, "WINEDLLPATH_PREPEND"),
            Some(&*expected.to_string_lossy())
        );
        assert!(rt.env_vars(None).unwrap().is_empty());
    }

    #[test]
    fn plain_runtime_points_gstreamer_to_its_plugins() {
        let root = runtime_dir("gst");
        let rt = Runtime::for_wine(&root.join("bin/wine"));
        assert!(rt.env_vars(None).unwrap().is_empty());

        fs::create_dir_all(root.join("lib/gstreamer-1.0")).unwrap();
        let vars = rt.env_vars(None).unwrap();

        let plugins = root.join("lib/gstreamer-1.0");
        assert_eq!(
            value(&vars, "GST_PLUGIN_SYSTEM_PATH"),
            Some(&*plugins.to_string_lossy())
        );
        assert!(value(&vars, "GST_REGISTRY").is_some());
    }

    #[test]
    fn plain_runtime_without_dxmt_is_an_error() {
        let rt = Runtime::for_wine(&runtime_dir("no-dxmt").join("bin/wine"));
        assert!(rt.env_vars(Some(Graphics::Dxmt)).is_err());
    }

    #[test]
    fn plain_runtime_uses_d3dmetal_from_its_lib_dir() {
        let root = runtime_dir("gptk");
        let rt = Runtime::for_wine(&root.join("bin/wine"));
        assert!(rt.env_vars(Some(Graphics::D3dmetal)).is_err());

        fs::create_dir_all(root.join("lib/apple_gptk/wine")).unwrap();
        let vars = rt.env_vars(Some(Graphics::D3dmetal)).unwrap();

        let gptk = root.join("lib/apple_gptk");
        let wine = gptk.join("wine");
        let shared = gptk.join("external/libd3dshared.dylib");
        assert_eq!(
            value(&vars, "WINEDLLPATH_PREPEND"),
            Some(&*wine.to_string_lossy())
        );
        assert_eq!(
            value(&vars, "CX_APPLEGPTK_LIBD3DSHARED_PATH"),
            Some(&*shared.to_string_lossy())
        );
    }
}
