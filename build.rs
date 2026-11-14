// Embeds assets/icon.ico into cartridge.exe on Windows (does nothing on other systems).
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Cartridge");
        res.set("FileDescription", "Cartridge - BIOS installer for Batocera");
        res.compile().expect("failed to embed the Windows icon");
    }
}
