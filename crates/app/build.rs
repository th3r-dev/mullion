//! Windows: put the Mullion icon in the .exe. It is resource 32512
//! (IDI_APPLICATION): Explorer shows it as the file icon, and Dioxus loads that
//! id for the window and taskbar icon instead of its own default.
//! The .ico comes from `python3 tools/make-icons.py --windows`.

fn main() {
    println!("cargo:rerun-if-changed=assets/icons/mullion.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("assets/icons/mullion.ico", "32512")
            .set("ProductName", "Mullion")
            .set("FileDescription", "Mullion");
        res.compile().expect("embed the Windows icon");
    }
}
