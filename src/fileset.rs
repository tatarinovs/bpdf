use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glob::glob;

use crate::formats::InputFormatSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSpec {
    pub path: PathBuf,
    pub pages: Option<String>,
}

impl InputSpec {
    /// Page ranges (`file.pdf:1-3`) only make sense for PDF inputs.
    pub fn reject_pages(&self) -> Result<()> {
        if self.pages.is_some() {
            bail!(
                "page ranges are only valid for PDF inputs: {}",
                self.path.display()
            );
        }
        Ok(())
    }
}

pub fn expand(arguments: &[String], formats: InputFormatSet) -> Result<Vec<InputSpec>> {
    let mut files = Vec::new();

    for argument in arguments {
        if let Some(manifest_arg) = argument.strip_prefix('@') {
            let (manifest_path, pages) = split_pdf_range(manifest_arg);
            let manifest_path = Path::new(&manifest_path);
            // Relative entries are relative to the list, not to the
            // directory bpdf happens to run in.
            let base = manifest_path.parent().unwrap_or(Path::new(""));
            for entry in read_manifest(manifest_path)? {
                let (entry_path, entry_pages) = split_pdf_range(&entry);
                files.push(InputSpec {
                    path: base.join(entry_path),
                    pages: entry_pages.or_else(|| pages.clone()),
                });
            }
            continue;
        }

        let (base, pages) = split_pdf_range(argument);
        let path = Path::new(&base);

        if path.is_dir() {
            let mut entries = fs::read_dir(path)
                .with_context(|| format!("failed to read directory {}", path.display()))?
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .file_type()
                        .map(|kind| kind.is_file())
                        .unwrap_or(false)
                })
                .map(|entry| entry.path())
                .filter(|entry| formats.includes(entry))
                .filter(|entry| !is_office_lock_file(entry))
                .collect::<Vec<_>>();
            natural_sort(&mut entries);
            files.extend(entries.into_iter().map(|path| InputSpec {
                path,
                pages: pages.clone(),
            }));
        } else if base.contains(['*', '?']) {
            // Like a directory listing: files of the formats the command reads.
            let mut matches = glob(&base)
                .with_context(|| format!("invalid glob pattern {base}"))?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.is_file() && formats.includes(entry))
                .filter(|entry| !is_office_lock_file(entry))
                .collect::<Vec<_>>();
            natural_sort(&mut matches);
            if matches.is_empty() {
                crate::output::warn(format!("no supported files match {base}, skipping"));
            }
            files.extend(matches.into_iter().map(|path| InputSpec {
                path,
                pages: pages.clone(),
            }));
        } else {
            files.push(InputSpec {
                path: path.to_path_buf(),
                pages,
            });
        }
    }

    if files.is_empty() {
        bail!("no input files found");
    }
    Ok(files)
}

fn split_pdf_range(argument: &str) -> (String, Option<String>) {
    let lowercase = argument.to_ascii_lowercase();
    if let Some(index) = lowercase.rfind(".pdf:") {
        return (
            argument[..index + 4].to_owned(),
            Some(argument[index + 5..].to_owned()),
        );
    }
    (argument.to_owned(), None)
}

fn is_office_lock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("~$"))
        .unwrap_or(false)
}

fn natural_sort(paths: &mut [PathBuf]) {
    paths.sort_by_cached_key(|path| NaturalKey(path.to_string_lossy().to_lowercase()));
}

/// Case-folded string ordered with [`natural_compare_folded`].
#[derive(Eq, PartialEq)]
struct NaturalKey(String);

impl Ord for NaturalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        natural_compare_folded(self.0.as_bytes(), other.0.as_bytes())
    }
}

impl PartialOrd for NaturalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Sort names case-insensitively with digit runs compared numerically.
pub fn natural_sort_names(names: &mut [String]) {
    names.sort_by_cached_key(|name| NaturalKey(name.to_lowercase()));
}

fn natural_compare_folded(left: &[u8], right: &[u8]) -> Ordering {
    let (mut l, mut r) = (0usize, 0usize);

    while l < left.len() && r < right.len() {
        if left[l].is_ascii_digit() && right[r].is_ascii_digit() {
            let l_start = l;
            let r_start = r;
            while l < left.len() && left[l].is_ascii_digit() {
                l += 1;
            }
            while r < right.len() && right[r].is_ascii_digit() {
                r += 1;
            }
            let l_significant = (l_start..l).find(|&index| left[index] != b'0').unwrap_or(l);
            let r_significant = (r_start..r)
                .find(|&index| right[index] != b'0')
                .unwrap_or(r);
            let by_digits = (l - l_significant)
                .cmp(&(r - r_significant))
                .then_with(|| left[l_significant..l].cmp(&right[r_significant..r]))
                .then_with(|| (l - l_start).cmp(&(r - r_start)));
            if by_digits != Ordering::Equal {
                return by_digits;
            }
            continue;
        }

        let by_byte = left[l].cmp(&right[r]);
        if by_byte != Ordering::Equal {
            return by_byte;
        }
        l += 1;
        r += 1;
    }
    left.len().cmp(&right.len())
}

fn read_manifest(path: &Path) -> Result<Vec<String>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest {}", path.display()))?;
    Ok(contents
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdf_page_range_without_breaking_drive_letter() {
        assert_eq!(
            split_pdf_range(r"C:\docs\a.pdf:1-5,8"),
            (r"C:\docs\a.pdf".to_owned(), Some("1-5,8".to_owned()))
        );
        assert_eq!(
            split_pdf_range(r"C:\docs\a.jpg"),
            (r"C:\docs\a.jpg".to_owned(), None)
        );
    }

    #[test]
    fn natural_sort_orders_scan_pages() {
        let mut paths = vec![
            PathBuf::from("scan_10.jpg"),
            PathBuf::from("scan_2.jpg"),
            PathBuf::from("scan_1.jpg"),
        ];
        natural_sort(&mut paths);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("scan_1.jpg"),
                PathBuf::from("scan_2.jpg"),
                PathBuf::from("scan_10.jpg")
            ]
        );
    }

    #[test]
    fn manifest_entries_are_relative_to_the_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let list = directory.path().join("list.txt");
        std::fs::write(&list, "# scans\nscan.pdf:2\nsub/photo.jpg\n").unwrap();
        let specs = expand(&[format!("@{}", list.display())], InputFormatSet::Merge).unwrap();
        assert_eq!(specs[0].path, directory.path().join("scan.pdf"));
        assert_eq!(specs[0].pages.as_deref(), Some("2"));
        assert_eq!(specs[1].path, directory.path().join("sub/photo.jpg"));
    }

    #[test]
    fn globs_take_supported_files_only() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["a.jpg", "b.txt", "c.pdf"] {
            std::fs::write(directory.path().join(name), "x").unwrap();
        }
        std::fs::create_dir(directory.path().join("d.jpg")).unwrap();
        let pattern = directory.path().join("*").display().to_string();
        let specs = expand(&[pattern], InputFormatSet::Rotate).unwrap();
        let names = specs
            .iter()
            .map(|spec| {
                spec.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["a.jpg", "c.pdf"]);
    }
}
