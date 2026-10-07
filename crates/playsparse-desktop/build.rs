use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/HEAD").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/refs/heads").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/index").display()
    );
    let revision = env::var("GITHUB_SHA")
        .ok()
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().into())
        })
        .unwrap_or_else(|| "NOT VERIFIED".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .map(|o| !o.status.success() || !o.stdout.is_empty())
        .unwrap_or(true);
    println!("cargo:rustc-env=PLAYSPARSE_BUILD_COMMIT={revision}");
    println!("cargo:rustc-env=PLAYSPARSE_BUILD_DIRTY={dirty}");
}
