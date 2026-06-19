fn main() {
    if cfg!(target_os = "windows") {
        winres::WindowsResource::new()
            .set_manifest_file("app.manifest")
            .set_icon("favicon.ico")
            .compile()
            .unwrap();
    }
}