use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
#[cfg(windows)]
use {
    crate::encoding::powershell_encoded_command,
    crate::formats::{self, Format},
    crate::process,
    anyhow::Context,
    std::fs,
    std::process::Command,
    tempfile::Builder,
};

#[derive(Clone, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
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

#[cfg(not(windows))]
pub fn convert_to_pdf(_path: &Path, _options: &OfficeOptions) -> Result<Vec<u8>> {
    bail!("Office conversion is only available on Windows");
}

#[cfg(windows)]
pub fn convert_to_pdf(path: &Path, options: &OfficeOptions) -> Result<Vec<u8>> {
    // Office COM servers misbehave with concurrent automation sessions, so
    // conversions are serialised even when inputs load in parallel.
    static OFFICE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _session = crate::parallel::lock(&OFFICE);
    convert_to_pdf_windows(path, options)
}

#[cfg(not(windows))]
pub fn probe(_options: &OfficeOptions) -> Result<OfficeAvailability> {
    Ok(OfficeAvailability {
        word: false,
        excel: false,
        powerpoint: false,
    })
}

#[cfg(windows)]
pub fn probe(options: &OfficeOptions) -> Result<OfficeAvailability> {
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

#[cfg(windows)]
fn convert_to_pdf_windows(path: &Path, options: &OfficeOptions) -> Result<Vec<u8>> {
    let Some(script) = formats::detect(path).and_then(office_script) else {
        bail!("unsupported Office format: {}", path.display());
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
            &powershell_encoded_command(&script),
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

/// Shared PowerShell frame of every Office export: unblock the temporary
/// copy, start the application hidden, run `__BODY__` (which opens `$doc`
/// and saves it as PDF) and always close and release both COM objects.
#[cfg(windows)]
const OFFICE_FRAME: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
    Unblock-File -LiteralPath $env:BPDF_OFFICE_INPUT -ErrorAction SilentlyContinue
}
$app = $null
$doc = $null
try {
    $app = New-Object -ComObject __PROGID__
    $app.AutomationSecurity = 3
__BODY__
} finally {
    if ($null -ne $doc) {
        __CLOSE__
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($doc)
    }
    if ($null -ne $app) {
        $app.Quit()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($app)
    }
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}
"#;

/// The export script for one Office application.
#[cfg(windows)]
fn office_script(format: Format) -> Option<String> {
    let (prog_id, body, close) = match format {
        Format::Word => (
            "Word.Application",
            r#"    $app.Visible = $false
    $app.DisplayAlerts = 0
    # ConfirmConversions, ReadOnly, AddToRecentFiles: the temporary copy
    # stays out of Word's recent documents.
    $doc = $app.Documents.Open($env:BPDF_OFFICE_INPUT, $false, $true, $false)
    if ($null -eq $doc) {
        # Some Word/PowerShell combinations perform Open successfully but do
        # not marshal its return value. ActiveDocument is then authoritative.
        $doc = $app.ActiveDocument
    }
    if ($null -eq $doc) {
        $exists = Test-Path -LiteralPath $env:BPDF_OFFICE_INPUT
        $count = $app.Documents.Count
        throw "Word opened no active document (input exists: $exists, open documents: $count)"
    }
    $doc.SaveAs2($env:BPDF_OFFICE_OUTPUT, 17)"#,
            "$doc.Close($false)",
        ),
        Format::Excel => (
            "Excel.Application",
            r#"    $app.Visible = $false
    $app.DisplayAlerts = $false
    $doc = $app.Workbooks.Open($env:BPDF_OFFICE_INPUT, 0, $true)
    if ($null -eq $doc) {
        throw 'Excel returned no workbook object'
    }
    $doc.ExportAsFixedFormat(0, $env:BPDF_OFFICE_OUTPUT, 0, $true, $false)"#,
            "$doc.Close($false)",
        ),
        Format::PowerPoint => (
            "PowerPoint.Application",
            r#"    $app.DisplayAlerts = 1
    $doc = $app.Presentations.Open($env:BPDF_OFFICE_INPUT, $true, $false, $false)
    if ($null -eq $doc) {
        throw 'PowerPoint returned no presentation object'
    }
    # ppSaveAsPDF = 32. SaveAs avoids PowerShell's ambiguous late-bound
    # ExportAsFixedFormat overload with its many optional COM arguments.
    $doc.SaveAs($env:BPDF_OFFICE_OUTPUT, 32)"#,
            "$doc.Close()",
        ),
        _ => return None,
    };
    Some(
        OFFICE_FRAME
            .replace("__PROGID__", prog_id)
            .replace("__BODY__", body)
            .replace("__CLOSE__", close),
    )
}

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
