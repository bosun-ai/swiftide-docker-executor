fn main() {
    tonic_prost_build::configure()
        .bytes(".shell.ShellEvent.stdout")
        .bytes(".shell.ShellEvent.stderr")
        .compile_protos(&["proto/shell.proto", "proto/loader.proto"], &["proto"])
        .unwrap();
}
