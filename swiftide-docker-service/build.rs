fn main() {
    tonic_prost_build::configure()
        .compile_protos(&["proto/shell.proto", "proto/loader.proto"], &["proto"])
        .unwrap();
}
