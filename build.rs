use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown-profile".to_string());
    let git_sha = git_sha().unwrap_or_else(|| "unknown".to_string());
    let long_version = format!(
        "{}\ncommit: {git_sha}\ntarget: {target}\nprofile: {profile}",
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string())
    );

    println!("cargo:rustc-env=TGBACKY_LONG_VERSION={long_version}");
}

fn git_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let trimmed = sha.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
