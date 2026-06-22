fn main() {
    slint_build::compile("ui/app.slint").expect("slint compile failed");

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
