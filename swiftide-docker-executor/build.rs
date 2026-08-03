fn main() {
    tonic_prost_build::configure()
        .bytes(".shell.ShellEvent.output")
        .bytes(".shell.ShellEvent.stdout")
        .bytes(".shell.ShellEvent.stderr")
        .build_server(false)
        .compile_protos(&["proto/shell.proto", "proto/loader.proto"], &["proto"])
        .unwrap();
}
