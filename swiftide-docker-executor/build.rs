use std::process::Command;
use std::{env, fs, path::PathBuf};

fn main() {
    // If any of these change, re-run this build script.
    // println!("cargo:rerun-if-changed=build.rs");
    // println!("cargo:rerun-if-changed=../swiftide-docker-service/Cargo.toml");
    // println!("cargo:rerun-if-changed=../swiftide-docker-service/src");

    tonic_build::compile_protos("../proto/shell.proto").unwrap();
    // NOTE: Earlier attempt to embed the binary
    // Problem is that this requires cross compilation on _all_ platforms

    // // 1) Build the `server` crate in release mode
    // let status = Command::new("cargo")
    //     .args(&["build", "--release", "-p", "swiftide-docker-service"])
    //     .status()
    //     .expect("Failed to invoke cargo build for the `swiftide-docker-service` crate");
    // if !status.success() {
    //     panic!("Failed to build the `swiftide-docker-service` crate");
    // }
    //
    // // 2) Copy the built binary (../target/release/server) into src/resources/server
    // let compiled_binary = PathBuf::from("../target/release/swiftide-docker-service"); // or "server.exe" on Windows
    // let resource_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
    //     .join("src")
    //     .join("resources");
    // fs::create_dir_all(&resource_dir).expect("failed to create resources folder");
    //
    // let destination = resource_dir.join("swiftide-docker-service");
    // fs::copy(&compiled_binary, &destination)
    //     .expect("Failed to copy the `swiftide-docker-service` binary into resources folder");
}
