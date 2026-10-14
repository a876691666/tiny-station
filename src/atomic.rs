use std::io;
use std::path::Path;

/// Write `bytes` to `path` atomically: write a sibling temp file, then rename
/// it over the destination. A crash can never leave a half-written key/config
/// file behind.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    let tmp = path.with_file_name(format!(".{}.tmp", file_name));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
