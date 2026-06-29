use build_vdso::*;

fn main() {
    if std::env::var("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS").is_ok() {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let vsched2_path = format!("{}/../deps/vsched2", manifest_dir);
    let vqueue_path = format!("{}/../deps/vqueue_vdso", manifest_dir);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", vsched2_path);
    println!("cargo:rerun-if-changed={}/src", vqueue_path);
    println!("cargo:rerun-if-changed=../deps/build_vdso");

    let mut config = BuildConfig::new(&vsched2_path, "vsched2");
    config.so_name = String::from("libvsched2");
    config.api_lib_name = String::from("libvsched2");
    config.out_dir = String::from("../vdso_vsched2_output");
    config.toolchain = String::from("nightly-2025-12-12");
    config.verbose = 2;
    config.features = vec![String::from("vdso_only")];
    config.log = true;
    build_vdso(&config);

    let mut config = BuildConfig::new(&vqueue_path, "vqueue");
    config.so_name = String::from("libvqueue");
    config.api_lib_name = String::from("libvqueue");
    config.out_dir = String::from("../vdso_vqueue_output");
    config.toolchain = String::from("nightly-2025-12-12");
    config.verbose = 2;
    build_vdso(&config);
}
