//! Stamp the build time into the binary.
//!
//! The dashboard is `include_str!`-ed, so a running api serves the page it
//! was COMPILED with — rebuild without restarting and the browser keeps
//! showing the old UI with no hint that it is stale. That looked like a
//! bug in the page more than once. Now the page can say which build it is.

fn main() {
    let stamp = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%d %H:%M"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=BUILD_STAMP={}", stamp.trim());
    // Rebuild the stamp whenever the page itself changes.
    println!("cargo:rerun-if-changed=src/dashboard.html");
}
