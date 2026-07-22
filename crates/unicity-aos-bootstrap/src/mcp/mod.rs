//! Product-owned MCP endpoint and local interaction bridge.
//!
//! The pinned runtime's MCP shim remains a compatibility transport for this
//! release. AOS owns the externally visible command, server identity, and
//! interaction policy. That lets hosts without MCP form elicitation use a
//! trusted local decision surface without weakening or forking the runtime.

mod interaction;

use std::ffi::OsString;
use std::process::{ExitCode, Stdio};

use clap::{Args, ValueEnum};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

use unicity_aos_bootstrap::AosHome;

#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Choose where constrained approval forms are presented.
    #[arg(long, value_enum, default_value_t = InteractionMode::Auto)]
    interaction: InteractionMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum InteractionMode {
    /// Prefer the MCP client, falling back to the trusted local AOS provider.
    #[default]
    Auto,
    /// Require the MCP client to present interactions.
    Client,
    /// Always present constrained decisions through the local AOS provider.
    Native,
    /// Refuse interactive requests.
    Deny,
}

pub(crate) fn handle_serve(principal: Option<String>, args: ServeArgs) -> ExitCode {
    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("aos mcp serve: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("aos mcp serve: failed to start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(&home, principal.as_deref(), args.interaction)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aos mcp serve: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(
    home: &AosHome,
    principal: Option<&str>,
    mode: InteractionMode,
) -> Result<(), String> {
    let mut runtime_args = Vec::<OsString>::new();
    if let Some(principal) = principal {
        runtime_args.push(OsString::from("--principal"));
        runtime_args.push(OsString::from(principal));
    }
    runtime_args.extend([OsString::from("mcp"), OsString::from("serve")]);

    let mut standard_command = home
        .runtime_command_with_args(&runtime_args)
        .map_err(|error| format!("failed to prepare bundled runtime: {error}"))?;
    standard_command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut command = tokio::process::Command::from(standard_command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start bundled MCP compatibility transport: {error}"))?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| "bundled MCP transport did not expose stdin".to_owned())?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "bundled MCP transport did not expose stdout".to_owned())?;

    let mut upstream = BufReader::new(tokio::io::stdin()).lines();
    let mut downstream = BufReader::new(child_stdout).lines();
    let mut upstream_out = tokio::io::stdout();
    let mut downstream_in = Some(child_stdin);
    let mut client_supports_form = false;
    let mut presenter = interaction::NativePresenter;
    let mut upstream_open = true;

    loop {
        tokio::select! {
            line = upstream.next_line(), if upstream_open => {
                let Some(line) = line.map_err(|error| format!("failed to read MCP client: {error}"))? else {
                    downstream_in.take();
                    upstream_open = false;
                    continue;
                };
                let forwarded = prepare_client_message(&line, mode, &mut client_supports_form);
                write_frame(
                    downstream_in
                        .as_mut()
                        .ok_or_else(|| "bundled MCP transport input is closed".to_owned())?,
                    &forwarded,
                )
                    .await
                    .map_err(|error| format!("failed to write bundled MCP transport: {error}"))?;
            },
            line = downstream.next_line() => {
                let Some(line) = line.map_err(|error| format!("failed to read bundled MCP transport: {error}"))? else {
                    break;
                };
                let mut message = serde_json::from_str::<Value>(&line).ok();
                let handling = message
                    .as_ref()
                    .map_or(ElicitationHandling::Forward, |value| {
                        elicitation_handling(value, mode, client_supports_form)
                    });
                if handling != ElicitationHandling::Forward {
                    let request = message.as_ref().expect("handling requires parsed JSON");
                    let response = match handling {
                        ElicitationHandling::Present => {
                            match interaction::resolve(request, &mut presenter) {
                                Ok(response) => response,
                                Err(error) => {
                                    eprintln!("aos mcp serve: local interaction denied: {error}");
                                    interaction::cancelled_response(request).ok_or_else(|| {
                                        "elicitation request did not carry a response id".to_owned()
                                    })?
                                },
                            }
                        },
                        ElicitationHandling::Cancel => {
                            interaction::cancelled_response(request).ok_or_else(|| {
                                "elicitation request did not carry a response id".to_owned()
                            })?
                        },
                        ElicitationHandling::Forward => unreachable!("handled above"),
                    };
                    let response = serde_json::to_string(&response)
                        .map_err(|error| format!("failed to encode local interaction: {error}"))?;
                    write_frame(
                        downstream_in
                            .as_mut()
                            .ok_or_else(|| "bundled MCP transport input is closed".to_owned())?,
                        &response,
                    )
                        .await
                        .map_err(|error| format!("failed to answer local interaction: {error}"))?;
                    continue;
                }
                if let Some(value) = message.as_mut() {
                    rewrite_server_identity(value);
                }
                let forwarded = message
                    .as_ref()
                    .and_then(|value| serde_json::to_string(value).ok())
                    .unwrap_or(line);
                write_frame(&mut upstream_out, &forwarded)
                    .await
                    .map_err(|error| format!("failed to write MCP client: {error}"))?;
            },
        }
    }

    drop(downstream_in);
    let status = child
        .wait()
        .await
        .map_err(|error| format!("failed waiting for bundled MCP transport: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("bundled MCP transport exited with {status}"))
    }
}

async fn write_frame(
    output: &mut (impl tokio::io::AsyncWrite + Unpin),
    frame: &str,
) -> std::io::Result<()> {
    output.write_all(frame.as_bytes()).await?;
    output.write_all(b"\n").await?;
    output.flush().await
}

fn prepare_client_message(
    line: &str,
    mode: InteractionMode,
    client_supports_form: &mut bool,
) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(line) else {
        return line.to_owned();
    };
    if value.get("method").and_then(Value::as_str) != Some("initialize") {
        return line.to_owned();
    }
    *client_supports_form = supports_form_elicitation(&value);
    if matches!(mode, InteractionMode::Auto | InteractionMode::Native)
        && (mode == InteractionMode::Native || !*client_supports_form)
    {
        advertise_form_elicitation(&mut value);
    }
    serde_json::to_string(&value).unwrap_or_else(|_| line.to_owned())
}

fn supports_form_elicitation(initialize: &Value) -> bool {
    initialize
        .pointer("/params/capabilities/elicitation/form")
        .is_some_and(Value::is_object)
}

fn advertise_form_elicitation(initialize: &mut Value) {
    let Some(params) = initialize.get_mut("params").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(capabilities) = object_entry(params, "capabilities") else {
        return;
    };
    let Some(elicitation) = object_entry(capabilities, "elicitation") else {
        return;
    };
    elicitation
        .entry("form".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
}

fn object_entry<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
) -> Option<&'a mut Map<String, Value>> {
    let value = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    value.as_object_mut()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElicitationHandling {
    Forward,
    Present,
    Cancel,
}

fn elicitation_handling(
    message: &Value,
    mode: InteractionMode,
    client_supports_form: bool,
) -> ElicitationHandling {
    if message.get("method").and_then(Value::as_str) != Some("elicitation/create") {
        return ElicitationHandling::Forward;
    }
    if mode == InteractionMode::Deny {
        return ElicitationHandling::Cancel;
    }
    if message
        .pointer("/params/mode")
        .and_then(Value::as_str)
        .unwrap_or("form")
        != "form"
    {
        return ElicitationHandling::Forward;
    }
    match mode {
        InteractionMode::Auto if !client_supports_form => ElicitationHandling::Present,
        InteractionMode::Native => ElicitationHandling::Present,
        InteractionMode::Auto | InteractionMode::Client => ElicitationHandling::Forward,
        InteractionMode::Deny => unreachable!("handled above"),
    }
}

fn rewrite_server_identity(message: &mut Value) {
    if message.pointer("/result/protocolVersion").is_none()
        || message.pointer("/result/capabilities").is_none()
    {
        return;
    }
    let Some(info) = message
        .pointer_mut("/result/serverInfo")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    info.insert("name".to_owned(), Value::String("unicity-aos".to_owned()));
    info.insert("title".to_owned(), Value::String("Unicity AOS".to_owned()));
    info.insert(
        "version".to_owned(),
        Value::String(env!("CARGO_PKG_VERSION").to_owned()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn initialize(capabilities: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": capabilities,
                "clientInfo": { "name": "test", "version": "1" }
            }
        })
        .to_string()
    }

    #[test]
    fn auto_advertises_form_only_when_client_cannot_present_it() {
        let mut supported = false;
        let forwarded = prepare_client_message(
            &initialize(json!({ "roots": {} })),
            InteractionMode::Auto,
            &mut supported,
        );
        let forwarded: Value = serde_json::from_str(&forwarded).expect("json");
        assert!(!supported);
        assert!(
            forwarded
                .pointer("/params/capabilities/elicitation/form")
                .is_some()
        );

        let mut supported = false;
        let forwarded = prepare_client_message(
            &initialize(json!({ "elicitation": { "form": {} } })),
            InteractionMode::Auto,
            &mut supported,
        );
        let forwarded: Value = serde_json::from_str(&forwarded).expect("json");
        assert!(supported);
        assert!(
            forwarded
                .pointer("/params/capabilities/elicitation/form")
                .is_some()
        );
    }

    #[test]
    fn client_and_deny_modes_never_invent_capabilities() {
        for mode in [InteractionMode::Client, InteractionMode::Deny] {
            let mut supported = false;
            let forwarded = prepare_client_message(&initialize(json!({})), mode, &mut supported);
            let forwarded: Value = serde_json::from_str(&forwarded).expect("json");
            assert!(
                forwarded
                    .pointer("/params/capabilities/elicitation/form")
                    .is_none()
            );
        }
    }

    #[test]
    fn malformed_initialize_capabilities_are_not_rewritten() {
        let malformed = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": "not-an-object" }
        })
        .to_string();
        let mut supported = false;
        let forwarded = prepare_client_message(&malformed, InteractionMode::Auto, &mut supported);
        let forwarded: Value = serde_json::from_str(&forwarded).expect("json");
        assert_eq!(forwarded["params"]["capabilities"], "not-an-object");
        assert!(!supported);
    }

    #[test]
    fn auto_intercepts_only_when_the_client_lacks_form_support() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "elicitation/create",
            "params": { "mode": "form" }
        });
        assert_eq!(
            elicitation_handling(&request, InteractionMode::Auto, false),
            ElicitationHandling::Present
        );
        assert_eq!(
            elicitation_handling(&request, InteractionMode::Auto, true),
            ElicitationHandling::Forward
        );
        assert_eq!(
            elicitation_handling(&request, InteractionMode::Native, true),
            ElicitationHandling::Present
        );
    }

    #[test]
    fn url_elicitation_is_never_intercepted_as_a_local_form() {
        let request = json!({
            "method": "elicitation/create",
            "params": { "mode": "url" }
        });
        assert_eq!(
            elicitation_handling(&request, InteractionMode::Native, false),
            ElicitationHandling::Forward
        );
    }

    #[test]
    fn deny_mode_cancels_form_and_url_elicitation() {
        for request in [
            json!({ "method": "elicitation/create", "params": { "mode": "form" } }),
            json!({ "method": "elicitation/create", "params": { "mode": "url" } }),
        ] {
            assert_eq!(
                elicitation_handling(&request, InteractionMode::Deny, true),
                ElicitationHandling::Cancel
            );
        }
    }

    #[test]
    fn initialize_response_is_product_branded() {
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "serverInfo": { "name": "astrid", "version": "0.10.4" }
            }
        });
        rewrite_server_identity(&mut response);
        assert_eq!(response["result"]["serverInfo"]["name"], "unicity-aos");
        assert_eq!(response["result"]["serverInfo"]["title"], "Unicity AOS");
    }
}
