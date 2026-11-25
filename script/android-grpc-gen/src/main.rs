fn main() {
    let proto = std::env::var("PROTO_DIR").unwrap();
    let out = std::env::var("OUT_DIR_GEN").unwrap();
    tonic_prost_build::configure()
        .build_server(false)
        .build_transport(false)
        .out_dir(&out)
        .compile_protos(&[format!("{proto}/emulator_controller.proto")], &[proto])
        .unwrap();
}
