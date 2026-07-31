use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=BPDF_WINDOWS_RES");
    println!("cargo::rerun-if-changed=resources/bpdf.rc");
    println!("cargo::rerun-if-changed=resources/bpdf.ico");
    println!("cargo::rerun-if-changed=resources/bpdf.manifest");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let Some(resource) = env::var_os("BPDF_WINDOWS_RES").map(PathBuf::from) else {
        return;
    };
    println!("cargo::rerun-if-changed={}", resource.display());
    println!("cargo::rustc-link-arg-bin=bpdf={}", resource.display());
}
