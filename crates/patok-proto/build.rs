fn main() {
    let proto_root = "../../proto";
    let proto = "../../proto/patok/v1/engine.proto";
    println!("cargo:rerun-if-changed={proto}");
    // Vendored protoc: no system protobuf compiler needed in CI or for contributors.
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    let mut config = tonic_prost_build::Config::new();
    config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .compile_with_config(config, &[proto], &[proto_root])
        .expect("compile engine.proto");
}
