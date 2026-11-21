//! `oo-session.exe`: openOrchestra's stand-in for tmux on Windows (see
//! `crates/oo-session`). Built with the app so it always sits beside
//! `harness-app.exe`, where `tmux::program()` looks for it.

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(oo_session::run(&args));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("oo-session is the Windows stand-in for tmux; use tmux on this platform");
    std::process::exit(1);
}
