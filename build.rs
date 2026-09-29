use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=GREPCAL_REVISION");

    let revision = std::env::var("GREPCAL_REVISION")
        .ok()
        .filter(|revision| !revision.is_empty())
        .or_else(|| git_output(&["describe", "--always", "--dirty"]))
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=GREPCAL_REVISION={revision}");
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
