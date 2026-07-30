fn main() {
    tonic_prost_build::configure()
        .bytes(".shell.ShellEvent.output")
        .compile_protos(&["proto/shell.proto", "proto/loader.proto"], &["proto"])
        .unwrap();
}
