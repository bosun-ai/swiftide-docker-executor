fn main() {
    tonic_build::configure()
        .build_server(false)
        .compile_protos(&["proto/shell.proto", "proto/loader.proto"], &["proto"])
        .unwrap();
}
