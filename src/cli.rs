use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Lightweight document and PDF toolkit",
    propagate_version = true
)]
pub struct Cli {
    /// Configuration file (defaults to config.jsonc in cwd or beside bpdf).
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Suppress progress and success messages.
    #[arg(long, global = true, conflicts_with = "json")]
    pub quiet: bool,

    /// Emit newline-delimited JSON events.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Merge PDF, images, Office and text files.
    Merge(MergeArgs),
    /// Extract text from PDF/images, using Groq Vision when needed.
    Ocr(OcrArgs),
    /// Split a PDF into one file per page.
    Split {
        input: PathBuf,
        output_dir: Option<PathBuf>,
    },
    /// Extract selected pages into one PDF.
    Extract {
        input: PathBuf,
        pages: String,
        output: Option<PathBuf>,
    },
    /// Inspect page, font, image and text information in a PDF.
    Inspect {
        input: PathBuf,
        #[arg(long)]
        text: bool,
    },
    /// Strip metadata without re-encoding JPEG/PNG pixel data.
    Strip(StripArgs),
    /// Rotate selected PDF pages by a multiple of 90 degrees.
    Rotate {
        input: PathBuf,
        degrees: i64,
        #[arg(short, long, default_value = "all")]
        pages: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Resize and center selected PDF pages on A4 or Letter.
    Resize {
        input: PathBuf,
        #[arg(short, long, default_value = "A4")]
        size: String,
        #[arg(short, long, default_value = "all")]
        pages: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Extract an existing PDF text layer without using a network service.
    Text {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Check configuration and external integrations without exposing secrets.
    Doctor,
    /// Apply a PNG stamp to an existing PDF.
    Stamp(StampArgs),
    /// Structurally optimize an existing PDF.
    Optimize {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Show or update PDF Info metadata.
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    /// Extract embedded images from a PDF.
    Images {
        input: PathBuf,
        output_dir: Option<PathBuf>,
        #[arg(long)]
        ffmpeg: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    Show {
        input: PathBuf,
    },
    Set {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        keywords: Option<String>,
        #[arg(long)]
        creator: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct StampArgs {
    pub input: PathBuf,
    pub stamp: PathBuf,
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    #[arg(long, default_value = "br")]
    pub position: String,
    #[arg(long, default_value_t = 0.0)]
    pub scale: f64,
    #[arg(long, default_value_t = 1.0)]
    pub opacity: f64,
    #[arg(long, default_value = "all")]
    pub pages: String,
    #[arg(long, default_value = "auto")]
    pub mode: String,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// Input files, directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,

    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// Target page size, or none/original/keep to preserve source sizes.
    #[arg(short, long)]
    pub size: Option<String>,

    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub auto_rotate: Option<bool>,

    #[arg(long)]
    pub stamp: Option<PathBuf>,
    #[arg(long, default_value = "br")]
    pub stamp_pos: String,
    #[arg(long, default_value_t = 0.0)]
    pub stamp_scale: f64,
    #[arg(long, default_value_t = 1.0)]
    pub stamp_op: f64,
    #[arg(long, default_value = "all")]
    pub stamp_pages: String,
    #[arg(long, default_value = "auto")]
    pub stamp_mode: String,

    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub keep_icc: Option<bool>,
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub optimize: Option<bool>,
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub strip_meta: Option<bool>,

    #[arg(long)]
    pub author: Option<String>,
    #[arg(long)]
    pub creator: Option<String>,
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct OcrArgs {
    /// Input files, directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    #[arg(long)]
    pub proxy: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub endpoint: Option<String>,
    #[arg(long)]
    pub force_ocr: bool,
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
    /// Maximum simultaneous OCR requests.
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Disable the content-addressed OCR cache.
    #[arg(long)]
    pub no_cache: bool,
    /// Override the OCR cache directory.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct StripArgs {
    /// Input files, directories, globs, or @list.txt manifests.
    #[arg(required = true)]
    pub inputs: Vec<String>,
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    #[arg(long, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub keep_icc: Option<bool>,
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
}
