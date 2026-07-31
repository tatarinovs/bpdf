use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run a child with bounded lifetime and concurrent pipe draining. Draining
/// both pipes prevents a verbose child from deadlocking while the parent waits.
pub fn run(mut command: Command, timeout: Duration, label: &str) -> Result<ProcessOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {label}"))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            terminate_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("{label} timed out after {} seconds", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(25));
    };

    Ok(ProcessOutput {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

pub fn require_success(output: ProcessOutput, label: &str) -> Result<ProcessOutput> {
    if output.status.success() {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw_detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let detail = clean_powershell_xml(raw_detail);
    bail!(
        "{label} exited with {}{}",
        output.status,
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )
}

fn clean_powershell_xml(value: &str) -> String {
    if !value.starts_with("#< CLIXML") {
        return value.to_owned();
    }

    let mut messages = Vec::new();
    let mut remaining = value;
    const START: &str = "<S S=\"Error\">";
    while let Some(start) = remaining.find(START) {
        remaining = &remaining[start + START.len()..];
        let Some(end) = remaining.find("</S>") else {
            break;
        };
        let message = remaining[..end]
            .replace("_x000D__x000A_", "\n")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        messages.push(message);
        remaining = &remaining[end + "</S>".len()..];
    }
    if messages.is_empty() {
        value.to_owned()
    } else {
        messages.join("")
    }
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.read_to_end(&mut bytes);
    bytes
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

#[cfg(windows)]
fn terminate_tree(pid: u32) {
    let mut command = Command::new("taskkill");
    command.args(["/PID", &pid.to_string(), "/T", "/F"]);
    hide_window(&mut command);
    let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(not(windows))]
fn terminate_tree(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::clean_powershell_xml;

    #[test]
    fn extracts_readable_powershell_errors() {
        let value = "#< CLIXML\n<Objs><S S=\"Error\">Ошибка_x000D__x000A_</S></Objs>";
        assert_eq!(clean_powershell_xml(value), "Ошибка\n");
    }
}
