//! Build steps beyond Cargo's: the small C++ face Nida's Blackmagic
//! outputs put on the DeckLink API (`src/internal/decklink`), built against
//! Blackmagic's DeckLink SDK headers in `vendor/decklink-sdk`. The API itself
//! is loaded at run time from Desktop Video, so nothing is linked to it.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/internal/decklink/shim.cpp");
    println!("cargo:rerun-if-changed=src/internal/decklink/platform.hpp");
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
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build_windows_decklink();
    }
}

fn build_windows_decklink() {
    use std::path::PathBuf;
    use std::process::Command;

    println!("cargo:rerun-if-changed=vendor/decklink-sdk/win");
    println!("cargo:rerun-if-env-changed=MIDL");
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Windows DeckLink requires the MSVC toolchain and Windows SDK (midl.exe)"
    );
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("aarch64") => "arm64",
        Ok("x86") => "win32",
        other => panic!("unsupported Windows DeckLink target architecture: {other:?}"),
    };
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let sdk = PathBuf::from("vendor/decklink-sdk/win");
    let compiler = cc::Build::new().cpp(true).get_compiler();
    // cc discovers the installed MSVC/Windows SDK even outside a Developer
    // Prompt. Use the same PATH and INCLUDE for MIDL's C preprocessor.
    let search = compiler
        .get_envs()
        .rev()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.to_owned())
        .or_else(|| std::env::var_os("PATH"));
    let midl = std::env::var_os("MIDL")
        .map(PathBuf::from)
        .or_else(|| {
            search.as_ref().and_then(|paths| {
                std::env::split_paths(paths)
                    .map(|dir| dir.join("midl.exe"))
                    .find(|file| file.is_file())
            })
        })
        .expect("midl.exe not found: install the Windows SDK with MSVC Build Tools, or set MIDL");
    let mut command = Command::new(midl);
    command.envs(compiler.get_envs());
    let result = command
        .args([
            "/nologo",
            "/env",
            arch,
            "/char",
            "signed",
            "/h",
            "DeckLinkAPI.h",
            "/iid",
            "DeckLinkAPI_i.c",
            "/tlb",
            "DeckLinkAPI.tlb",
            "/out",
        ])
        .arg(&out)
        .arg("/I")
        .arg(&sdk)
        .arg(sdk.join("DeckLinkAPI.idl"))
        .status()
        .expect("run Windows SDK midl.exe");
    assert!(
        result.success(),
        "MIDL failed to generate the Windows DeckLink SDK interfaces"
    );
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include(&out)
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        .file("src/internal/decklink/shim.cpp")
        .warnings(true)
        .compile("nida_decklink");
    cc::Build::new()
        .file(out.join("DeckLinkAPI_i.c"))
        .warnings(false)
        .compile("nida_decklink_guids");
    println!("cargo:rustc-link-lib=ole32");
    println!("cargo:rustc-link-lib=oleaut32");
    println!("cargo:rustc-link-lib=uuid");
}
