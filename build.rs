fn main() {
    if let Ok(path) = std::env::var("WINDIVERT_PATH") {
        println!("cargo:rustc-link-search=native={path}");
    }

    // Keep the DLL import library in repo so builds do not depend on a local SDK folder
    println!("cargo:rustc-link-search=native=lib");
    println!("cargo:rustc-link-lib=dylib=WinDivert");

    println!("cargo:rerun-if-env-changed=WINDIVERT_PATH");
    println!("cargo:rerun-if-changed=lib/WinDivert.lib");
    println!("cargo:rerun-if-changed=lib/WinDivert.def");
}
