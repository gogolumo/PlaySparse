fn main() {
    println!(
        "cargo:rustc-env=ENGINE_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=dylib=delayimp");
        println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-x64.dll");
    }
    tauri_build::build()
}
