//! Embeds the version resource in Xiro.exe (code signing needs it).
//! XIRO_REQUIRE_VERSION_INFO=1 fails the build when rc.exe/windres is missing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=XIRO_REQUIRE_VERSION_INFO");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The exe's folder may be writable by anyone: imported DLLs load from System32 only.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set("ProductName", "Xiro")
        .set("FileDescription", "Xiro: iPad or iPhone as a second monitor")
        .set("CompanyName", "Xiro contributors")
        .set("LegalCopyright", "MIT License, Xiro contributors")
        .set("OriginalFilename", "Xiro.exe")
        .set("InternalName", "Xiro");
    if let Err(err) = resource.compile() {
        if std::env::var_os("XIRO_REQUIRE_VERSION_INFO").is_some() {
            panic!("could not add version information to Xiro.exe: {err}");
        }
        println!("cargo:warning=Xiro.exe built without version information ({err})");
    }
}
