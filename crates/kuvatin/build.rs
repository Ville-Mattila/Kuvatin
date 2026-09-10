fn main() {
    // The Slint compiler recurses per nested expression; a long callback (or a
    // deep component tree) overflowed the build script's default 1 MB stack.
    // Compile on a thread with room to spare instead of trimming the UI.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| slint_build::compile("ui/app.slint").expect("slint compile failed"))
        .expect("spawn slint compile thread")
        .join()
        .expect("slint compile thread panicked");

    // Embed the application icon into the Windows executable so the taskbar and
    // Explorer show it (the frameless window takes its taskbar icon from here).
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/kuvatin.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed windows icon resource: {e}");
        }
    }
}
