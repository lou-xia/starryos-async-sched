use std::{fs, path::{Path, PathBuf}};

use crate::BuildConfig;

fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from = fs::canonicalize(from).unwrap();
    let to = fs::canonicalize(to).unwrap();
    let mut from_comps = from.components().peekable();
    let mut to_comps = to.components().peekable();
    while from_comps.peek() == to_comps.peek() {
        from_comps.next();
        to_comps.next();
    }
    let mut result = PathBuf::new();
    for _ in from_comps {
        result.push("..");
    }
    for comp in to_comps {
        result.push(comp);
    }
    result
}

pub(crate) fn gen_wrapper(config: &BuildConfig) {
    let lib_path = Path::new(&config.out_dir).join("vdso_wrapper");
    let src_path = lib_path.join("src");
    fs::create_dir_all(&src_path).unwrap();
    let cargo_toml = cargo_toml_content(config);
    let lib_rs = lib_rs_content(config);

    fs::write(&lib_path.join("Cargo.toml"), cargo_toml).unwrap();
    fs::write(&src_path.join("lib.rs"), lib_rs).unwrap();
}

fn cargo_toml_content(config: &BuildConfig) -> String {
    let mut features = config.features.join("\", \"");
    if !config.features.is_empty() {
        features = String::from("\"") + &features + "\"";
    }
    let out_dir = fs::canonicalize(Path::new(&config.out_dir)).unwrap();
    let src_dir = fs::canonicalize(Path::new(&config.src_dir)).unwrap();
    let wrapper_path = out_dir.join("vdso_wrapper");
    let rel_path = relative_path(&wrapper_path, &src_dir);
    format!(
        r#"[package]
name = "vdso_wrapper"
edition = "2021"

[lib]
crate-type = ["staticlib"]

[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"

[dependencies]
{} = {{ path = "{}", features = [{}] }}
"#,
        config.package_name,
        rel_path.display(),
        features
    )
}

fn lib_rs_content(config: &BuildConfig) -> String {
    format!(
        r#"#![no_std]

pub use {}::*;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {{
    panic_loop();
}}

/// 导出此符号，从而确认当在vdso中panic时，会在哪个地址循环。
#[no_mangle]
pub fn panic_loop() -> ! {{
    loop {{}}
}}

"#,
        config.package_name,
    )
}
