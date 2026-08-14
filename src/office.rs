use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tempfile::Builder;

use crate::encoding::powershell_encoded_command;
use crate::formats::{self, Format};
use crate::process;

#[derive(Clone, Debug)]
pub struct OfficeOptions {
    pub powershell: PathBuf,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct OfficeAvailability {
    pub word: bool,
    pub excel: bool,
    pub powerpoint: bool,
}

pub fn convert_to_pdf(path: &Path, options: &OfficeOptions) -> Result<Vec<u8>> {
    #[cfg(not(windows))]
    {
        let _ = options;
        bail!("Office conversion is only available on Windows");
    }

    #[cfg(windows)]
    {
        convert_to_pdf_windows(path, options)
    }
}

pub fn probe(options: &OfficeOptions) -> Result<OfficeAvailability> {
    #[cfg(not(windows))]
    {
        let _ = options;
        return Ok(OfficeAvailability {
            word: false,
            excel: false,
            powerpoint: false,
        });
    }

    #[cfg(windows)]
    {
        let mut command = Command::new(&options.powershell);
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-OutputFormat",
            "Text",
            "-EncodedCommand",
            &powershell_encoded_command(OFFICE_PROBE_SCRIPT),
        ]);
        let output = process::require_success(
            process::run(
                command,
                options.timeout.min(Duration::from_secs(20)),
                "Office probe",
            )?,
            "Office probe",
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(OfficeAvailability {
            word: text.lines().any(|line| line.trim() == "word=true"),
            excel: text.lines().any(|line| line.trim() == "excel=true"),
            powerpoint: text.lines().any(|line| line.trim() == "powerpoint=true"),
        })
    }
}

#[cfg(windows)]
fn convert_to_pdf_windows(path: &Path, options: &OfficeOptions) -> Result<Vec<u8>> {
    let script = match formats::detect(path) {
        Some(Format::Word) => WORD_SCRIPT,
        Some(Format::Excel) => EXCEL_SCRIPT,
        Some(Format::PowerPoint) => POWERPOINT_SCRIPT,
        _ => bail!("unsupported Office format: {}", path.display()),
    };
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("docx");

    // Work on a temporary copy: Office automation may need to remove the
    // downloaded-file marker, but bpdf must never change the security state of
    // the user's source document.
    let input_copy = Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile()?
        .into_temp_path();
    fs::copy(path, &input_copy)
        .with_context(|| format!("failed to copy {} for Office conversion", path.display()))?;
    let absolute_input = fs::canonicalize(&input_copy)
        .map(normalize_com_path)
        .with_context(|| format!("failed to resolve temporary copy of {}", path.display()))?;

    let temporary = Builder::new().suffix(".pdf").tempfile()?;
    let output_path = temporary.into_temp_path();
    // Word and Excel should create the output themselves. An existing empty
    // file can trigger an overwrite prompt despite DisplayAlerts=false.
    fs::remove_file(&output_path)?;

    let mut command = Command::new(&options.powershell);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-OutputFormat",
            "Text",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &powershell_encoded_command(script),
        ])
        .env("BPDF_OFFICE_INPUT", &absolute_input)
        .env("BPDF_OFFICE_OUTPUT", &output_path);

    let output =
        process::run(command, options.timeout, "Office conversion").with_context(|| {
            format!(
                "failed to convert {}; Microsoft Office must be installed and activated",
                path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stderr.contains("80080005") || stdout.contains("80080005") {
            bail!(
                "Microsoft Office COM server is stuck (error 80080005). Please close invisible WINWORD.EXE/EXCEL.EXE processes via Task Manager or dismiss open Office dialogs."
            );
        }
        if stderr.contains("8001010A") || stdout.contains("8001010A") {
            bail!(
                "Microsoft Office is busy (error 8001010A). Please close open Office dialogs and try again."
            );
        }
    }

    process::require_success(output, "Office conversion")?;

    let pdf = fs::read(&output_path)
        .with_context(|| format!("Office did not create {}", output_path.display()))?;
    if !pdf.starts_with(b"%PDF-") {
        bail!("Office output is not a PDF");
    }
    Ok(pdf)
}

#[cfg(windows)]
const WORD_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
    Unblock-File -LiteralPath $env:BPDF_OFFICE_INPUT -ErrorAction SilentlyContinue
}
$word = $null
$document = $null
try {
    $word = New-Object -ComObject Word.Application
    $word.Visible = $false
    $word.DisplayAlerts = 0
    $word.AutomationSecurity = 3
    $document = $word.Documents.Open($env:BPDF_OFFICE_INPUT, $false, $true)
    if ($null -eq $document) {
        # Some Word/PowerShell combinations perform Open successfully but do
        # not marshal its return value. ActiveDocument is then authoritative.
        $document = $word.ActiveDocument
    }
    if ($null -eq $document) {
        $exists = Test-Path -LiteralPath $env:BPDF_OFFICE_INPUT
        $count = $word.Documents.Count
        throw "Word opened no active document (input exists: $exists, open documents: $count)"
    }
    $document.SaveAs2($env:BPDF_OFFICE_OUTPUT, 17)
} finally {
    if ($null -ne $document) {
        $document.Close($false)
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($document)
    }
    if ($null -ne $word) {
        $word.Quit()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($word)
    }
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}
"#;

#[cfg(windows)]
const EXCEL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
    Unblock-File -LiteralPath $env:BPDF_OFFICE_INPUT -ErrorAction SilentlyContinue
}
$excel = $null
$workbook = $null
try {
    $excel = New-Object -ComObject Excel.Application
    $excel.Visible = $false
    $excel.DisplayAlerts = $false
    $excel.AutomationSecurity = 3
    $workbook = $excel.Workbooks.Open($env:BPDF_OFFICE_INPUT, 0, $true)
    if ($null -eq $workbook) {
        throw 'Excel returned no workbook object'
    }
    $workbook.ExportAsFixedFormat(0, $env:BPDF_OFFICE_OUTPUT, 0, $true, $false)
} finally {
    if ($null -ne $workbook) {
        $workbook.Close($false)
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($workbook)
    }
    if ($null -ne $excel) {
        $excel.Quit()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($excel)
    }
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}
"#;

#[cfg(windows)]
const POWERPOINT_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
    Unblock-File -LiteralPath $env:BPDF_OFFICE_INPUT -ErrorAction SilentlyContinue
}
$powerpoint = $null
$presentation = $null
try {
    $powerpoint = New-Object -ComObject PowerPoint.Application
    $powerpoint.DisplayAlerts = 1
    $powerpoint.AutomationSecurity = 3
    $presentation = $powerpoint.Presentations.Open(
        $env:BPDF_OFFICE_INPUT,
        $true,
        $false,
        $false
    )
    if ($null -eq $presentation) {
        throw 'PowerPoint returned no presentation object'
    }
    # ppSaveAsPDF = 32. SaveAs avoids PowerShell's ambiguous late-bound
    # ExportAsFixedFormat overload with its many optional COM arguments.
    $presentation.SaveAs($env:BPDF_OFFICE_OUTPUT, 32)
} finally {
    if ($null -ne $presentation) {
        $presentation.Close()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($presentation)
    }
    if ($null -ne $powerpoint) {
        $powerpoint.Quit()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($powerpoint)
    }
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}
"#;

#[cfg(windows)]
const OFFICE_PROBE_SCRIPT: &str = r#"
$word = $null -ne [type]::GetTypeFromProgID('Word.Application')
$excel = $null -ne [type]::GetTypeFromProgID('Excel.Application')
$powerpoint = $null -ne [type]::GetTypeFromProgID('PowerPoint.Application')
Write-Output "word=$($word.ToString().ToLowerInvariant())"
Write-Output "excel=$($excel.ToString().ToLowerInvariant())"
Write-Output "powerpoint=$($powerpoint.ToString().ToLowerInvariant())"
"#;

#[cfg(windows)]
fn normalize_com_path(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

#[cfg(all(test, windows))]
mod tests {
    use super::normalize_com_path;
    use std::path::PathBuf;

    #[test]
    fn removes_extended_path_prefix_for_office_com() {
        assert_eq!(
            normalize_com_path(PathBuf::from(r"\\?\D:\docs\file.docx")),
            PathBuf::from(r"D:\docs\file.docx")
        );
        assert_eq!(
            normalize_com_path(PathBuf::from(r"\\?\UNC\server\share\file.xlsx")),
            PathBuf::from(r"\\server\share\file.xlsx")
        );
    }
}
