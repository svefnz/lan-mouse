use shadow_rs::ShadowBuilder;

use std::{env, path::PathBuf, process::Command};

fn main() {
    ShadowBuilder::builder()
        .deny_const(Default::default())
        .build()
        .expect("shadow build");

    let unix = cfg!(unix);
    let macos = cfg!(target_os = "macos");

    let layer_shell_capture = cfg!(feature = "layer_shell_capture");
    let libei_capture = cfg!(feature = "libei_capture");
    let x11_capture = cfg!(feature = "x11_capture");

    let libei_emulation = cfg!(feature = "libei_emulation");
    let x11_emulation = cfg!(feature = "x11_emulation");
    let wlroots_emulation = cfg!(feature = "wlroots_emulation");
    let rdp_emulation = cfg!(feature = "rdp_emulation");

    let layer_shell_capture = unix && !macos && layer_shell_capture;
    let libei_capture = unix && !macos && libei_capture;
    let x11_capture = unix && !macos && x11_capture;

    let libei_emulation = unix && !macos && libei_emulation;
    let rdp_emulation = unix && !macos && rdp_emulation;
    let wlroots_emulation = unix && !macos && wlroots_emulation;
    let x11_emulation = unix && !macos && x11_emulation;

    println!("cargo::rustc-check-cfg=cfg(layer_shell_capture)");
    println!("cargo::rustc-check-cfg=cfg(libei_capture)");
    println!("cargo::rustc-check-cfg=cfg(x11_capture)");

    println!("cargo::rustc-check-cfg=cfg(libei_emulation)");
    println!("cargo::rustc-check-cfg=cfg(rdp_emulation)");
    println!("cargo::rustc-check-cfg=cfg(wlroots_emulation)");
    println!("cargo::rustc-check-cfg=cfg(x11_emulation)");

    if layer_shell_capture {
        println!("cargo::rustc-cfg=layer_shell_capture");
    }
    if libei_capture {
        println!("cargo::rustc-cfg=libei_capture");
    }
    if x11_capture {
        println!("cargo::rustc-cfg=x11_capture");
    }

    if libei_emulation {
        println!("cargo::rustc-cfg=libei_emulation");
    }
    if rdp_emulation {
        println!("cargo::rustc-cfg=rdp_emulation");
    }
    if wlroots_emulation {
        println!("cargo::rustc-cfg=wlroots_emulation");
    }
    if x11_emulation {
        println!("cargo::rustc-cfg=x11_emulation");
    }

    if cfg!(feature = "mygo") {
        build_ui();
    }
}

/// Build the Go frontend into the cargo target directory, so a plain
/// `cargo build` produces a complete application (design D5).
fn build_ui() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let frontend = manifest_dir.join("frontend-mygo");
    println!("cargo::rerun-if-changed={}", frontend.display());

    // OUT_DIR is <target>/<profile>/build/<crate>-<hash>/out; the UI
    // binary belongs next to the lan-mouse binary, in <target>/<profile>
    let profile_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"))
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is not inside a target directory")
        .to_path_buf();

    let go = env::var("GO").unwrap_or_else(|_| "go".to_string());
    match Command::new(&go).arg("version").status() {
        Ok(status) if status.success() => {}
        Ok(status) => panic!(
            "`{go} version` failed with {status}.\n\
             The `mygo` feature builds the frontend in `frontend-mygo` with the Go\n\
             toolchain (>= 1.27): install Go from https://go.dev/dl/, point the `GO`\n\
             environment variable at your go binary, or build the GTK frontend instead:\n\
             cargo build --no-default-features --features gtk"
        ),
        Err(err) => panic!(
            "could not run `{go}`: {err}.\n\
             The `mygo` feature builds the frontend in `frontend-mygo` with the Go\n\
             toolchain (>= 1.27): install Go from https://go.dev/dl/, point the `GO`\n\
             environment variable at your go binary, or build the GTK frontend instead:\n\
             cargo build --no-default-features --features gtk"
        ),
    }

    let mut binary = profile_dir.join("lan-mouse-ui");
    if cfg!(target_os = "windows") {
        binary.set_extension("exe");
    }

    // GOOS and GOARCH are inherited for cross compiling; cross compiling
    // the UI is not solved beyond that (documented limitation)
    let status = Command::new(&go)
        .arg("build")
        .arg("-o")
        .arg(&binary)
        .arg(".")
        .current_dir(&frontend)
        .status()
        .expect("go vanished between version check and build");
    if !status.success() {
        panic!("building `frontend-mygo` failed: {go} build exited with {status}");
    }
}
