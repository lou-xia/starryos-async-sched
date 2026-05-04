use build_vdso::*;

fn main() {
    // 检测 rust-analyzer 环境，跳过 vDSO 构建。
    //
    // rust-analyzer 在 proc macro server 中会设置 RUST_ANALYZER，
    // 但在运行 build script 时不一定设置该变量。
    // 因此增加 __CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS 作为额外检测——
    // 这是 rust-analyzer 强制 nightly channel 的内部环境变量。
    // 之后需要删掉，目前只是为了让我看着舒服点（）
    if std::env::var("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS").is_ok() {
        return;
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=/home/lou-xia/lou-xia/vsched/vsched2");
    println!("cargo:rerun-if-changed=../build_vdso");

    let mut config = BuildConfig::new("/home/lou-xia/lou-xia/vsched/vsched2", "vsched2");
    config.so_name = String::from("libvsched2");
    config.api_lib_name = String::from("libvsched2");
    config.out_dir = String::from("../vdso_vsched2_output");
    config.toolchain = String::from("nightly-2025-12-12");
    config.verbose = 2;
    build_vdso(&config);
}
