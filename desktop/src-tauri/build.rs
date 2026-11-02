fn main() {
    // macOS only: the Secure Enclave half of the dbLens for Mac importer
    // (swift/SecureEnclaveOpen.swift, called from src/import_swift.rs).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        secure_enclave::build();
    }
    tauri_build::build()
}

/// Compiles swift/SecureEnclaveOpen.swift with the Xcode (or Command Line
/// Tools) `swiftc` into a static library for the target arch, and links the
/// Swift runtime and CryptoKit. Runs once per arch, so a universal build
/// (aarch64 + x86_64, joined by lipo) gets both.
mod secure_enclave {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Keep in step with bundle.macOS.minimumSystemVersion in tauri.conf.json.
    const MACOS_MIN: &str = "13.0";
    const SOURCE: &str = "swift/SecureEnclaveOpen.swift";
    const LIB: &str = "dblens_secure_enclave";

    fn output(program: &str, args: &[&str]) -> String {
        let out = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|e| {
                panic!("couldn't run {program}: {e} (install Xcode or its Command Line Tools)")
            });
        if !out.status.success() {
            panic!(
                "{program} {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn build() {
        println!("cargo:rerun-if-changed={SOURCE}");
        println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
        let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("aarch64") => "arm64",
            Ok("x86_64") => "x86_64",
            other => panic!("unsupported macOS arch {other:?}"),
        };
        let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let sdk = output("xcrun", &["--sdk", "macosx", "--show-sdk-path"]);
        let swiftc = output("xcrun", &["--sdk", "macosx", "--find", "swiftc"]);
        let target = format!("{arch}-apple-macosx{MACOS_MIN}");
        let library = out_dir.join(format!("lib{LIB}.a"));
        let profile = std::env::var("PROFILE").unwrap_or_default();
        let optimize = if profile == "release" { "-O" } else { "-Onone" };

        let status = Command::new(&swiftc)
            .args(["-parse-as-library", "-emit-library", "-static"])
            .args(["-module-name", "DBLensSecureEnclave"])
            .args(["-target", &target, "-sdk", &sdk, optimize])
            .arg(SOURCE)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap_or_else(|e| panic!("couldn't run {swiftc}: {e}"));
        if !status.success() {
            panic!("swiftc failed to compile {SOURCE} for {target}");
        }

        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static={LIB}");
        println!("cargo:rustc-link-lib=framework=CryptoKit");
        println!("cargo:rustc-link-lib=framework=Foundation");
        // The object files ask for the Swift runtime (-lswiftCore, the
        // compatibility shims) by autolink: the SDK has its .tbd stubs, the
        // toolchain the static shims. At run time it is the OS's /usr/lib/swift.
        let toolchain_lib = Path::new(&swiftc)
            .parent()
            .and_then(Path::parent)
            .map(|usr| usr.join("lib/swift/macosx"));
        for dir in [Some(Path::new(&sdk).join("usr/lib/swift")), toolchain_lib]
            .into_iter()
            .flatten()
            .filter(|d| d.is_dir())
        {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
