use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("out dir"));

    for (directory, output_name) in [
        ("smoke-write", "smoke_write.wasm"),
        ("pwd", "pwd.wasm"),
        ("echo", "echo.wasm"),
        ("write-file", "write_file.wasm"),
        ("cat", "cat.wasm"),
        ("stdin-cat", "stdin_cat.wasm"),
        ("env", "env.wasm"),
        ("guest-pipeline", "guest_pipeline.wasm"),
        ("mini-shell", "mini_shell.wasm"),
    ] {
        let guest_source = manifest_dir.join(format!("../../guests/{directory}/{directory}.wat"));
        println!("cargo:rerun-if-changed={}", guest_source.display());
        let wasm = wat::parse_file(&guest_source)
            .unwrap_or_else(|error| panic!("compile {}: {error}", guest_source.display()));
        fs::write(output_dir.join(output_name), wasm)
            .unwrap_or_else(|error| panic!("write {output_name}: {error}"));
    }
}
