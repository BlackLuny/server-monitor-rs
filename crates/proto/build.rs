use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from("proto");
    let proto_files = [proto_root.join("monitor/v1/agent.proto")];

    // Re-run the build script if any .proto file changes.
    for file in &proto_files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    println!("cargo:rerun-if-changed=proto");

    let build_server = cfg!(feature = "server");
    let build_client = cfg!(feature = "client");

    tonic_build::configure()
        .build_server(build_server)
        .build_client(build_client)
        .compile_protos(
            &proto_files
                .iter()
                .map(|p| p.to_str().expect("non-utf8 path"))
                .collect::<Vec<_>>(),
            &[proto_root.to_str().expect("non-utf8 path")],
        )?;

    Ok(())
}
