fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    // Checked before any child process (ffmpeg, browser, Office) can attach to the console.
    let standalone = owns_console();

    let result = bpdf::run(args);
    if let Err(error) = &result {
        bpdf::report_error(error);
        pause_if(standalone);
        std::process::exit(bpdf::exit_code(error));
    } else if bpdf::shows_report() {
        pause_if(standalone);
    }
}

/// True when bpdf is the only process attached to its console, i.e. the window
/// was created for it (Explorer, Total Commander) and closes when it exits.
fn owns_console() -> bool {
    #[cfg(windows)]
    {
        // SAFETY: Querying console process list into fixed-size array
        let mut processes = [0u32; 2];
        let count =
            unsafe { windows::Win32::System::Console::GetConsoleProcessList(&mut processes) };
        count == 1
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn pause_if(standalone: bool) {
    if !standalone || bpdf::is_json_mode() {
        return;
    }
    println!("\nPress Enter to exit...");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}
