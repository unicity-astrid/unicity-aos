//! Scaffold templates for `scaffold_capsule`.
//!
//! Produces a complete, *compiling* tool-capsule skeleton as a
//! `path -> content` map. Kept in its own module so the per-file format
//! helpers stay small and the main tool body reads as a manifest of files.

use serde_json::{Map, Value};

/// Build the full `path -> content` file map for a tool capsule named `name`.
///
/// `crate_name` is `name` verbatim; the wasm artifact is `name` with hyphens
/// turned into underscores plus `.wasm` (the kernel resolves the component by
/// that file name).
pub(crate) fn tool_skeleton(name: &str) -> Value {
    let wasm = format!("{}.wasm", name.replace('-', "_"));
    let mut files = Map::new();
    files.insert(".cargo/config.toml".into(), Value::String(cargo_config()));
    files.insert("rust-toolchain.toml".into(), Value::String(toolchain()));
    files.insert("Cargo.toml".into(), Value::String(cargo_toml(name)));
    files.insert(
        "Capsule.toml".into(),
        Value::String(capsule_toml(name, &wasm)),
    );
    files.insert("src/lib.rs".into(), Value::String(lib_rs()));
    Value::Object(files)
}

fn cargo_config() -> String {
    // The getrandom flag is the #1 silent footgun — without it uuid/HashMap
    // fail to LINK on wasm32-unknown-unknown.
    "[build]\n\
target = \"wasm32-unknown-unknown\"\n\
\n\
[target.wasm32-unknown-unknown]\n\
rustflags = [\"--cfg=getrandom_backend=\\\"custom\\\"\"]\n"
        .to_string()
}

fn toolchain() -> String {
    "[toolchain]\n\
channel = \"1.94.0\"\n\
targets = [\"wasm32-unknown-unknown\"]\n\
components = [\"rustfmt\", \"clippy\"]\n"
        .to_string()
}

fn cargo_toml(name: &str) -> String {
    format!(
        "[package]\n\
name = \"{name}\"\n\
version = \"0.1.0\"\n\
edition = \"2024\"\n\
license = \"MIT OR Apache-2.0\"\n\
publish = false\n\
\n\
[lib]\n\
crate-type = [\"cdylib\"]\n\
\n\
[dependencies]\n\
astrid-sdk = {{ version = \"0.7\", features = [\"derive\"] }}\n\
serde = {{ version = \"1.0\", features = [\"derive\"] }}\n\
serde_json = \"1.0\"\n\
\n\
[profile.release]\n\
opt-level = \"z\"\n\
lto = true\n\
codegen-units = 1\n\
strip = true\n\
panic = \"abort\"\n"
    )
}

fn capsule_toml(name: &str, wasm: &str) -> String {
    format!(
        "[package]\n\
name = \"{name}\"\n\
version = \"0.1.0\"\n\
description = \"A new Unicity AOS tool capsule\"\n\
authors = [\"Your Name <you@example.com>\"]\n\
astrid-version = \">=0.7.0\"\n\
\n\
[[component]]\n\
id = \"{name}\"\n\
file = \"{wasm}\"\n\
type = \"executable\"\n\
\n\
[capabilities]\n\
fs_read = [\"home://\"]\n\
\n\
[publish]\n\
\"tool.v1.execute.*.result\" = {{ wit = \"@unicity-astrid/wit/types/tool-call-result\" }}\n\
\"tool.v1.response.describe.*\" = {{ wit = \"@unicity-astrid/wit/tool/describe-response\" }}\n\
\n\
[subscribe]\n\
\"tool.v1.execute.hello\" = {{ wit = \"@unicity-astrid/wit/types/tool-call\", handler = \"tool_execute_hello\" }}\n\
\"tool.v1.request.describe\" = {{ wit = \"@unicity-astrid/wit/tool/describe-request\", handler = \"tool_describe\" }}\n"
    )
}

fn lib_rs() -> String {
    "#![deny(unsafe_code)]\n\
#![deny(clippy::all)]\n\
\n\
//! A new Unicity AOS tool capsule. Replace `hello` with your own tools.\n\
\n\
use astrid_sdk::prelude::*;\n\
use astrid_sdk::schemars;\n\
use serde::Deserialize;\n\
\n\
#[derive(Default)]\n\
pub struct Capsule;\n\
\n\
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]\n\
pub struct HelloArgs {\n\
    /// Who to greet (shown to the LLM as the parameter description).\n\
    pub name: String,\n\
}\n\
\n\
#[capsule]\n\
impl Capsule {\n\
    /// Greet someone. This doc-comment becomes the tool description the LLM sees.\n\
    #[astrid::tool(\"hello\")]\n\
    pub fn hello(&self, args: HelloArgs) -> Result<String, SysError> {\n\
        Ok(format!(\"Hello, {}!\", args.name))\n\
    }\n\
}\n"
    .to_string()
}
