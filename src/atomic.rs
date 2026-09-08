use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use tempfile::Builder;

/// Write next to the destination, flush it to disk, then atomically replace the
/// destination. `tempfile::persist` uses the platform replacement primitive,
/// including replacement of an existing file on Windows.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;

    let prefix = format!(
        ".{}.tmp-",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("bpdf")
    );
    let mut temporary = Builder::new().prefix(&prefix).tempfile_in(directory)?;
    temporary.write_all(data)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}
