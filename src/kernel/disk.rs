//! On-disk kernel layout.
//!
//! A kernel is stored either as a single `.lua` file (*file mode*) or as a
//! directory of `.lua` modules (*dir mode*). A path ending in `.lua` selects
//! file mode; anything else is a directory. Dir mode keeps each module as its
//! own file so real business logic stays editable and reviewable.

use crate::kernel::types::{default_entry, ModuleSet};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// True when `path` denotes a single-file kernel.
pub fn is_file_mode(path: &Path) -> bool {
    path.extension().map(|e| e == "lua").unwrap_or(false)
}

/// Read a kernel from disk (single file or directory).
pub fn load(path: &Path) -> Result<ModuleSet> {
    if path.is_dir() {
        let mut files = BTreeMap::new();
        walk(path, path, &mut files).with_context(|| format!("读取内核目录 {}", path.display()))?;
        if files.is_empty() {
            bail!("内核目录 {} 下没有 .lua 文件", path.display());
        }
        let set = ModuleSet {
            entry: default_entry(),
            files,
        };
        set.validate().map_err(|e| anyhow::anyhow!(e))?;
        Ok(set)
    } else if path.is_file() {
        let code = std::fs::read_to_string(path)
            .with_context(|| format!("读取内核 {}", path.display()))?;
        Ok(ModuleSet::single(&code))
    } else {
        bail!("内核路径不存在: {}", path.display())
    }
}

/// Write a kernel to disk, replacing whatever was there.
pub fn save(path: &Path, set: &ModuleSet) -> Result<()> {
    set.validate().map_err(|e| anyhow::anyhow!(e))?;
    if is_file_mode(path) {
        let src = set
            .files
            .get(&set.entry)
            .ok_or_else(|| anyhow::anyhow!("入口模块 {} 不存在", set.entry))?;
        crate::atomic::write(path, src.as_bytes())
            .with_context(|| format!("写入内核 {}", path.display()))?;
        return Ok(());
    }
    // Dir mode: rebuild from scratch so removed modules never linger.
    if path.exists() {
        if path.is_dir() {
            std::fs::remove_dir_all(path)
                .with_context(|| format!("清理内核目录 {}", path.display()))?;
        } else {
            std::fs::remove_file(path)
                .with_context(|| format!("清理内核文件 {}", path.display()))?;
        }
    }
    std::fs::create_dir_all(path).with_context(|| format!("创建内核目录 {}", path.display()))?;
    for (rel, src) in &set.files {
        let full = path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::atomic::write(&full, src.as_bytes())
            .with_context(|| format!("写入模块 {}", full.display()))?;
    }
    Ok(())
}

fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, base, out)?;
        } else if path.extension().map(|e| e == "lua").unwrap_or(false) {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read_to_string(&path)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ts-kdir-{}", uuid::Uuid::new_v4()));
        let mut files = BTreeMap::new();
        files.insert("main.lua".to_string(), "return 1".to_string());
        files.insert("lib/a.lua".to_string(), "return 2".to_string());
        let set = ModuleSet {
            entry: "main.lua".to_string(),
            files,
        };
        save(&dir, &set).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back.files.len(), 2);
        assert_eq!(back.files.get("lib/a.lua").unwrap(), "return 2");
        // Overwrite with a smaller set: stale modules must be gone.
        save(&dir, &ModuleSet::single("return 3")).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back.files.len(), 1);
        assert!(!dir.join("lib/a.lua").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_mode_roundtrip() {
        let f = std::env::temp_dir().join(format!("ts-kfile-{}.lua", uuid::Uuid::new_v4()));
        save(&f, &ModuleSet::single("return { abi = 1 }")).unwrap();
        let back = load(&f).unwrap();
        assert_eq!(back.files.get("main.lua").unwrap(), "return { abi = 1 }");
        let _ = std::fs::remove_file(&f);
    }
}
