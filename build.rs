fn main() {
    if cfg!(target_os = "windows") {
        winres::WindowsResource::new()
            .set_manifest_file("app.manifest")
            .compile()
            .unwrap();
    }
}