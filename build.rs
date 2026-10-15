//! Build steps beyond Cargo's: on a Mac, the small C++ face Nida's Blackmagic
//! outputs put on the DeckLink API (`src/internal/decklink`), built against
//! Blackmagic's DeckLink SDK headers in `vendor/decklink-sdk`. The API itself
//! is loaded at run time from Desktop Video, so nothing is linked to it.

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
}
