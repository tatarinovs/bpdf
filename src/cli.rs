use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Lightweight document and PDF toolkit",
    propagate_version = true
)]
pub struct Cli {
    /// Configuration file (otherwise searches config.jsonc/config.json in cwd and beside bpdf).
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Suppress progress and success messages; command results are still printed.
    #[arg(long, global = true, conflicts_with = "json")]
    pub quiet: bool,

    /// Emit progress, result and error events as newline-delimited JSON.
    #[arg(long, global = true)]
    pub json: bool,

    /// Stop a batch command after its first input error.
    #[arg(long, global = true)]
    pub fail_fast: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Merge PDF, image frames, Office/OpenDocument and text files.
    Merge(MergeArgs),
    /// Extract text from PDF/images, using Groq Vision when needed.
    Ocr(OcrArgs),
    /// Split a PDF into one file per page.
    Split {
        /// Source PDF file.
        input: PathBuf,
        /// Destination directory (defaults to the source file's directory).
        output_dir: Option<PathBuf>,
    },
    /// Extract selected pages into one PDF.
    Extract {
        /// Source PDF file.
        input: PathBuf,
        /// Page selection, for example 1-5,8,last, even or odd.
        pages: String,
        /// Destination PDF (must differ from the source file).
        output: Option<PathBuf>,
    },
    /// Inspect page, font, image and text information in a PDF.
    Inspect {
        /// PDF file to inspect.
        input: PathBuf,
        /// Also extract and print the existing text layer.
        #[arg(long)]
        text: bool,
    },
    /// Strip metadata without re-encoding JPEG/PNG pixel data.
    Strip(StripArgs),
    /// Rotate PDF pages or JPEG images by a multiple of 90 degrees or to a target orientation.
    Rotate {
        /// Source PDF files or JPEG images, non-recursive directories, globs, or @list.txt manifests.
        #[arg(required = true)]
        inputs: Vec<String>,
        /// Clockwise angle in degrees; must be a multiple of 90.
        #[arg(required_unless_present = "orient")]
        degrees: Option<i64>,
        /// Target orientation (landscape or portrait).
        #[arg(long, conflicts_with = "degrees")]
        orient: Option<String>,
        /// Pages to rotate, for example all, 1-5, even or odd (PDF only).
        #[arg(short, long, default_value = "all")]
        pages: String,
        /// Destination file or directory; may equal the input for an in-place update.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Resize PDF pages or JPEG images.
    Resize {
        /// Source PDF files or JPEG images, non-recursive directories, globs, or @list.txt manifests.
        #[arg(required = true)]
        inputs: Vec<String>,
        /// Target paper size for PDFs: A4 or Letter.
        #[arg(short, long, default_value = "A4")]
        size: String,
        /// Resize JPEG images so the longest edge is at most this many pixels.
        #[arg(long)]
        long_edge: Option<u32>,
        /// Resize JPEG images so the shortest edge is at least this many pixels.
        #[arg(long)]
        short_edge: Option<u32>,
        /// Pages to resize, for example all, 1-5, even or odd (PDF only).
        #[arg(short, long, default_value = "all")]
        pages: String,
        /// Destination file or directory; may equal the input for an in-place update.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Extract an existing PDF text layer without using a network service.
    Text {
        /// Source PDF file with an existing text layer.
        input: PathBuf,
        /// Destination text file (prints to stdout when omitted).
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Check configuration and external integrations without exposing secrets.
    Doctor,
    /// Apply a PNG stamp to an existing PDF.
    Stamp(StampArgs),
    /// Optimize PDF structure and downsample oversized images.
    Optimize {
        /// Source PDF file.
        input: PathBuf,
        /// Destination PDF; may equal the input for an in-place update.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Show or update PDF Info metadata.
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    /// Convert supported images or PDF embedded images to JPEG.
    Convert(ConvertArgs),
}

#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    /// Show PDF Info metadata.
    Show {
        /// Source PDF file.
        input: PathBuf,
    },
    /// Set selected PDF Info fields, leaving unspecified fields unchanged.
    Set {
        /// Source PDF file.
        input: PathBuf,
        /// Destination PDF; may equal the input for an in-place update.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Document title.
        #[arg(long)]
        title: Option<String>,
        /// Document author.
        #[arg(long)]
        author: Option<String>,
        /// Document subject.
        #[arg(long)]
        subject: Option<String>,
        /// Document keywords.
        #[arg(long)]
        keywords: Option<String>,
        /// Application that created the document.
        #[arg(long)]
        creator: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct StampArgs {
    /// Source PDF file.
    pub input: PathBuf,
    /// PNG image used as the stamp.
    pub stamp: PathBuf,
    /// Destination PDF; may equal the input for an in-place update.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Anchor (br, bl, tr, tl, c, tc, bc, l, r) or X,Y mm offset from bottom-right.
    #[arg(long, default_value = "br")]
    pub position: String,
    /// Stamp scale; 0 selects an automatic size up to 25% of the page.
    #[arg(long, default_value_t = 0.0)]
    pub scale: f64,
    /// Stamp opacity from 0 (transparent) to 1 (opaque).
    #[arg(long, default_value_t = 1.0)]
    pub opacity: f64,
    /// Pages to stamp, for example all, first, last, 1-5, even or odd.
    #[arg(long, default_value = "all")]
    pub pages: String,
    /// Layer mode: auto, over or under.
    #[arg(long, default_value = "auto")]
    pub mode: String,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,

    /// Destination PDF or text file (generated automatically when omitted).
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// Target PDF page size: A4, Letter, or none/original/keep for source PDF sizes.
    #[arg(short, long)]
    pub size: Option<String>,

    /// Rotate pages to the document's majority orientation; ties follow the first page.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub auto_rotate: Option<bool>,

    /// Preserve each page's orientation, overriding auto_rotate from configuration.
    #[arg(long, conflicts_with = "auto_rotate")]
    pub no_rotate: bool,

    /// PNG image to apply as a stamp.
    #[arg(long)]
    pub stamp: Option<PathBuf>,
    /// Stamp anchor or X,Y mm offset from the bottom-right.
    #[arg(long, default_value = "br")]
    pub stamp_pos: String,
    /// Stamp scale; 0 selects an automatic size up to 25% of the page.
    #[arg(long, default_value_t = 0.0)]
    pub stamp_scale: f64,
    /// Stamp opacity from 0 (transparent) to 1 (opaque).
    #[arg(long, default_value_t = 1.0)]
    pub stamp_op: f64,
    /// Pages to stamp, for example all, first, last, 1-5, even or odd.
    #[arg(long, default_value = "all")]
    pub stamp_pages: String,
    /// Stamp layer mode: auto, over or under.
    #[arg(long, default_value = "auto")]
    pub stamp_mode: String,

    /// Preserve embedded ICC colour profiles.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub keep_icc: Option<bool>,
    /// Optimize PDF structure and downsample images using configured image_dpi/jpeg_quality.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub optimize: Option<bool>,
    /// Remove metadata from the resulting document.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub strip_meta: Option<bool>,

    /// Add PDF outline bookmarks for each merged input file.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub bookmarks: Option<bool>,

    /// PDF Author metadata value.
    #[arg(long)]
    pub author: Option<String>,
    /// PDF Creator metadata value.
    #[arg(long)]
    pub creator: Option<String>,
    /// Path to FFmpeg for external image formats and decoder fallback.
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct OcrArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Combined Markdown output; when omitted, writes one .md file per input.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Update the source PDF in-place with the searchable text layer instead of creating a new file.
    #[arg(long, conflicts_with = "out")]
    pub in_place: bool,
    /// HTTP or SOCKS5 proxy URL.
    #[arg(long)]
    pub proxy: Option<String>,
    /// Groq Vision model name.
    #[arg(long)]
    pub model: Option<String>,
    /// OCR instruction sent to the model.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Groq-compatible chat completions endpoint.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Run Vision OCR even when a PDF already has a text layer.
    #[arg(long)]
    pub force_ocr: bool,
    /// Path to FFmpeg for external image formats and decoder fallback.
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
    /// Maximum simultaneous OCR requests (1-64).
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Disable the content-addressed OCR cache.
    #[arg(long)]
    pub no_cache: bool,
    /// Override the OCR cache directory.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
    /// OCR engine: groq, windows (or winocr), auto.
    #[arg(long)]
    pub engine: Option<String>,
    /// OCR language tag for Windows OCR (e.g. ru, en-US).
    #[arg(long)]
    pub lang: Option<String>,
}

#[derive(Debug, Args)]
pub struct StripArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Destination file (allowed only for a single input); defaults to in-place.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Preserve embedded ICC colour profiles.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub keep_icc: Option<bool>,
    /// Path to FFmpeg for external image formats and decoder fallback.
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// Input files, non-recursive directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Destination directory for generated JPEG files.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Preserve embedded ICC colour profiles.
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub keep_icc: Option<bool>,
    /// Path to FFmpeg for external image formats and decoder fallback.
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
    /// Allow output to overwrite an input file (e.g. jpg -> jpg in place).
    #[arg(long)]
    pub force: bool,
    /// Resize images so the longest edge is at most this many pixels.
    #[arg(long)]
    pub long_edge: Option<u32>,
    /// Resize images so the shortest edge is at least this many pixels.
    #[arg(long)]
    pub short_edge: Option<u32>,
    /// Force rotation to 'landscape' or 'portrait'.
    #[arg(long)]
    pub orient: Option<String>,
    /// JPEG quality (0-100), overrides config.
    #[arg(short = 'q', long)]
    pub quality: Option<u8>,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn merge_no_rotate_is_available_and_conflicts_with_auto_rotate() {
        let cli = Cli::try_parse_from(["bpdf", "merge", "scan.pdf", "--no-rotate"]).unwrap();
        let Command::Merge(args) = cli.command else {
            panic!("expected merge command");
        };
        assert!(args.no_rotate);

        assert!(
            Cli::try_parse_from(["bpdf", "merge", "scan.pdf", "--no-rotate", "--auto-rotate",])
                .is_err()
        );
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
