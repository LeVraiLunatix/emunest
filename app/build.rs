// Icône et métadonnées de l'exécutable Windows (nécessite rc.exe du SDK Windows ; sinon on continue sans icône).
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        res.set("ProductName", "EmuNest");
        res.set("FileDescription", "EmuNest — launcher d'émulateurs");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icône de l'exe non intégrée : {e}");
        }
    }
}
