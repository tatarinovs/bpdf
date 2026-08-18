use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lopdf::Document;
use serde_json::json;

use crate::atomic::write_atomic;
use crate::fileset::InputSpec;
use crate::{output, pdf};

pub const DOCUMENT_SEPARATOR: &str = "\n\n---\n\n";

pub fn finish_batch(operation: &str, total: usize, failed: usize, outputs: usize) -> Result<()> {
    let succeeded = total - failed;
    output::result(
        "batch_summary",
        format!("{operation}: {succeeded} succeeded, {failed} failed, {outputs} output(s)"),
        json!({
            "operation": operation,
            "total": total,
            "succeeded": succeeded,
            "failed": failed,
            "outputs": outputs,
        }),
    );
    if failed != 0 {
        bail!("{failed} of {total} files failed");
    }
    Ok(())
}

pub fn handle_results<T, P>(
    results: impl IntoIterator<Item = (P, Result<T>)>,
    fail_fast: bool,
    failure: &str,
    warning: &str,
    mut success: impl FnMut(T),
) -> Result<usize>
where
    P: AsRef<Path>,
{
    let mut failures = 0;
    for (path, result) in results {
        match result {
            Ok(value) => success(value),
            Err(error) if fail_fast => {
                return Err(error)
                    .with_context(|| format!("{failure} {}", path.as_ref().display()));
            }
            Err(error) => {
                failures += 1;
                output::warn(format!("{warning} {}: {error:#}", path.as_ref().display()));
            }
        }
    }
    Ok(failures)
}

pub fn write_output(path: &Path, data: &[u8]) -> Result<()> {
    write_atomic(path, data).with_context(|| format!("failed to write {}", path.display()))?;
    output::written(path);
    Ok(())
}

pub fn edit_pdf<F>(input: &Path, output: Option<PathBuf>, suffix: &str, edit: F) -> Result<()>
where
    F: FnOnce(&mut Document) -> Result<()>,
{
    let output = output.unwrap_or_else(|| suffixed_output(input, suffix, "pdf"));
    write_output(&output, &pdf::transform_file(input, edit)?)
}

pub fn suffixed_output(input: &Path, suffix: &str, extension: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    input.with_file_name(format!("{stem}_{suffix}.{extension}"))
}

pub fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

pub fn path_key(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or_else(|| std::path::absolute(path).unwrap_or_else(|_| path.to_owned()))
    });
    let value = resolved.to_string_lossy().into_owned();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

#[derive(Default)]
pub struct OutputRegistry {
    destinations: HashSet<String>,
}

impl OutputRegistry {
    pub fn reserve(&mut self, input: &Path, output: &Path, force: bool) -> Result<()> {
        if !self.destinations.insert(path_key(output)) {
            bail!(
                "multiple inputs would write {}; choose unique names or output directories",
                output.display()
            );
        }
        if output.exists() && !force {
            if same_path(input, output) {
                bail!(
                    "{} would overwrite the input; pass --force to convert in place",
                    output.display()
                );
            }
            bail!(
                "output {} already exists; pass --force to replace it",
                output.display()
            );
        }
        Ok(())
    }
}

pub fn reject_output_collision(output: &Path, inputs: &[InputSpec]) -> Result<()> {
    if inputs.iter().any(|input| same_path(output, &input.path)) {
        bail!(
            "output {} is also an input; choose a different path",
            output.display()
        );
    }
    Ok(())
}

pub fn join_numbers(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub fn resolve_in_place_output(input: &Path, explicit_out: Option<&Path>, verb: &str) -> PathBuf {
    let output_path = explicit_out
        .map(|p| {
            if p.is_dir() {
                p.join(input.file_name().unwrap_or_default())
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| input.to_path_buf());

    if same_path(input, &output_path) {
        crate::output::info(format!("{} in-place: {}", verb, input.display()));
    }

    output_path
}

pub fn err_pdf_only_page_ranges(input: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "page ranges are only valid for PDF inputs: {}",
        input.display()
    )
}
