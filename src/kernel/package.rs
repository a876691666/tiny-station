//! Kernel **package** support: a `.zip` archive of module files.
//!
//! This is the portable unit users upload from the management UI (and that CI
//! produces). It contains `main.lua` plus any `require`-able modules; a common
//! single top-level directory is stripped so both `zip -r pkg .` and "zip the
//! folder" layouts work.

use crate::kernel::types::ModuleSet;
use std::collections::BTreeMap;
use std::io::Read;

/// Caps so a hostile/accidental package cannot exhaust memory.
const MAX_ARCHIVE: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 4 * 1024 * 1024;
const MAX_FILE: usize = 1024 * 1024;
const MAX_FILES: usize = 256;

/// Parse a `.zip` kernel package into a [`ModuleSet`].
pub fn from_zip(bytes: &[u8]) -> Result<ModuleSet, String> {
    if bytes.len() > MAX_ARCHIVE {
        return Err("内核包过大".to_string());
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("解析内核包失败: {}", e))?;
    if zip.len() > MAX_FILES {
        return Err(format!("内核包模块过多(> {})", MAX_FILES));
    }

    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    let mut total = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = normalize(entry.name());
        if name.is_empty() || !name.ends_with(".lua") {
            continue;
        }
        if name.contains("..") || name.contains('\\') {
            return Err(format!("非法模块路径: {}", name));
        }
        let mut buf = Vec::new();
        entry
            .by_ref()
            .take((MAX_FILE + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        if buf.len() > MAX_FILE {
            return Err(format!("模块 {} 过大", name));
        }
        total += buf.len();
        if total > MAX_TOTAL {
            return Err("内核包解压后过大".to_string());
        }
        let src = String::from_utf8(buf).map_err(|_| format!("模块 {} 不是 UTF-8", name))?;
        raw.insert(name, src);
    }
    if raw.is_empty() {
        return Err("内核包内没有 .lua 模块".to_string());
    }

    // Locate `main.lua` (at root, or under one top-level dir) and, if needed,
    // strip that directory so module paths are relative to the kernel root.
    let entry = raw
        .keys()
        .find(|k| k.as_str() == "main.lua")
        .cloned()
        .or_else(|| {
            raw.keys()
                .filter(|k| k.ends_with("/main.lua"))
                .min_by_key(|k| k.len())
                .cloned()
        })
        .ok_or_else(|| "内核包缺少 main.lua".to_string())?;
    let prefix = entry.strip_suffix("main.lua").unwrap_or("").to_string();

    let files: BTreeMap<String, String> = raw
        .into_iter()
        .filter_map(|(k, v)| {
            if prefix.is_empty() {
                Some((k, v))
            } else {
                k.strip_prefix(&prefix).map(|s| (s.to_string(), v))
            }
        })
        .collect();

    let set = ModuleSet {
        entry: "main.lua".to_string(),
        files,
    };
    set.validate().map_err(|e| format!("内核包不合法: {}", e))?;
    Ok(set)
}

fn normalize(name: &str) -> String {
    name.trim_start_matches("./").trim_start_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            for (name, src) in files {
                w.start_file(*name, opts).unwrap();
                w.write_all(src.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn reads_root_layout() {
        let z = zip_of(&[
            ("main.lua", "return {}"),
            ("lib/util.lua", "return {}"),
            ("readme.txt", "ignored"),
        ]);
        let set = from_zip(&z).unwrap();
        assert_eq!(set.entry, "main.lua");
        assert!(set.files.contains_key("lib/util.lua"));
        assert!(!set.files.contains_key("readme.txt"));
    }

    #[test]
    fn strips_single_top_dir() {
        let z = zip_of(&[
            ("kernel/main.lua", "return {}"),
            ("kernel/lib/a.lua", "return {}"),
        ]);
        let set = from_zip(&z).unwrap();
        assert!(set.files.contains_key("main.lua"));
        assert!(set.files.contains_key("lib/a.lua"));
    }

    #[test]
    fn rejects_missing_entry_and_traversal() {
        assert!(from_zip(&zip_of(&[("lib/a.lua", "return {}")])).is_err());
        assert!(from_zip(&zip_of(&[("../evil.lua", "return {}")])).is_err());
        assert!(from_zip(b"not a zip").is_err());
    }
}
