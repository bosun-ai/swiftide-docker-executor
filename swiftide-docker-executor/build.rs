fn main() {
    tonic_build::compile_protos("../proto/shell.proto").unwrap();
}
