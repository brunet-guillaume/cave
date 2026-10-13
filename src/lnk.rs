//! Reading Windows shortcuts (.lnk), the ones that installers put in the
//! Start menu.
//!
//! The format is documented by Microsoft ([MS-SHLLINK]): a 76-byte header,
//! then optional blocks announced by the bits of `LinkFlags`. We only read
//! what is needed to launch the program: its target (LinkInfo block), its
//! arguments, its working directory and its icon.

use anyhow::{bail, Context, Result};

#[derive(Debug, Default, PartialEq)]
pub struct Shortcut {
    /// Windows path of the target (`C:\Program Files\…\prog.exe`).
    pub target: Option<String>,
    /// Path of the target relative to the shortcut, when LinkInfo is missing.
    pub relative_path: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    /// File that contains the icon (otherwise: the target) and its index.
    pub icon_location: Option<String>,
    pub icon_index: i32,
}

const HEADER_SIZE: usize = 0x4C;
const HAS_ID_LIST: u32 = 0x01;
const HAS_LINK_INFO: u32 = 0x02;
const HAS_NAME: u32 = 0x04;
const HAS_RELATIVE_PATH: u32 = 0x08;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;
/// LinkInfoFlags: the target is on a local disk (not a network share).
const LOCAL_BASE_PATH: u32 = 0x01;

pub fn parse(data: &[u8]) -> Result<Shortcut> {
    if u32_at(data, 0)? != HEADER_SIZE as u32 {
        bail!("not a Windows shortcut");
    }
    let flags = u32_at(data, 0x14)?;
    let mut shortcut = Shortcut {
        icon_index: u32_at(data, 0x38)? as i32,
        ..Default::default()
    };

    let mut pos = HEADER_SIZE;
    if flags & HAS_ID_LIST != 0 {
        // Shell item ID list: we just skip it.
        pos += 2 + u16_at(data, pos)? as usize;
    }
    if flags & HAS_LINK_INFO != 0 {
        let info = data.get(pos..).context("truncated shortcut")?;
        shortcut.target = link_info_path(info)?;
        pos += u32_at(data, pos)? as usize;
    }

    // The strings follow, in this order, each preceded by its length in
    // characters (UTF-16 if IS_UNICODE).
    let unicode = flags & IS_UNICODE != 0;
    for (flag, field) in [
        (HAS_NAME, None),
        (HAS_RELATIVE_PATH, Some(&mut shortcut.relative_path)),
        (HAS_WORKING_DIR, Some(&mut shortcut.working_dir)),
        (HAS_ARGUMENTS, Some(&mut shortcut.arguments)),
        (HAS_ICON_LOCATION, Some(&mut shortcut.icon_location)),
    ] {
        if flags & flag == 0 {
            continue;
        }
        let chars = u16_at(data, pos)? as usize;
        let len = if unicode { chars * 2 } else { chars };
        let bytes = data.get(pos + 2..pos + 2 + len).context("truncated shortcut")?;
        pos += 2 + len;
        let text = if unicode { utf16(bytes) } else { ansi(bytes) };
        // Wine counts the terminating zero in the length: we strip it.
        let text = text.trim_end_matches('\0').to_string();
        if let Some(field) = field {
            *field = Some(text).filter(|t| !t.is_empty());
        }
    }
    Ok(shortcut)
}

/// Local path of the LinkInfo block: LocalBasePath + CommonPathSuffix, in
/// Unicode when the block's header is large enough to announce it.
fn link_info_path(info: &[u8]) -> Result<Option<String>> {
    let header_size = u32_at(info, 4)?;
    if u32_at(info, 8)? & LOCAL_BASE_PATH == 0 {
        return Ok(None);
    }
    let path = if header_size >= 0x24 {
        let base = utf16_cstr(info, u32_at(info, 0x1C)? as usize)?;
        base + &utf16_cstr(info, u32_at(info, 0x20)? as usize)?
    } else {
        let base = ansi_cstr(info, u32_at(info, 0x10)? as usize)?;
        base + &ansi_cstr(info, u32_at(info, 0x18)? as usize)?
    };
    Ok(Some(path).filter(|p| !p.is_empty()))
}

fn u16_at(data: &[u8], pos: usize) -> Result<u16> {
    let bytes = data.get(pos..pos + 2).context("truncated shortcut")?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn u32_at(data: &[u8], pos: usize) -> Result<u32> {
    let bytes = data.get(pos..pos + 4).context("truncated shortcut")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    String::from_utf16_lossy(&units)
}

/// "ANSI" strings depend on the system's code page; we read them as
/// Latin-1, which is right for Western paths only.
fn ansi(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn ansi_cstr(data: &[u8], pos: usize) -> Result<String> {
    let rest = data.get(pos..).context("truncated shortcut")?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Ok(ansi(&rest[..end]))
}

fn utf16_cstr(data: &[u8], pos: usize) -> Result<String> {
    let rest = data.get(pos..).context("truncated shortcut")?;
    let end = rest
        .as_chunks::<2>()
        .0
        .iter()
        .position(|&pair| pair == [0, 0])
        .map_or(rest.len(), |i| i * 2);
    Ok(utf16(&rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_steam_shortcut() {
        // Put in the Start menu by the Steam installer.
        let shortcut = parse(include_bytes!("../testdata/Steam.lnk")).unwrap();
        assert_eq!(
            shortcut,
            Shortcut {
                target: Some(r"C:\Program Files (x86)\Steam\steam.exe".into()),
                working_dir: Some(r"C:\Program Files (x86)\Steam".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn rejects_other_files() {
        assert!(parse(b"[InternetShortcut]\nURL=steam://rungameid/1").is_err());
        assert!(parse(&include_bytes!("../testdata/Steam.lnk")[..100]).is_err());
    }
}
