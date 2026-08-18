use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use serde_json::{Value, json};

const NORMAL: u8 = 0;
const QUIET: u8 = 1;
const JSON: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(NORMAL);

pub fn init(quiet: bool, json_mode: bool) {
    MODE.store(
        if json_mode {
            JSON
        } else if quiet {
            QUIET
        } else {
            NORMAL
        },
        Ordering::Relaxed,
    );
}

pub fn is_json() -> bool {
    MODE.load(Ordering::Relaxed) == JSON
}

pub fn info(message: impl AsRef<str>) {
    emit("info", "progress", message.as_ref(), None, false);
}

pub fn warn(message: impl AsRef<str>) {
    emit("warning", "warning", message.as_ref(), None, false);
}

pub fn error(message: impl AsRef<str>) {
    emit("error", "error", message.as_ref(), None, true);
}

pub fn result(event: &str, message: impl AsRef<str>, data: Value) {
    emit("info", event, message.as_ref(), Some(data), false);
}

pub fn written(path: &Path) {
    result(
        "written",
        format!("Written {}", path.display()),
        json!({"path": path.to_string_lossy()}),
    );
}

/// Print command data. Unlike progress, data is retained in quiet mode.
pub fn data(event: &str, text: &str, data: Value) {
    match MODE.load(Ordering::Relaxed) {
        JSON => emit("info", event, event, Some(data), false),
        _ => print!("{text}"),
    }
}

fn emit(level: &str, event: &str, message: &str, data: Option<Value>, force: bool) {
    match MODE.load(Ordering::Relaxed) {
        QUIET if !force => {}
        JSON => {
            let mut value = json!({
                "level": level,
                "event": event,
                "message": message,
            });
            if let Some(data) = data {
                value["data"] = data;
            }
            println!("{value}");
        }
        _ => {
            let prefix = match level {
                "warning" => "Warning: ",
                "error" => "Error: ",
                _ => "",
            };
            eprintln!("{prefix}{message}");
        }
    }
}
