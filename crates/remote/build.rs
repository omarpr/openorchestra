//! Embeds the remote web client (`web/remote/dist`) when it has been built.
//! Without it, sets cfg `oo_remote_no_web` and the server answers every page
//! request with a 503 that says how to build it (`make web`).

use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(oo_remote_no_web)");
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    let dist = web.join("remote/dist");
    if dist.join("index.html").is_file() {
        // Directory: cargo rescans it, so rebuilt assets re-embed.
        println!("cargo::rerun-if-changed={}", dist.display());
        return;
    }
    println!("cargo::rustc-cfg=oo_remote_no_web");
    // Watch the deepest existing ancestor so a later `make web` is noticed.
    // While even `web/` is missing, the missing path keeps this script dirty
    // (rerun every build) rather than caching the no-web state forever.
    let watch = [web.join("remote"), web.clone()]
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or(dist);
    println!("cargo::rerun-if-changed={}", watch.display());
}
