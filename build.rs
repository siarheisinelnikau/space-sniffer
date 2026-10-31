fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/app.ico")
            .set("ProductName", "Space Sniffer")
            .set("FileDescription", "Space Sniffer")
            .compile()
            .expect("could not embed the application icon");
    }
}
