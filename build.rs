fn main() {
    #[cfg(target_os = "linux")]
    flatpak_patch();
}

// this is just here so we can build and run from within vscode as a flatpak
#[cfg(target_os = "linux")]
fn flatpak_patch() {
    use std::env;
    use std::path::PathBuf;
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_dir = manifest_dir.join(".cargo/lib");
    let lib_flatpak_dir = manifest_dir.join(".cargo/lib-flatpak");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        lib_flatpak_dir.display()
    );

    for rpath_dir in [
        "/usr/lib64",
        "/usr/lib64/pulseaudio",
        "/usr/lib64/tinysparql-3.0",
        "/run/host/usr/lib64",
        "/run/host/usr/lib64/pulseaudio",
        "/run/host/usr/lib64/tinysparql-3.0",
    ] {
        println!("cargo:rustc-link-arg=-Wl,-rpath-link,{rpath_dir}");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{rpath_dir}");
    }

    println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
    println!("cargo:rustc-link-arg=-Wl,--no-as-needed");
    for soname in [
        "libxml2.so.2",
        "libsqlite3.so.0",
        "libjson-glib-1.0.so.0",
        "libgmodule-2.0.so.0",
        "libdbus-1.so.3",
        "libpulsecommon-17.0.so",
        "libpulse.so.0",
        "libpulse-simple.so.0",
        "libtinysparql-3.0.so.0",
    ] {
        println!("cargo:rustc-link-arg=-l:{soname}");
    }
    println!("cargo:rustc-link-arg=-Wl,--as-needed");
}
