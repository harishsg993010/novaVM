fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = [
        "proto/runtime.proto",
        "proto/sandbox.proto",
        "proto/sensor.proto",
        "proto/policy.proto",
    ];

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &["proto/"])?;

    // Re-run if any proto file changes.
    for proto in &protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    Ok(())
}
