//! Product-native interaction handling for MCP hosts that cannot render forms.
//!
//! This boundary deliberately accepts only constrained decisions. Arbitrary
//! strings, password-shaped schemas, and URL elicitations are never collected
//! here, so a secret cannot be reflected into the MCP transport or agent
//! context by this compatibility bridge.

use std::fmt;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write as _};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

const MAX_MESSAGE_BYTES: usize = 4096;
const SAFE_APPROVAL_CHOICES: &[&str] =
    &["approve_once", "approve_session", "approve_always", "deny"];
#[cfg(unix)]
const GPG_ERR_CANCELED: u32 = 99;
#[cfg(unix)]
const GPG_ERR_NOT_CONFIRMED: u32 = 114;
#[cfg(unix)]
const GPG_ERR_CODE_MASK: u32 = 0xffff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InteractionRequest {
    message: String,
    field: String,
    response: ResponseKind,
    options: Vec<OptionValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResponseKind {
    Boolean,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OptionValue {
    label: String,
    value: Value,
    affirmative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InteractionError {
    Unsupported(&'static str),
    Invalid(&'static str),
    Unavailable(String),
}

impl fmt::Display for InteractionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) | Self::Invalid(message) => formatter.write_str(message),
            Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

pub(super) trait Presenter {
    /// Return the selected option index, or `None` when the user cancelled.
    fn present(&mut self, request: &InteractionRequest) -> Result<Option<usize>, InteractionError>;
}

/// Resolve one MCP form elicitation through a trusted local decision surface.
///
/// Every parse, provider, or cancellation failure produces the MCP `cancel`
/// action. Nothing in this function can synthesize consent.
pub(super) fn resolve(
    request: &Value,
    presenter: &mut dyn Presenter,
) -> Result<Value, InteractionError> {
    let id = request
        .get("id")
        .cloned()
        .ok_or(InteractionError::Invalid("elicitation request has no id"))?;
    let parsed = parse_request(request)?;
    let selected = presenter.present(&parsed)?;
    let Some(index) = selected else {
        return Ok(response(id, "cancel", None));
    };
    let Some(selected) = parsed.options.get(index) else {
        return Err(InteractionError::Invalid(
            "interaction provider returned an invalid choice",
        ));
    };
    let mut content = Map::new();
    content.insert(parsed.field, selected.value.clone());
    Ok(response(id, "accept", Some(Value::Object(content))))
}

pub(super) fn cancelled_response(request: &Value) -> Option<Value> {
    request
        .get("id")
        .cloned()
        .map(|id| response(id, "cancel", None))
}

fn response(id: Value, action: &str, content: Option<Value>) -> Value {
    let mut result = Map::new();
    result.insert("action".to_owned(), Value::String(action.to_owned()));
    if let Some(content) = content {
        result.insert("content".to_owned(), content);
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn parse_request(request: &Value) -> Result<InteractionRequest, InteractionError> {
    let params =
        request
            .get("params")
            .and_then(Value::as_object)
            .ok_or(InteractionError::Invalid(
                "elicitation params must be an object",
            ))?;
    if params.get("mode").and_then(Value::as_str).unwrap_or("form") != "form" {
        return Err(InteractionError::Unsupported(
            "local interaction only handles non-secret form decisions",
        ));
    }
    let message =
        params
            .get("message")
            .and_then(Value::as_str)
            .ok_or(InteractionError::Invalid(
                "elicitation message must be a string",
            ))?;
    if message.is_empty() || message.len() > MAX_MESSAGE_BYTES {
        return Err(InteractionError::Invalid(
            "elicitation message is empty or too large",
        ));
    }
    let schema = params
        .get("requestedSchema")
        .and_then(Value::as_object)
        .ok_or(InteractionError::Invalid(
            "form elicitation has no requested schema",
        ))?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(InteractionError::Invalid(
            "form elicitation schema must be an object",
        ));
    }
    let properties =
        schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or(InteractionError::Invalid(
                "form elicitation has no properties",
            ))?;
    if properties.len() != 1 {
        return Err(InteractionError::Unsupported(
            "local interaction accepts exactly one constrained decision",
        ));
    }
    let (field, property) = properties.iter().next().expect("length checked");
    let property = property
        .as_object()
        .ok_or(InteractionError::Invalid("form property must be an object"))?;
    if property.get("format").and_then(Value::as_str) == Some("password") {
        return Err(InteractionError::Unsupported(
            "local interaction refuses password-shaped fields",
        ));
    }

    let (response, options) = match property.get("type").and_then(Value::as_str) {
        Some("boolean") => (
            ResponseKind::Boolean,
            vec![
                OptionValue {
                    label: affirmative_label(field).to_owned(),
                    value: Value::Bool(true),
                    affirmative: true,
                },
                OptionValue {
                    label: "Deny".to_owned(),
                    value: Value::Bool(false),
                    affirmative: false,
                },
            ],
        ),
        Some("string") => (ResponseKind::String, parse_safe_enum(property.get("enum"))?),
        _ => {
            return Err(InteractionError::Unsupported(
                "local interaction refuses free-form or non-decision fields",
            ));
        }
    };

    Ok(InteractionRequest {
        message: message.to_owned(),
        field: field.to_owned(),
        response,
        options,
    })
}

fn parse_safe_enum(value: Option<&Value>) -> Result<Vec<OptionValue>, InteractionError> {
    let values = value
        .and_then(Value::as_array)
        .ok_or(InteractionError::Unsupported(
            "local interaction refuses unconstrained strings",
        ))?;
    if values.is_empty() || values.len() > SAFE_APPROVAL_CHOICES.len() {
        return Err(InteractionError::Unsupported(
            "approval choice set is empty or too large",
        ));
    }
    let mut options = Vec::with_capacity(values.len());
    for value in values {
        let choice = value.as_str().ok_or(InteractionError::Unsupported(
            "approval choices must be strings",
        ))?;
        if !SAFE_APPROVAL_CHOICES.contains(&choice) {
            return Err(InteractionError::Unsupported(
                "local interaction refuses an unknown approval choice",
            ));
        }
        options.push(OptionValue {
            label: choice_label(choice).to_owned(),
            value: Value::String(choice.to_owned()),
            affirmative: choice != "deny",
        });
    }
    if !options.iter().any(|option| !option.affirmative) {
        return Err(InteractionError::Unsupported(
            "approval choice set has no deny option",
        ));
    }
    Ok(options)
}

fn affirmative_label(field: &str) -> &'static str {
    match field {
        "grant" => "Grant",
        "allow" => "Allow",
        _ => "Approve",
    }
}

fn choice_label(choice: &str) -> &'static str {
    match choice {
        "approve_once" => "Approve Once",
        "approve_session" => "Approve for Session",
        "approve_always" => "Always Approve",
        "deny" => "Deny",
        _ => "Deny",
    }
}

pub(super) struct NativePresenter;

impl Presenter for NativePresenter {
    fn present(&mut self, request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
        present_platform(request)
    }
}

#[cfg(target_os = "macos")]
fn present_platform(request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
    present_appkit(request).or_else(|appkit_error| {
        present_pinentry(request).map_err(|pinentry_error| {
            InteractionError::Unavailable(format!(
                "AppKit unavailable ({appkit_error}); fallback unavailable ({pinentry_error})"
            ))
        })
    })
}

#[cfg(target_os = "macos")]
fn present_appkit(request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
    use objc2::rc::autoreleasepool;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
    use objc2_foundation::NSString;

    let main_thread = MainThreadMarker::new().ok_or_else(|| {
        InteractionError::Unavailable("AppKit interaction was not run on the main thread".into())
    })?;
    autoreleasepool(|_| {
        let _application = NSApplication::sharedApplication(main_thread);
        let alert = NSAlert::init(NSAlert::alloc(main_thread));
        alert.setMessageText(&NSString::from_str("Unicity AOS approval"));
        alert.setInformativeText(&NSString::from_str(&request.message));
        for option in &request.options {
            alert.addButtonWithTitle(&NSString::from_str(&option.label));
        }
        let response = alert.runModal();
        let offset = response - NSAlertFirstButtonReturn;
        usize::try_from(offset)
            .ok()
            .filter(|index| *index < request.options.len())
            .map(Some)
            .ok_or_else(|| {
                InteractionError::Unavailable("AppKit returned an unknown response".into())
            })
    })
}

#[cfg(target_os = "windows")]
fn present_platform(request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDNO, IDYES, MB_ICONWARNING, MB_SETFOREGROUND, MB_TASKMODAL, MB_YESNO, MessageBoxW,
    };

    let affirmative = request
        .options
        .iter()
        .position(|option| option.affirmative)
        .ok_or(InteractionError::Invalid(
            "interaction has no affirmative choice",
        ))?;
    let deny = request
        .options
        .iter()
        .position(|option| !option.affirmative)
        .ok_or(InteractionError::Invalid("interaction has no deny choice"))?;
    let mut message = request.message.encode_utf16().collect::<Vec<_>>();
    message.push(0);
    let mut title = "Unicity AOS approval".encode_utf16().collect::<Vec<_>>();
    title.push(0);
    // SAFETY: Both UTF-16 buffers are NUL-terminated and remain alive for the
    // duration of the modal call. A null owner is permitted by MessageBoxW.
    let response = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_TASKMODAL | MB_SETFOREGROUND,
        )
    };
    match response {
        IDYES => Ok(Some(affirmative)),
        IDNO => Ok(Some(deny)),
        0 => Err(InteractionError::Unavailable(format!(
            "Windows approval dialog failed: {}",
            std::io::Error::last_os_error()
        ))),
        other => Err(InteractionError::Unavailable(format!(
            "Windows approval dialog returned unknown response {other}"
        ))),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn present_platform(request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
    present_pinentry(request)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn present_platform(_request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
    Err(InteractionError::Unavailable(
        "this platform has no trusted local interaction provider".into(),
    ))
}

#[cfg(unix)]
fn present_pinentry(request: &InteractionRequest) -> Result<Option<usize>, InteractionError> {
    let executable = pinentry_path().ok_or_else(|| {
        InteractionError::Unavailable("no trusted native provider or pinentry was found".into())
    })?;
    let mut child = Command::new(&executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            InteractionError::Unavailable(format!(
                "failed to start {}: {error}",
                executable.display()
            ))
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| InteractionError::Unavailable("pinentry did not expose stdout".into()))?;
    let mut reader = BufReader::new(stdout);
    expect_pinentry_ok(&mut reader)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| InteractionError::Unavailable("pinentry did not expose stdin".into()))?;

    pinentry_command(&mut stdin, &mut reader, "SETTITLE Unicity AOS approval")?;
    pinentry_command(
        &mut stdin,
        &mut reader,
        &format!("SETDESC {}", pinentry_escape(&request.message)),
    )?;
    let affirmative = request
        .options
        .iter()
        .position(|option| option.affirmative)
        .ok_or(InteractionError::Invalid(
            "interaction has no affirmative choice",
        ))?;
    let deny = request
        .options
        .iter()
        .position(|option| !option.affirmative)
        .ok_or(InteractionError::Invalid("interaction has no deny choice"))?;
    pinentry_command(
        &mut stdin,
        &mut reader,
        &format!(
            "SETOK {}",
            pinentry_escape(&request.options[affirmative].label)
        ),
    )?;
    pinentry_command(
        &mut stdin,
        &mut reader,
        &format!("SETNOTOK {}", pinentry_escape(&request.options[deny].label)),
    )?;
    pinentry_command(&mut stdin, &mut reader, "SETCANCEL Cancel")?;
    stdin
        .write_all(b"CONFIRM\n")
        .and_then(|()| stdin.flush())
        .map_err(|error| {
            InteractionError::Unavailable(format!("pinentry write failed: {error}"))
        })?;
    let status = read_pinentry_status(&mut reader)?;
    let _ = stdin.write_all(b"BYE\n");
    let _ = child.wait();
    pinentry_selection(status, affirmative, deny)
}

#[cfg(unix)]
fn pinentry_selection(
    status: PinentryStatus,
    affirmative: usize,
    deny: usize,
) -> Result<Option<usize>, InteractionError> {
    match status {
        PinentryStatus::Ok => Ok(Some(affirmative)),
        PinentryStatus::Error(GPG_ERR_NOT_CONFIRMED) => Ok(Some(deny)),
        PinentryStatus::Error(GPG_ERR_CANCELED) => Ok(None),
        PinentryStatus::Error(code) => Err(InteractionError::Unavailable(format!(
            "pinentry confirmation failed with error code {code}"
        ))),
    }
}

#[cfg(unix)]
fn pinentry_command(
    stdin: &mut impl std::io::Write,
    reader: &mut impl BufRead,
    command: &str,
) -> Result<(), InteractionError> {
    writeln!(stdin, "{command}")
        .and_then(|()| stdin.flush())
        .map_err(|error| {
            InteractionError::Unavailable(format!("pinentry write failed: {error}"))
        })?;
    match read_pinentry_status(reader)? {
        PinentryStatus::Ok => Ok(()),
        PinentryStatus::Error(code) => Err(InteractionError::Unavailable(format!(
            "pinentry rejected its setup command with error code {code}"
        ))),
    }
}

#[cfg(unix)]
fn expect_pinentry_ok(reader: &mut impl BufRead) -> Result<(), InteractionError> {
    match read_pinentry_status(reader)? {
        PinentryStatus::Ok => Ok(()),
        PinentryStatus::Error(code) => Err(InteractionError::Unavailable(format!(
            "pinentry rejected the connection with error code {code}"
        ))),
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinentryStatus {
    Ok,
    Error(u32),
}

#[cfg(unix)]
fn read_pinentry_status(reader: &mut impl BufRead) -> Result<PinentryStatus, InteractionError> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|error| {
            InteractionError::Unavailable(format!("pinentry read failed: {error}"))
        })? == 0
        {
            return Err(InteractionError::Unavailable(
                "pinentry closed without a response".into(),
            ));
        }
        if line.starts_with("OK") {
            return Ok(PinentryStatus::Ok);
        }
        if let Some(error) = line.strip_prefix("ERR ") {
            let encoded = error
                .split_ascii_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| {
                    InteractionError::Unavailable(
                        "pinentry returned a malformed error status".into(),
                    )
                })?;
            return Ok(PinentryStatus::Error(encoded & GPG_ERR_CODE_MASK));
        }
    }
}

#[cfg(unix)]
fn pinentry_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => escaped.push_str("%25"),
            '\n' => escaped.push_str("%0A"),
            '\r' => escaped.push_str("%0D"),
            character if character.is_control() => escaped.push(' '),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(unix)]
fn pinentry_path() -> Option<PathBuf> {
    pinentry_candidates().find(|path| path.is_file())
}

#[cfg(unix)]
fn pinentry_candidates() -> impl Iterator<Item = PathBuf> {
    #[cfg(target_os = "macos")]
    const PATHS: &[&str] = &[
        "/opt/homebrew/bin/pinentry-mac",
        "/usr/local/bin/pinentry-mac",
        "/opt/homebrew/bin/pinentry",
        "/usr/local/bin/pinentry",
    ];
    #[cfg(target_os = "linux")]
    const PATHS: &[&str] = &[
        "/usr/bin/pinentry-gnome3",
        "/usr/bin/pinentry-qt",
        "/usr/bin/pinentry",
        "/usr/bin/pinentry-curses",
    ];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    const PATHS: &[&str] = &[];

    PATHS.iter().map(Path::new).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePresenter(Option<usize>);

    impl Presenter for FakePresenter {
        fn present(
            &mut self,
            _request: &InteractionRequest,
        ) -> Result<Option<usize>, InteractionError> {
            Ok(self.0)
        }
    }

    fn request(schema: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 17,
            "method": "elicitation/create",
            "params": {
                "mode": "form",
                "message": "A capsule is requesting approval.",
                "requestedSchema": schema
            }
        })
    }

    #[test]
    fn boolean_accept_returns_only_the_constrained_field() {
        let request = request(json!({
            "type": "object",
            "properties": { "grant": { "type": "boolean" } },
            "required": ["grant"]
        }));
        let resolved = resolve(&request, &mut FakePresenter(Some(0))).expect("resolve");
        assert_eq!(resolved["result"]["action"], "accept");
        assert_eq!(resolved["result"]["content"], json!({ "grant": true }));
    }

    #[test]
    fn approval_enum_preserves_explicit_choice() {
        let request = request(json!({
            "type": "object",
            "properties": {
                "choice": {
                    "type": "string",
                    "enum": ["approve_once", "approve_session", "approve_always", "deny"]
                }
            },
            "required": ["choice"]
        }));
        let resolved = resolve(&request, &mut FakePresenter(Some(1))).expect("resolve");
        assert_eq!(
            resolved["result"]["content"],
            json!({ "choice": "approve_session" })
        );
    }

    #[test]
    fn cancellation_never_synthesizes_content() {
        let request = request(json!({
            "type": "object",
            "properties": { "allow": { "type": "boolean" } }
        }));
        let resolved = resolve(&request, &mut FakePresenter(None)).expect("resolve");
        assert_eq!(resolved["result"]["action"], "cancel");
        assert!(resolved["result"].get("content").is_none());
    }

    #[test]
    fn free_form_and_secret_shaped_strings_are_refused() {
        for property in [
            json!({ "type": "string" }),
            json!({ "type": "string", "format": "password" }),
            json!({
                "type": "string",
                "format": "password",
                "enum": ["approve_once", "deny"]
            }),
            json!({ "type": "string", "enum": ["yes", "no"] }),
        ] {
            let request = request(json!({
                "type": "object",
                "properties": { "secret": property }
            }));
            assert!(matches!(
                resolve(&request, &mut FakePresenter(Some(0))),
                Err(InteractionError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn url_elicitation_is_not_opened_by_the_local_form_bridge() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": "url-1",
            "method": "elicitation/create",
            "params": {
                "mode": "url",
                "message": "Authenticate",
                "url": "https://example.invalid/",
                "elicitationId": "e1"
            }
        });
        assert!(matches!(
            resolve(&request, &mut FakePresenter(Some(0))),
            Err(InteractionError::Unsupported(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pinentry_escaping_cannot_inject_commands() {
        assert_eq!(pinentry_escape("hello%\nBYE\r"), "hello%25%0ABYE%0D");
        assert_eq!(pinentry_escape("Autoriser ✓"), "Autoriser ✓");
    }

    #[cfg(unix)]
    #[test]
    fn pinentry_status_distinguishes_deny_cancel_and_failure() {
        let mut denied = std::io::Cursor::new("ERR 83886194 Not confirmed <Pinentry>\n");
        assert_eq!(
            read_pinentry_status(&mut denied).expect("deny status"),
            PinentryStatus::Error(GPG_ERR_NOT_CONFIRMED)
        );

        let mut cancelled = std::io::Cursor::new("ERR 83886179 Operation cancelled <Pinentry>\n");
        assert_eq!(
            read_pinentry_status(&mut cancelled).expect("cancel status"),
            PinentryStatus::Error(GPG_ERR_CANCELED)
        );

        let mut failed = std::io::Cursor::new("ERR 83886140 Not supported <Pinentry>\n");
        assert_eq!(
            read_pinentry_status(&mut failed).expect("failure status"),
            PinentryStatus::Error(60)
        );

        assert_eq!(
            pinentry_selection(PinentryStatus::Error(GPG_ERR_NOT_CONFIRMED), 0, 1)
                .expect("deny selection"),
            Some(1)
        );
        assert_eq!(
            pinentry_selection(PinentryStatus::Error(GPG_ERR_CANCELED), 0, 1)
                .expect("cancel selection"),
            None
        );
        assert!(matches!(
            pinentry_selection(PinentryStatus::Error(60), 0, 1),
            Err(InteractionError::Unavailable(_))
        ));
    }
}
