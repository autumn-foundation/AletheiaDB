use std::process::Command;
use std::env;

fn main() {
    // Check if we are running in a CI environment (GitHub Actions)
    if env::var("GITHUB_ACTIONS").is_ok() {
        // Only run this on Linux, as Verus verification is likely Linux-specific in the PR Tier workflow
        if cfg!(target_os = "linux") {
            println!("cargo:warning=Ensuring Rust 1.93.0 is installed for Verus...");

            // Install Rust 1.93.0
            // Verus strictly requires this version, but 'cargo fuzz' requires nightly.
            // We use rust-toolchain.toml to set 'nightly' as the active toolchain for the directory (for fuzzing),
            // and we install 1.93.0 here so Verus can find it.
            let status = Command::new("rustup")
                .args(&["toolchain", "install", "1.93.0", "--profile", "minimal", "--no-self-update"])
                .status();

            match status {
                Ok(s) if s.success() => println!("cargo:warning=Successfully installed/verified Rust 1.93.0"),
                Ok(s) => println!("cargo:warning=Rust 1.93.0 install exited with code: {:?}", s.code()),
                Err(e) => println!("cargo:warning=Failed to execute rustup: {}", e),
            }
        }
    }

    println!("cargo:rerun-if-changed=build.rs");
}
