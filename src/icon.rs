//! A Windows program's icon, extracted from its .exe.
//!
//! Icons are resources of the PE file (the format of .exe and .dll files):
//! - an RT_GROUP_ICON resource describes an icon and its various sizes;
//! - each size is an RT_ICON resource: a PNG image (256×256, usually) or a
//!   Windows bitmap without a file header.
//!
//! We keep the largest size. A PNG image is written as is; a bitmap is
//! wrapped in a single-image .ico file, which macOS can display.

use anyhow::{bail, Context, Result};

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// An image ready to be written to a file, with its extension.
pub struct Image {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

/// The program's icon number `index` (as in a shortcut: n ≥ 0 is the
/// file's n-th icon, n < 0 the icon with identifier -n).
pub fn extract(exe: &[u8], index: i32) -> Result<Image> {
    let pe = Pe::parse(exe)?;
    // First level of the resource directory: the resource types.
    let types = pe.entries(0)?;
    let group = types
        .iter()
        .find(|e| e.id == Some(RT_GROUP_ICON))
        .context("no icon in this program")?;
    let icons = pe.entries(group.offset)?;
    let chosen = if index >= 0 {
        icons.get(index as usize)
    } else {
        icons.iter().find(|e| e.id == Some(index.unsigned_abs()))
    };
    let dir = pe.first_leaf(chosen.context("icon not found")?.offset)?;

    // GRPICONDIR: 6-byte header, then 14 bytes per size.
    let count = u16_at(dir, 4)? as usize;
    let mut best: Option<&[u8]> = None;
    for i in 0..count {
        let entry = dir.get(6 + i * 14..6 + (i + 1) * 14).context("truncated icon")?;
        if best.is_none_or(|b| size_rank(entry) > size_rank(b)) {
            best = Some(entry);
        }
    }
    let entry = best.context("empty icon")?;
    let id = u16_at(entry, 12)? as u32;

    let images = types
        .iter()
        .find(|e| e.id == Some(RT_ICON))
        .context("icon images not found")?;
    let image = pe
        .entries(images.offset)?
        .into_iter()
        .find(|e| e.id == Some(id))
        .context("icon image not found")?;
    let data = pe.first_leaf(image.offset)?;

    if data.starts_with(PNG_SIGNATURE) {
        return Ok(Image { bytes: data.to_vec(), extension: "png" });
    }
    // .ico file: ICONDIR (6 bytes), one ICONDIRENTRY (16 bytes) that reuses
    // the first 12 bytes of the group's entry, then the image.
    let mut ico = vec![0, 0, 1, 0, 1, 0];
    ico.extend_from_slice(&entry[..12]);
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(data);
    Ok(Image { bytes: ico, extension: "ico" })
}

/// Ranking of a size: the widest, then the richest in colors.
/// A width of 0 means 256 pixels.
fn size_rank(entry: &[u8]) -> (u32, u16) {
    let width = match entry[0] {
        0 => 256,
        w => w as u32,
    };
    (width, u16::from_le_bytes([entry[6], entry[7]]))
}

/// A PE file, just enough to read its resources.
struct Pe<'a> {
    data: &'a [u8],
    /// Sections: virtual address, size, position in the file.
    sections: Vec<(u32, u32, u32)>,
    /// Virtual address of the resource directory.
    resources: u32,
}

/// An entry of a resource directory: numeric identifier (None for a name)
/// and position, within the resources, of what it points to.
struct Entry {
    id: Option<u32>,
    offset: u32,
}

impl<'a> Pe<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        if !data.starts_with(b"MZ") {
            bail!("not a Windows program");
        }
        let pe = u32_at(data, 0x3C)? as usize;
        if data.get(pe..pe + 4) != Some(b"PE\0\0") {
            bail!("not a Windows program");
        }
        let section_count = u16_at(data, pe + 6)? as usize;
        let optional = pe + 24;
        let optional_size = u16_at(data, pe + 20)? as usize;
        // The data directories follow the fixed part of the optional
        // header, which is 16 bytes longer in 64-bit (PE32+).
        let directories = match u16_at(data, optional)? {
            0x10B => optional + 96,
            0x20B => optional + 112,
            magic => bail!("unknown PE header ({magic:#x})"),
        };
        // The resource directory is the third one.
        let resources = u32_at(data, directories + 2 * 8)?;
        if resources == 0 {
            bail!("no resources in this program");
        }
        let mut sections = vec![];
        for i in 0..section_count {
            let s = optional + optional_size + i * 40;
            let virtual_size = u32_at(data, s + 8)?;
            let raw_size = u32_at(data, s + 16)?;
            sections.push((u32_at(data, s + 12)?, virtual_size.max(raw_size), u32_at(data, s + 20)?));
        }
        Ok(Pe { data, sections, resources })
    }

    /// Position in the file of a virtual address.
    fn file_offset(&self, rva: u32) -> Result<usize> {
        let (va, _, raw) = self
            .sections
            .iter()
            .find(|(va, size, _)| rva >= *va && rva - va < *size)
            .context("address outside the sections")?;
        Ok((rva - va + raw) as usize)
    }

    /// Entries of the resource directory located at `offset`.
    fn entries(&self, offset: u32) -> Result<Vec<Entry>> {
        let dir = self.file_offset(self.resources + (offset & 0x7FFF_FFFF))?;
        let count = u16_at(self.data, dir + 12)? as usize + u16_at(self.data, dir + 14)? as usize;
        (0..count)
            .map(|i| {
                let e = dir + 16 + i * 8;
                let name = u32_at(self.data, e)?;
                Ok(Entry {
                    id: (name & 0x8000_0000 == 0).then_some(name),
                    offset: u32_at(self.data, e + 4)?,
                })
            })
            .collect()
    }

    /// Data of the first resource under `offset`: we descend through the
    /// subdirectories (usually the language) to a leaf.
    fn first_leaf(&self, mut offset: u32) -> Result<&'a [u8]> {
        for _ in 0..4 {
            if offset & 0x8000_0000 == 0 {
                let leaf = self.file_offset(self.resources + offset)?;
                let start = self.file_offset(u32_at(self.data, leaf)?)?;
                let size = u32_at(self.data, leaf + 4)? as usize;
                return self.data.get(start..start + size).context("truncated resource");
            }
            offset = self.entries(offset)?.first().context("empty resource")?.offset;
        }
        bail!("resources nested too deeply")
    }
}

fn u16_at(data: &[u8], pos: usize) -> Result<u16> {
    let bytes = data.get(pos..pos + 2).context("truncated file")?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn u32_at(data: &[u8], pos: usize) -> Result<u32> {
    let bytes = data.get(pos..pos + 4).context("truncated file")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal .exe (PE32+) whose only section holds the resources: an
    /// icon group with two sizes, 16×16 (bitmap) and 256×256 (PNG).
    fn exe_with_icons(small: &[u8], large: &[u8]) -> Vec<u8> {
        const SECTION_VA: u32 = 0x1000;
        const SECTION_FILE: usize = 0x200;

        // Resource directory: 16-byte header then entries (identifier,
        // position); the high bit marks a subdirectory.
        fn dir(entries: &[(u32, u32)]) -> Vec<u8> {
            let mut d = vec![0u8; 12];
            d.extend_from_slice(&0u16.to_le_bytes());
            d.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            for (id, offset) in entries {
                d.extend_from_slice(&id.to_le_bytes());
                d.extend_from_slice(&offset.to_le_bytes());
            }
            d
        }
        const SUB: u32 = 0x8000_0000;

        let mut group = vec![0, 0, 1, 0, 2, 0];
        for (width, bits, size, id) in [(16u8, 32u16, small.len(), 1u16), (0, 32, large.len(), 2)] {
            group.extend_from_slice(&[width, width, 0, 0, 1, 0]);
            group.extend_from_slice(&bits.to_le_bytes());
            group.extend_from_slice(&(size as u32).to_le_bytes());
            group.extend_from_slice(&id.to_le_bytes());
        }

        // Layout: root, types, then per type the names and the languages,
        // the leaves (IMAGE_RESOURCE_DATA_ENTRY) and finally the data.
        let root = dir(&[(RT_ICON, SUB | 0x20), (RT_GROUP_ICON, SUB | 0x40)]);
        let icons = dir(&[(1, SUB | 0x58), (2, SUB | 0x70)]);
        let groups = dir(&[(1, SUB | 0x88)]);
        let lang = |leaf: u32| dir(&[(0x409, leaf)]);
        let mut rsrc = [root, icons, groups].concat();
        assert_eq!(rsrc.len(), 0x58);
        rsrc.extend(lang(0xA0));
        rsrc.extend(lang(0xB0));
        rsrc.extend(lang(0xC0));
        assert_eq!(rsrc.len(), 0xA0);
        let data_start = 0xD0u32;
        let mut payload = vec![];
        for blob in [small, large, &group] {
            let rva = SECTION_VA + data_start + payload.len() as u32;
            rsrc.extend_from_slice(&rva.to_le_bytes());
            rsrc.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            rsrc.extend_from_slice(&[0; 8]);
            payload.extend_from_slice(blob);
        }
        rsrc.extend(vec![0; data_start as usize - rsrc.len()]);
        rsrc.extend(payload);

        let mut exe = vec![0u8; SECTION_FILE];
        exe[..2].copy_from_slice(b"MZ");
        exe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        exe[0x40..0x44].copy_from_slice(b"PE\0\0");
        let coff = 0x44;
        exe[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        exe[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
        let optional = coff + 20;
        exe[optional..optional + 2].copy_from_slice(&0x20Bu16.to_le_bytes());
        let resources = optional + 112 + 16;
        exe[resources..resources + 4].copy_from_slice(&SECTION_VA.to_le_bytes());
        exe[resources + 4..resources + 8].copy_from_slice(&(rsrc.len() as u32).to_le_bytes());
        let section = optional + 240;
        exe[section..section + 5].copy_from_slice(b".rsrc");
        exe[section + 8..section + 12].copy_from_slice(&(rsrc.len() as u32).to_le_bytes());
        exe[section + 12..section + 16].copy_from_slice(&SECTION_VA.to_le_bytes());
        exe[section + 16..section + 20].copy_from_slice(&(rsrc.len() as u32).to_le_bytes());
        exe[section + 20..section + 24].copy_from_slice(&(SECTION_FILE as u32).to_le_bytes());
        exe.extend(rsrc);
        exe
    }

    #[test]
    fn keeps_the_largest_size() {
        let png = [PNG_SIGNATURE, b"image 256"].concat();
        let exe = exe_with_icons(b"bitmap 16", &png);

        let image = extract(&exe, 0).unwrap();

        assert_eq!(image.extension, "png");
        assert_eq!(image.bytes, png);
    }

    #[test]
    fn wraps_bitmaps_in_an_ico_file() {
        let exe = exe_with_icons(b"bitmap 16", b"bitmap 256");

        let image = extract(&exe, 0).unwrap();

        assert_eq!(image.extension, "ico");
        assert_eq!(&image.bytes[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(image.bytes[6], 0, "width 0 = 256 pixels");
        assert_eq!(u32::from_le_bytes(image.bytes[18..22].try_into().unwrap()), 22);
        assert_eq!(&image.bytes[22..], b"bitmap 256");
    }

    #[test]
    fn finds_an_icon_by_index_or_id() {
        let exe = exe_with_icons(b"a", b"b");
        assert!(extract(&exe, 0).is_ok());
        assert!(extract(&exe, -1).is_ok(), "identifier 1");
        assert!(extract(&exe, 1).is_err(), "there is only one icon");
        assert!(extract(b"MZ not a program", 0).is_err());
    }
}
