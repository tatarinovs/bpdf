fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let is_inspect = args.iter().any(|a| a == "inspect");

    let result = bpdf::run(args);
    if let Err(error) = &result {
        bpdf::report_error(error);
        pause_if_standalone();
        std::process::exit(1);
    } else if is_inspect {
        pause_if_standalone();
    }
}

fn pause_if_standalone() {
    #[cfg(windows)]
    {
        if bpdf::is_json_mode() {
            return;
        }

        // Check if we are the only process attached to this console
        let mut processes = [0u32; 2];
        let count =
            unsafe { windows::Win32::System::Console::GetConsoleProcessList(&mut processes) };
        if count == 1 {
            println!("\nPress Enter to exit...");
            let mut buf = String::new();
            let _ = std::io::stdin().read_line(&mut buf);
        }
    }
}
