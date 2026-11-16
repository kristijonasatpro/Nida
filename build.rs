//! Build steps beyond Cargo's: on a Mac, the small C++ face Nida's Blackmagic
//! outputs put on the DeckLink API (`src/internal/decklink`), built against
//! Blackmagic's DeckLink SDK headers in `vendor/decklink-sdk`. The API itself
//! is loaded at run time from Desktop Video, so nothing is linked to it. On
//! Apple silicon, WebRTC's echo canceller is linked from `vendor/webrtc-aec3`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/internal/decklink/shim.cpp");
    println!("cargo:rerun-if-changed=vendor/decklink-sdk/mac");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .include("vendor/decklink-sdk/mac")
            .file("src/internal/decklink/shim.cpp")
            .file("vendor/decklink-sdk/mac/DeckLinkAPIDispatch.cpp")
            .warnings(false)
            .compile("nida_decklink");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Accelerate");
    }
    // WebRTC's echo canceller, prebuilt for Apple silicon (see
    // vendor/webrtc-aec3/README.md); elsewhere the echo removal is left out.
    println!("cargo:rerun-if-changed=vendor/webrtc-aec3/lib/libkk_aec_all.a");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
    {
        let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
            .join("vendor/webrtc-aec3/lib");
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=kk_aec_all");
        println!("cargo:rustc-link-lib=c++");
    }
}
