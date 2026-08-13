fn main() {
    if let Err(error) = bpdf::run(std::env::args_os().collect()) {
        bpdf::report_error(&error);
        std::process::exit(1);
    }
}
