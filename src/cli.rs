use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::imageconv::Orientation;
use crate::ocr::OcrBackend;
use crate::pdf::PageSize;
use crate::pdf::transform::BlendMode;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Lightweight document and PDF toolkit",
    propagate_version = true
)]
pub struct Cli {
    /// Configuration file (otherwise config.toml beside bpdf is used).
    #[arg(long, global = true, help_heading = "Global options")]
    pub config: Option<PathBuf>,

    /// Suppress progress and success messages; command results are still printed.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        conflicts_with = "json"
    )]
    pub quiet: bool,

    /// Emit progress, result and error events as newline-delimited JSON.
    #[arg(long, global = true, help_heading = "Global options")]
    pub json: bool,

    /// Stop a batch command after its first input error.
    #[arg(long, global = true, help_heading = "Global options")]
    pub fail_fast: bool,

    /// Never pause at exit.
    #[arg(long, global = true, help_heading = "Global options")]
    pub no_pause: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Merge PDF, images, Office/OpenDocument, HTML, e-books and text files into one PDF.
    Merge(MergeArgs),
    /// Extract text from PDF/images (Groq Vision or Windows OCR) or add a searchable layer.
    Ocr(OcrArgs),
    /// Split a PDF into one file per page.
    Split {
        /// Source PDF file.
        input: PathBuf,
        /// Destination directory (defaults to the source file's directory).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Replace page files that already exist.
        #[arg(long)]
        force: bool,
    },
    /// Extract selected pages into one PDF (also reorders, e.g. 3,1,2 or reverse).
    Extract {
        /// Source PDF file.
        input: PathBuf,
        /// Page selection, for example 1-5,8,last, even, odd or reverse.
        pages: String,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Show page sizes, fonts, images and metadata of a PDF.
    Info {
        /// PDF file to describe.
        input: PathBuf,
    },
    /// Extract an existing PDF text layer without using a network service.
    Text {
        /// Source PDF file with an existing text layer.
        input: PathBuf,
        /// Destination text file (prints to stdout when omitted).
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Strip metadata from PDF, JPEG and PNG without re-encoding pixels.
    Strip(StripArgs),
    /// Rotate PDF pages or JPEG images.
    Rotate(RotateArgs),
    /// Fit PDF pages to a paper size or resize JPEG images.
    Resize(ResizeArgs),
    /// Optimize PDF structure and downsample oversized images.
    Optimize(OptimizeArgs),
    /// Apply an image stamp to a PDF.
    Stamp(StampArgs),
    /// Add page numbers to a PDF.
    Number(NumberArgs),
    /// Add a text watermark to a PDF.
    Watermark(WatermarkArgs),
    /// Set PDF document properties, leaving unspecified fields unchanged.
    Metadata(MetadataArgs),
    /// Encrypt a PDF with AES-256: an open password and/or restrictions.
    Protect(ProtectArgs),
    /// Remove the password and restrictions of a PDF.
    Unlock(UnlockArgs),
    /// Convert images, PDF pages or images, and HTML pages to JPEG.
    Convert(ConvertArgs),
    /// Check configuration and external integrations without exposing secrets.
    Doctor,
}

/// Where an edited file is written.
#[derive(Debug, Args)]
pub struct OutputArgs {
    /// Destination file, or an existing directory for several inputs (defaults to NAME_<action>).
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Overwrite the source file.
    #[arg(short, long, conflicts_with = "out")]
    pub in_place: bool,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,

    /// Destination PDF or text file (generated automatically when omitted).
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// Page size: A3-A6, B4, B5, Letter, Legal, Tabloid, WxH[mm|cm|in|pt], keep (PDF sizes) or fit (image sizes) (default: page_size).
    #[arg(short, long, value_parser = parse_page_size)]
    pub size: Option<PageSize>,

    /// Rotate pages to the document's majority orientation (default: auto_rotate).
    #[arg(long, overrides_with = "no_auto_rotate")]
    pub auto_rotate: bool,
    /// Keep every page's own orientation.
    #[arg(long, hide = true)]
    pub no_auto_rotate: bool,

    /// Downsample images to image_dpi and tidy the PDF structure (default: optimize).
    #[arg(long, overrides_with = "no_optimize")]
    pub optimize: bool,
    /// Do not optimize the result.
    #[arg(long, hide = true)]
    pub no_optimize: bool,

    /// Remove document metadata from the result (default: strip_metadata).
    #[arg(long, overrides_with = "no_strip_meta")]
    pub strip_meta: bool,
    /// Keep document metadata.
    #[arg(long, hide = true)]
    pub no_strip_meta: bool,

    /// Add an outline bookmark for every merged file (default: bookmarks).
    #[arg(long, overrides_with = "no_bookmarks")]
    pub bookmarks: bool,
    /// Do not add bookmarks.
    #[arg(long, hide = true)]
    pub no_bookmarks: bool,

    /// Largest acceptable PDF size, e.g. 10MB or 500KB; image DPI and JPEG quality are lowered until the PDF fits.
    #[arg(long, value_parser = parse_size)]
    pub max_size: Option<u64>,

    /// Drop blank scanned pages (no text, only paper-coloured images).
    #[arg(long)]
    pub remove_blank: bool,
}

impl MergeArgs {
    /// Resolve a `--x` / `--no-x` pair against the configured default.
    pub fn switch(on: bool, off: bool, default: bool) -> bool {
        (default || on) && !off
    }
}

#[derive(Debug, Args)]
pub struct OcrArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required_unless_present = "clear_cache")]
    pub inputs: Vec<String>,
    /// Combined output: .md for text, .pdf for a searchable PDF; otherwise one .md per input.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Add a searchable text layer to the source PDFs; images become searchable NAME.pdf beside them.
    #[arg(short, long, conflicts_with = "out")]
    pub in_place: bool,
    /// OCR engine (default: ocr_engine).
    #[arg(short, long, value_enum, ignore_case = true)]
    pub engine: Option<OcrBackend>,
    /// Windows OCR language tag, e.g. ru or en-US (default: ocr_lang).
    #[arg(long)]
    pub lang: Option<String>,
    /// Recognise pages that already have a text layer.
    #[arg(long)]
    pub force_ocr: bool,
    /// Do not read or write the OCR cache.
    #[arg(long)]
    pub no_cache: bool,
    /// Delete all cached OCR responses before running.
    #[arg(long)]
    pub clear_cache: bool,
}

#[derive(Debug, Args)]
pub struct StripArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct RotateArgs {
    /// PDF files or JPEG images, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Clockwise angle (90, 180, 270, -90) or a target orientation (portrait, landscape).
    #[arg(short, long, allow_negative_numbers = true)]
    pub angle: String,
    /// Pages to rotate, for example all, 1-5, even or odd (PDF only).
    #[arg(short, long, default_value = "all")]
    pub pages: String,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct ResizeArgs {
    /// PDF files or JPEG images, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Paper size for PDF pages: A3-A6, B4, B5, Letter, Legal, Tabloid or WxH[mm|cm|in|pt].
    #[arg(short, long, default_value = "A4", value_parser = parse_paper_size)]
    pub size: PageSize,
    /// JPEG: shrink so the longest edge is at most this many pixels.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "short_edge")]
    pub long_edge: Option<u32>,
    /// JPEG: scale so the shortest edge is exactly this many pixels.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub short_edge: Option<u32>,
    /// Pages to resize, for example all, 1-5, even or odd (PDF only).
    #[arg(short, long, default_value = "all")]
    pub pages: String,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct OptimizeArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Largest acceptable file size, e.g. 10MB or 500KB; image DPI and JPEG quality are lowered until the PDF fits.
    #[arg(long, value_parser = parse_size)]
    pub max_size: Option<u64>,
    /// Convert colour images to grayscale (text and drawings keep their colours).
    #[arg(long)]
    pub gray: bool,
    /// Drop blank scanned pages (no text, only paper-coloured images).
    #[arg(long)]
    pub remove_blank: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct StampArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Stamp image (PNG with transparency, or JPEG).
    pub stamp: PathBuf,
    /// Anchor (br, bl, tr, tl, c, tc, bc, l, r) or X,Y mm offset from bottom-right.
    #[arg(long, default_value = "br", value_parser = parse_position)]
    pub position: String,
    /// Scale multiplier; omit for the physical size, 0 fits up to a quarter of the page.
    #[arg(long, value_parser = parse_non_negative)]
    pub scale: Option<f64>,
    /// Opacity from 0 (transparent) to 1 (opaque).
    #[arg(long, default_value_t = 1.0, value_parser = parse_opacity)]
    pub opacity: f64,
    /// Stamp image resolution in DPI (default: from PNG/JPEG metadata, else 96).
    #[arg(long, value_parser = parse_positive)]
    pub dpi: Option<f64>,
    /// Pages to stamp, for example all, first, last, 1-5, even or odd.
    #[arg(short, long, default_value = "all")]
    pub pages: String,
    /// Blend mode of the stamp with the page.
    #[arg(long, value_enum, default_value = "normal", ignore_case = true)]
    pub blend: BlendMode,
    /// Place the stamp in a browser window; --position, --scale and --pages set the starting point.
    #[arg(long)]
    pub pick: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct NumberArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Label template: {n} is the page number, {total} the last number.
    #[arg(long, default_value = "{n}")]
    pub format: String,
    /// Anchor (bc, br, bl, tc, tr, tl, c, l, r) or X,Y mm offset from bottom-right.
    #[arg(long, default_value = "bc", value_parser = parse_position)]
    pub position: String,
    /// Font size in points.
    #[arg(long, default_value_t = 10.0, value_parser = parse_positive)]
    pub size: f64,
    /// Number given to the first numbered page.
    #[arg(long, default_value_t = 1, allow_negative_numbers = true)]
    pub start: i64,
    /// Pages to number, for example all, 2-last, even or odd.
    #[arg(short, long, default_value = "all")]
    pub pages: String,
    /// Text colour: black, gray, red, blue, green, white or #RRGGBB.
    #[arg(long, default_value = "black", value_parser = parse_color)]
    pub color: [f64; 3],
    /// Text opacity from 0 (transparent) to 1 (opaque).
    #[arg(long, default_value_t = 1.0, value_parser = parse_opacity)]
    pub opacity: f64,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct WatermarkArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Watermark text.
    pub text: String,
    /// Font size in points; 0 fits the text to the page.
    #[arg(long, default_value_t = 0.0, value_parser = parse_non_negative)]
    pub size: f64,
    /// Counter-clockwise angle in degrees.
    #[arg(long, default_value_t = 45.0, allow_negative_numbers = true, value_parser = parse_number)]
    pub angle: f64,
    /// Anchor (c, tc, bc, ...) or X,Y mm offset from bottom-right.
    #[arg(long, default_value = "c", value_parser = parse_position)]
    pub position: String,
    /// Text colour: black, gray, red, blue, green, white or #RRGGBB.
    #[arg(long, default_value = "gray", value_parser = parse_color)]
    pub color: [f64; 3],
    /// Text opacity from 0 (transparent) to 1 (opaque).
    #[arg(long, default_value_t = 0.3, value_parser = parse_opacity)]
    pub opacity: f64,
    /// Pages to mark, for example all, first, 1-5, even or odd.
    #[arg(short, long, default_value = "all")]
    pub pages: String,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct MetadataArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Document title; an empty value removes the field.
    #[arg(long)]
    pub title: Option<String>,
    /// Document author.
    #[arg(long)]
    pub author: Option<String>,
    /// Document subject.
    #[arg(long)]
    pub subject: Option<String>,
    /// Document keywords.
    #[arg(long)]
    pub keywords: Option<String>,
    /// Application that created the document.
    #[arg(long)]
    pub creator: Option<String>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct ProtectArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Password needed to open the PDF (omit to let anyone open it).
    #[arg(long, env = "BPDF_PROTECT_PASSWORD", hide_env_values = true)]
    pub password: Option<String>,
    /// Password that lifts the restrictions (default: the open password, or a random one with restrictions).
    #[arg(long, env = "BPDF_PROTECT_OWNER_PASSWORD", hide_env_values = true)]
    pub owner_password: Option<String>,
    /// Forbid printing.
    #[arg(long)]
    pub no_print: bool,
    /// Forbid copying text and images.
    #[arg(long)]
    pub no_copy: bool,
    /// Forbid editing, annotating, filling forms and assembling pages.
    #[arg(long)]
    pub no_modify: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct UnlockArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// Open or owner password (omit for PDFs that open without one but carry restrictions).
    #[arg(long, env = "BPDF_UNLOCK_PASSWORD", hide_env_values = true)]
    pub password: Option<String>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Destination directory for generated JPEG files.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Replace existing files, including an input (e.g. jpg -> jpg in place).
    #[arg(long)]
    pub force: bool,
    /// Shrink so the longest edge is at most this many pixels.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "short_edge")]
    pub long_edge: Option<u32>,
    /// Scale so the shortest edge is exactly this many pixels.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub short_edge: Option<u32>,
    /// Rotate images to landscape or portrait.
    #[arg(long, value_enum, ignore_case = true)]
    pub orient: Option<Orientation>,
    /// JPEG quality 1-100 (default: jpeg_quality).
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub quality: Option<u8>,
    /// Largest size of each JPEG, e.g. 500KB; quality, then dimensions, are lowered until it fits.
    #[arg(long, value_parser = parse_size)]
    pub max_size: Option<u64>,
    /// Render whole PDF pages instead of extracting their images.
    #[arg(long)]
    pub render: bool,
    /// Browser window size for HTML pages, e.g. 1200x1600 (default: html_viewport).
    #[arg(long)]
    pub viewport: Option<String>,
}

/// Parse a page size for merge and OCR (paper, `keep` or `fit`).
pub fn parse_page_size(value: &str) -> Result<PageSize, String> {
    value.parse()
}

/// Parse a sheet of paper; `keep` and `fit` are not sizes to fit pages to.
pub fn parse_paper_size(value: &str) -> Result<PageSize, String> {
    match value.parse::<PageSize>()? {
        size if size.is_paper() => Ok(size),
        _ => Err(format!("'{value}' is not a paper size")),
    }
}

/// Check a stamp, number or watermark position early.
pub fn parse_position(value: &str) -> Result<String, String> {
    crate::pdf::transform::parse_position(value)
        .map(|_| value.to_owned())
        .map_err(|error| error.to_string())
}

/// A finite number (`nan` and `inf` would end up in the page content).
pub fn parse_number(value: &str) -> Result<f64, String> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("'{value}' is not a number"))
}

/// A number of at least 0.
pub fn parse_non_negative(value: &str) -> Result<f64, String> {
    match parse_number(value)? {
        number if number >= 0.0 => Ok(number),
        _ => Err(format!("'{value}' must not be negative")),
    }
}

/// A number above 0.
pub fn parse_positive(value: &str) -> Result<f64, String> {
    match parse_number(value)? {
        number if number > 0.0 => Ok(number),
        _ => Err(format!("'{value}' must be above 0")),
    }
}

/// An opacity from 0 to 1.
pub fn parse_opacity(value: &str) -> Result<f64, String> {
    match parse_number(value)? {
        number if (0.0..=1.0).contains(&number) => Ok(number),
        _ => Err(format!("opacity '{value}' must be between 0 and 1")),
    }
}

/// Parse a colour name or `#RRGGBB` into RGB components in 0..=1.
pub fn parse_color(value: &str) -> Result<[f64; 3], String> {
    let rgb = match value.trim().to_ascii_lowercase().as_str() {
        "black" => [0, 0, 0],
        "gray" | "grey" => [128, 128, 128],
        "white" => [255, 255, 255],
        "red" => [200, 0, 0],
        "green" => [0, 140, 0],
        "blue" => [0, 60, 200],
        other => {
            let hex = other.strip_prefix('#').unwrap_or(other);
            let channel = |index: usize| {
                hex.get(index..index + 2)
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            };
            match (hex.len(), channel(0), channel(2), channel(4)) {
                (6, Some(red), Some(green), Some(blue)) => [red, green, blue],
                _ => return Err(format!("unknown colour '{value}'; use a name or #RRGGBB")),
            }
        }
    };
    Ok(rgb.map(|channel| f64::from(channel) / 255.0))
}

/// Parse a size such as `10MB`, `1.5M`, `500 KB`, `750кб` or plain bytes
/// (binary multiples).
pub fn parse_size(value: &str) -> Result<u64, String> {
    let value = value.trim();
    let split = value
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number = number
        .parse::<f64>()
        .map_err(|_| format!("invalid size '{value}'"))?;
    let multiplier = match unit.trim().to_lowercase().as_str() {
        "" | "b" => 1u64,
        "k" | "kb" | "kib" | "к" | "кб" => 1 << 10,
        "m" | "mb" | "mib" | "м" | "мб" => 1 << 20,
        "g" | "gb" | "gib" | "г" | "гб" => 1 << 30,
        other => return Err(format!("unknown size unit '{other}'; use KB, MB or GB")),
    };
    let bytes = number * multiplier as f64;
    if !bytes.is_finite() || bytes < 1.0 {
        return Err(format!("size '{value}' must be positive"));
    }
    Ok(bytes as u64)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    fn parse(arguments: &[&str]) -> Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once("bpdf").chain(arguments.iter().copied()))
            .map(|cli| cli.command)
    }

    #[test]
    fn command_definitions_are_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn rotate_accepts_angles_and_orientations_after_inputs() {
        let Ok(Command::Rotate(args)) = parse(&["rotate", "a.pdf", "b.pdf", "-a", "-90"]) else {
            panic!("expected rotate");
        };
        assert_eq!(args.inputs, ["a.pdf", "b.pdf"]);
        assert_eq!(args.angle, "-90");
        let Ok(Command::Rotate(args)) = parse(&["rotate", "a.pdf", "--angle", "portrait", "-i"])
        else {
            panic!("expected rotate");
        };
        assert_eq!(args.angle, "portrait");
        assert!(args.output.in_place);
    }

    #[test]
    fn merge_switches_do_not_swallow_inputs() {
        let Ok(Command::Merge(args)) = parse(&["merge", "--optimize", "a.pdf", "--no-bookmarks"])
        else {
            panic!("expected merge");
        };
        assert_eq!(args.inputs, ["a.pdf"]);
        assert!(MergeArgs::switch(args.optimize, args.no_optimize, false));
        assert!(!MergeArgs::switch(args.bookmarks, args.no_bookmarks, true));
        assert!(MergeArgs::switch(
            args.auto_rotate,
            args.no_auto_rotate,
            true
        ));
    }

    #[test]
    fn last_merge_switch_wins() {
        let Ok(Command::Merge(args)) = parse(&["merge", "a.pdf", "--optimize", "--no-optimize"])
        else {
            panic!("expected merge");
        };
        assert!(!MergeArgs::switch(args.optimize, args.no_optimize, true));
    }

    #[test]
    fn out_and_in_place_conflict() {
        assert!(parse(&["strip", "a.jpg", "-o", "b.jpg", "-i"]).is_err());
    }

    #[test]
    fn values_are_checked_before_any_work() {
        let Ok(Command::Merge(args)) = parse(&["merge", "a.pdf", "-s", "210x297mm"]) else {
            panic!("expected merge");
        };
        assert_eq!(args.size, Some(PageSize::A4));
        assert!(parse(&["merge", "a.pdf", "-s", "A9"]).is_err());
        assert!(parse(&["resize", "a.pdf", "-s", "keep"]).is_err());
        assert!(parse(&["stamp", "a.pdf", "s.png", "--position", "middle"]).is_err());
        assert!(parse(&["stamp", "a.pdf", "s.png", "--position", "5,10"]).is_ok());
        assert!(parse(&["stamp", "a.pdf", "s.png", "--blend", "ColorDodge"]).is_ok());
        assert!(parse(&["ocr", "a.pdf", "-e", "winocr"]).is_ok());
        assert!(parse(&["ocr", "a.pdf", "-e", "tesseract"]).is_err());
        assert!(parse(&["convert", "a.jpg", "--long-edge", "9", "--short-edge", "5"]).is_err());
        assert!(parse(&["convert", "a.jpg", "--orient", "diagonal"]).is_err());
        assert!(parse(&["watermark", "a.pdf", "x", "--angle", "nan"]).is_err());
        assert!(parse(&["watermark", "a.pdf", "x", "--angle", "-30"]).is_ok());
        assert!(parse(&["stamp", "a.pdf", "s.png", "--dpi", "inf"]).is_err());
        assert!(parse(&["stamp", "a.pdf", "s.png", "--scale", "-1"]).is_err());
        assert!(parse(&["number", "a.pdf", "--opacity", "1.5"]).is_err());
    }

    #[test]
    fn parses_colours() {
        assert_eq!(parse_color("black"), Ok([0.0, 0.0, 0.0]));
        assert_eq!(parse_color("#FF0000"), Ok([1.0, 0.0, 0.0]));
        assert_eq!(parse_color("00ff00"), Ok([0.0, 1.0, 0.0]));
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("purple").is_err());
    }

    #[test]
    fn parses_human_sizes() {
        assert_eq!(parse_size("10MB"), Ok(10 << 20));
        assert_eq!(parse_size("1.5 m"), Ok(3 << 19));
        assert_eq!(parse_size("500кб"), Ok(500 << 10));
        assert_eq!(parse_size("2048"), Ok(2048));
        assert!(parse_size("ten MB").is_err());
        assert!(parse_size("5 TB").is_err());
        assert!(parse_size("0").is_err());
    }

    #[test]
    fn every_command_and_argument_has_help_text() {
        fn check(command: &clap::Command) {
            assert!(
                command.get_about().is_some(),
                "command '{}' has no description",
                command.get_name()
            );

            for argument in command.get_arguments() {
                assert!(
                    argument.get_help().is_some(),
                    "argument '{}' in command '{}' has no description",
                    argument.get_id(),
                    command.get_name()
                );
            }

            for subcommand in command.get_subcommands() {
                check(subcommand);
            }
        }

        check(&Cli::command());
    }
}
