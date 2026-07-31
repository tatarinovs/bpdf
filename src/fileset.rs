use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glob::glob;

pub const MERGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "gif", "tiff", "tif", "webp", "heic", "heif", "pdf", "doc",
    "docx", "xls", "xlsx", "md", "txt",
];
pub const STRIP_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "pdf"];

#[derive(Clone, Copy)]
pub struct ExpandOptions {
    pub directory_extensions: &'static [&'static str],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSpec {
    pub path: PathBuf,
    pub pages: Option<String>,
}

pub fn expand(arguments: &[String], options: ExpandOptions) -> Result<Vec<InputSpec>> {
    let mut files = Vec::new();

    for argument in arguments {
        let (base, pages) = split_pdf_range(argument);
        let path = Path::new(&base);

        if is_manifest(path) {
            for entry in read_manifest(path)? {
                let (entry_path, entry_pages) = split_pdf_range(&entry);
                files.push(InputSpec {
                    path: PathBuf::from(entry_path),
                    pages: entry_pages.or_else(|| pages.clone()),
                });
            }
        } else if path.is_dir() {
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
                .filter(|entry| extension_allowed(entry, options.directory_extensions))
                .collect::<Vec<_>>();
            natural_sort(&mut entries);
            files.extend(entries.into_iter().map(|path| InputSpec {
                path,
                pages: pages.clone(),
            }));
        } else if base.contains(['*', '?']) {
            let mut matches = glob(&base)
                .with_context(|| format!("invalid glob pattern {base}"))?
                .filter_map(|entry| entry.ok())
                .collect::<Vec<_>>();
            natural_sort(&mut matches);
            if matches.is_empty() {
                crate::output::warn(format!("no files match {base}, skipping"));
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

fn extension_allowed(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn natural_sort(paths: &mut [PathBuf]) {
    paths.sort_by(|left, right| natural_compare(&left.to_string_lossy(), &right.to_string_lossy()));
}

fn natural_compare(left: &str, right: &str) -> Ordering {
    let left_folded = left.to_lowercase();
    let right_folded = right.to_lowercase();
    let left = left_folded.as_bytes();
    let right = right_folded.as_bytes();
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

fn is_manifest(path: &Path) -> bool {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "lst" | "tmp" => true,
        "txt" => first_manifest_entry_exists(path),
        _ => false,
    }
}

fn first_manifest_entry_exists(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return true;
    };
    contents
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| {
            let (entry, _) = split_pdf_range(line);
            Path::new(&entry).exists()
                || (entry.contains(['*', '?'])
                    && glob(&entry)
                        .ok()
                        .is_some_and(|mut matches| matches.next().is_some()))
        })
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
}
