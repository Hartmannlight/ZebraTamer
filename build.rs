use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=ZPL_AGENT_GIT_COMMIT");
    let commit = std::env::var("ZPL_AGENT_GIT_COMMIT")
        .ok()
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ZPL_AGENT_GIT_COMMIT={commit}");
}
