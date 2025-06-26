fn main() {
    // tonic_build::compile_protos("proto/").unwrap();
    tonic_build::configure()
        .build_server(true)
        .compile_protos(&["proto/shell.proto", "proto/loader.proto"], &["proto"])
        .unwrap();
}
