fn main() {
    println!("cargo:rerun-if-changed=macos/Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let root = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        // codesign reads a standalone executable's identity from this section.
        println!(
            "cargo:rustc-link-arg-bin=enso=-Wl,-sectcreate,__TEXT,__info_plist,{root}/macos/Info.plist"
        );
    }
}
