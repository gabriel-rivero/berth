// build.rs: embeds berth.ico into the Windows binary (taskbar/Explorer icon + versions).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            // Requires the .ico at build time; fails builds when unset only on windows targets.
            .set_icon("../../assets/berth.ico")
            .compile()
            .expect("windows resource compilation");
    }
}
