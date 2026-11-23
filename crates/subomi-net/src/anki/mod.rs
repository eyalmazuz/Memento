//! Typed AnkiConnect client over HTTP/JSON.
//!
//! Reply shapes are validated via `subomi_anki::client` (`ReplyValidation`,
//! `TypedReplyStatus`, `validate_typed_result`, `check_ankiconnect_version`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use subomi_anki::client::{
    check_ankiconnect_version, validate_typed_result, ReplyValidation, TypedReplyKind,
    TypedReplyStatus, VersionCheckResult,
};

use crate::error::HttpError;
use crate::http::{HttpClient, HttpClientConfig, Request, Url};

/// Mirrors `AnkiClient`'s `m_manager.setTransferTimeout(TIMEOUT)` with
/// `TIMEOUT = 5000`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);

/// Raised transfer timeout for bulk media uploads (`storeMediaFile` via
/// `multi`) so large audio/video/image payloads do not hit the 5 s RPC default.
pub const MEDIA_UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Re-exported from the ported validation core (`ankiclient.cpp`'s
/// `MIN_ANKICONNECT_VERSION`).
pub use subomi_anki::client::MIN_ANKICONNECT_VERSION;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnkiAction
{
    Version,
    DeckNames,
    ModelNames,
    FieldNames,
    CanAddNotes,
    AddNote,
    UpdateNoteFields,
    AddTags,
    FindNotes,
    NotesInfo,
    GuiBrowse,
    StoreMediaFile,
    Multi,
}

impl AnkiAction
{
    pub const fn as_wire(self) -> &'static str
    {
        match self
        {
            Self::Version => "version",
            Self::DeckNames => "deckNames",
            Self::ModelNames => "modelNames",
            Self::FieldNames => "modelFieldNames",
            Self::CanAddNotes => "canAddNotes",
            Self::AddNote => "addNote",
            Self::UpdateNoteFields => "updateNoteFields",
            Self::AddTags => "addTags",
            Self::FindNotes => "findNotes",
            Self::NotesInfo => "notesInfo",
            Self::GuiBrowse => "guiBrowse",
            Self::StoreMediaFile => "storeMediaFile",
            Self::Multi => "multi",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnkiEndpoint
{
    /// Hostname (with or without scheme, per `AnkiConfig::load`).
    pub hostname: String,
    pub port: u16,
    /// Sent as `key` when `profiles[].useApiKey` is set.
    pub api_key: Option<String>,
    /// Socket read/write timeout; `ankiclient.cpp` uses 5000 ms.
    pub timeout: Option<Duration>,
    /// Same value as `hostname` captures: `profiles[].useApiKey`.
    pub use_api_key: bool,
}

impl Default for AnkiEndpoint
{
    fn default() -> Self
    {
        Self {
            hostname: "http://localhost".to_owned(),
            port: 8765,
            api_key: None,
            timeout: Some(DEFAULT_TIMEOUT),
            use_api_key: false,
        }
    }
}

impl AnkiEndpoint
{
    /// Returns the API key sent in AnkiConnect requests when `use_api_key` is
    /// enabled (`Some("")` when enabled with `api_key: None`), or `None` when
    /// API key authentication is disabled.
    pub fn api_key_sent(&self) -> Option<&str>
    {
        self.use_api_key.then(|| self.api_key.as_deref().unwrap_or(""))
    }
}

/// Builds a single sub-action JSON object for AnkiConnect's `"multi"` action.
///
/// Includes `"key": api_key` whenever `api_key` is `Some(...)` (Yomitan PR #2421)
/// so AnkiConnect servers with an API key configured accept every sub-action
/// dispatched inside `multi`.
pub fn build_multi_action(
    action: AnkiAction,
    params: Option<Value>,
    api_key: Option<&str>,
) -> Value
{
    let mut obj = serde_json::Map::with_capacity(3);
    obj.insert("action".to_owned(), Value::String(action.as_wire().to_owned()));
    if let Some(key) = api_key
    {
        obj.insert("key".to_owned(), Value::String(key.to_owned()));
    }
    if let Some(params) = params
    {
        obj.insert("params".to_owned(), params);
    }
    Value::Object(obj)
}

/// Ensures every sub-action object inside a `multi` request's `params.actions`
/// array includes `"key": api_key` (recursing into nested `multi` sub-actions).
fn inject_api_key_into_multi_params(params: &mut Value, api_key: &str)
{
    let Some(actions) = params
        .as_object_mut()
        .and_then(|obj| obj.get_mut("actions"))
        .and_then(Value::as_array_mut)
    else
    {
        return;
    };
    for sub_action in actions
    {
        if let Some(sub_obj) = sub_action.as_object_mut()
        {
            sub_obj
                .entry("key".to_owned())
                .or_insert_with(|| Value::String(api_key.to_owned()));
            let is_nested_multi = sub_obj
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|name| name == AnkiAction::Multi.as_wire());
            if is_nested_multi
            {
                if let Some(nested_params) = sub_obj.get_mut("params")
                {
                    inject_api_key_into_multi_params(nested_params, api_key);
                }
            }
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum AnkiError
{
    /// Socket/I-O failure (connection refused, timeout, reset, EOF).
    Network(std::io::Error),
    /// Invalid URL, request serialization, or HTTP protocol/redirect failure.
    Transport(String),
    /// A custom HTTP transport refused to provide TLS.
    Tls,
    /// HTTP response status rejected by `QNetworkReply::error()`.
    HttpStatus
    {
        status: u16,
        reason: String,
    },
    /// The reply was not a valid AnkiConnect frame (mirrors the
    /// `ReplyValidation` statuses of `AnkiClient::processReply`).
    Frame(ReplyValidation),
    /// The frame was valid but the `result` shape did not match the
    /// request kind (mirrors `TypedReplyStatus`).
    Typed(TypedReplyStatus),
    /// AnkiConnect reported a non-null `error` field.
    Remote(String),
    /// `testConnectionAsync`: the reported version is below
    /// `MIN_ANKICONNECT_VERSION`.
    VersionTooLow
    {
        version: i32,
        min: i32,
    },
    /// `addNote` returned zero or a value that is not a positive note ID.
    InvalidNoteId,
}

impl AnkiError
{
    /// Mirrors the `ConnectionRefusedError` classification of failed
    /// `SubomiNetReply` instances; C API HTTP-status failures use that
    /// same reply error even though the status remains available separately.
    pub fn is_network(&self) -> bool
    {
        matches!(self, Self::Network(_) | Self::HttpStatus { .. })
    }
}

impl std::fmt::Display for AnkiError
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        match self
        {
            Self::HttpStatus { status, reason } => write!(f, "HTTP error {status}: {reason}"),
            Self::Network(err) => write!(f, "network: {err}"),
            Self::Tls => write!(f, "endpoint requires TLS but none is configured"),
            Self::Transport(why) => write!(f, "transport: {why}"),
            Self::Remote(message) => write!(f, "AnkiConnect error: {message}"),
            Self::VersionTooLow { version, min } => {
                write!(f, "AnkiConnect version {version} < {min}")
            }
            Self::InvalidNoteId => write!(f, "AnkiConnect did not return a valid note ID"),
            Self::Frame(status) => match status
            {
                ReplyValidation::Ok => write!(f, "reply validated"),
                ReplyValidation::NotJson => write!(f, "Reply was not JSON"),
                ReplyValidation::NotObject => write!(f, "Reply was not an object"),
                ReplyValidation::UnexpectedFieldCount => {
                    write!(f, "Anki response has unexpected number of fields")
                }
                ReplyValidation::MissingErrorField => {
                    write!(f, "Anki response is missing error field")
                }
                ReplyValidation::MissingResultField => {
                    write!(f, "Anki response is missing result field")
                }
                ReplyValidation::HasErrorString => {
                    write!(f, "AnkiConnect reported an error")
                }
            },
            Self::Typed(status) => match status
            {
                TypedReplyStatus::Success => write!(f, "reply validated"),
                TypedReplyStatus::EmptySuccess => write!(f, "reply validated (empty success)"),
                TypedReplyStatus::ReplyError => write!(f, "AnkiConnect reported an error"),
                TypedReplyStatus::ResultNotArray => write!(f, "Result is not an array"),
                TypedReplyStatus::ElementTypeMismatch => {
                    write!(f, "Result is not an array of the expected element type")
                }
                TypedReplyStatus::EmptyError => write!(f, "AnkiConnect result was an empty value"),
                TypedReplyStatus::ResultNotDouble => {
                    write!(f, "AnkiConnect result is not a double")
                }
                TypedReplyStatus::InvalidKind => {
                    write!(f, "unknown result-shape validator")
                }
            },
        }
    }
}

impl std::error::Error for AnkiError
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)>
    {
        match self
        {
            Self::Network(err) => Some(err),
            _ => None,
        }
    }
}

pub type AnkiResult<T> = Result<T, AnkiError>;

/// Serialized request before it hits the socket; also what tests assert
/// byte-shape against (via `serde_json::Value` equality).
#[derive(Clone, Debug, PartialEq)]
pub struct AnkiRequest
{
    pub action: String,
    pub version: i32,
    pub key: Option<String>,
    pub params: Option<Value>,
}

impl AnkiRequest
{
    pub fn new(action: impl Into<String>) -> Self
    {
        Self {
            action: action.into(),
            version: MIN_ANKICONNECT_VERSION,
            key: None,
            params: None,
        }
    }

    /// Builds the JSON body exactly the way `AnkiClient::makeRequest` does:
    /// `action`, then `version: 6`, then an optional `key`, then an
    /// optional `params` object, moving owned fields without cloning `params`.
    pub fn to_json(self) -> Value
    {
        let mut message = serde_json::Map::with_capacity(4);
        message.insert("action".to_owned(), Value::String(self.action));
        message.insert("version".to_owned(), Value::from(self.version));
        if let Some(key) = self.key
        {
            message.insert("key".to_owned(), Value::String(key));
        }
        if let Some(params) = self.params
        {
            message.insert("params".to_owned(), params);
        }
        Value::Object(message)
    }

    /// Serializes the request directly to JSON bytes by borrowing its fields
    /// without cloning `params`.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error>
    {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(br#"{"action":"#);
        serde_json::to_writer(&mut buf, &self.action)?;
        buf.extend_from_slice(br#","version":"#);
        serde_json::to_writer(&mut buf, &self.version)?;
        if let Some(key) = &self.key
        {
            buf.extend_from_slice(br#","key":"#);
            serde_json::to_writer(&mut buf, key)?;
        }
        if let Some(params) = &self.params
        {
            buf.extend_from_slice(br#","params":"#);
            serde_json::to_writer(&mut buf, params)?;
        }
        buf.push(b'}');
        Ok(buf)
    }
}

pub struct AnkiClient
{
    endpoint: AnkiEndpoint,
    http: HttpClient,
}

fn shared_anki_http_client(timeout: Option<Duration>) -> HttpClient
{
    static POOL: OnceLock<Mutex<Vec<(Option<Duration>, HttpClient)>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, client)) = guard.iter().find(|(t, _)| *t == timeout)
    {
        return client.clone();
    }
    let config = HttpClientConfig {
        timeout,
        connect_timeout: timeout,
        ..HttpClientConfig::default()
    };
    let client = HttpClient::new(config);
    if guard.len() >= 8
    {
        guard.remove(0);
    }
    guard.push((timeout, client.clone()));
    client
}

impl AnkiClient
{
    /// Endpoint with the ported `ankiclient.cpp` semantics: 5000 ms
    /// transfer timeout, transport kept on tap through settings.
    pub fn new(endpoint: AnkiEndpoint) -> Self
    {
        let http = shared_anki_http_client(endpoint.timeout);
        Self::with_transport(endpoint, http)
    }

    /// Plug in a shared/custom `HttpClient` when a profile needs a different
    /// connection policy. The endpoint's timeout is advisory: the supplied
    /// client's configuration is used.
    pub fn with_transport(endpoint: AnkiEndpoint, http: HttpClient) -> Self
    {
        Self { endpoint, http }
    }

    pub fn endpoint(&self) -> &AnkiEndpoint
    {
        &self.endpoint
    }

    /// Returns the API key sent on requests when `use_api_key` is enabled, or `None`.
    pub fn api_key(&self) -> Option<&str>
    {
        self.endpoint.api_key_sent()
    }

    /// Builds a sub-action JSON object for AnkiConnect `"multi"`, automatically
    /// including `"key"` when this client's endpoint uses an API key.
    pub fn build_multi_action(&self, action: AnkiAction, params: Option<Value>) -> Value
    {
        build_multi_action(action, params, self.endpoint.api_key_sent())
    }

    /// Sends an AnkiConnect `"multi"` request containing `actions`, ensuring
    /// every sub-action includes `"key"` when `use_api_key` is enabled.
    pub fn multi(&self, actions: Vec<Value>) -> AnkiResult<Value>
    {
        let params = json!({ "actions": actions });
        self.request(AnkiAction::Multi, Some(params))
    }

    /// Composes the AnkiConnect endpoint URL from `hostname` and `port`,
    /// wrapping bare IPv6 literals in `[...]`, preserving explicit schemes,
    /// explicit ports in `hostname`, and paths/queries cleanly.
    pub fn endpoint_url(&self) -> String
    {
        compose_endpoint_url(&self.endpoint.hostname, self.endpoint.port)
    }

    fn endpoint_request(&self, action: AnkiAction, mut params: Option<Value>) -> AnkiResult<Request>
    {
        let api_key = self.endpoint.api_key_sent();
        if action == AnkiAction::Multi
        {
            if let (Some(key), Some(params_val)) = (api_key, params.as_mut())
            {
                inject_api_key_into_multi_params(params_val, key);
            }
        }
        let request = AnkiRequest {
            action: action.as_wire().to_owned(),
            version: MIN_ANKICONNECT_VERSION,
            key: api_key.map(str::to_owned),
            params,
        };
        // `Url::parse` canonicalizes a pathless URL to `/`, while preserving
        // any path already present in the profile URL as Qt does.
        let url = Url::parse(&self.endpoint_url()).map_err(http_transport_err)?;
        let body = request
            .to_json_bytes()
            .map_err(|err| AnkiError::Transport(err.to_string()))?;
        Ok(
            Request::post(url, body).header("Content-Type", "application/json"),
        )
    }

    /// One `makeRequest(processReply)` cycle: sends the action, validates
    /// the frame directly on `serde_json::Value`, and returns the decoded
    /// `result` value.
    pub fn request(
        &self,
        action: AnkiAction,
        params: Option<Value>,
    ) -> AnkiResult<Value>
    {
        self.request_with_timeout(action, params, None)
    }

    fn request_with_timeout(
        &self,
        action: AnkiAction,
        params: Option<Value>,
        timeout_override: Option<Option<Duration>>,
    ) -> AnkiResult<Value>
    {
        let request = self.endpoint_request(action, params)?;
        let mut response = self
            .http
            .send_with_timeout(&request, timeout_override)
            .map_err(anki_http_err)?;
        if !(200..300).contains(&response.status())
        {
            return Err(AnkiError::HttpStatus {
                status: response.status(),
                reason: response.reason().to_owned(),
            });
        }
        let bytes = response
            .read_all(MAX_REPLY_BYTES)
            .map_err(anki_body_err)?;
        let frame: Value =
            serde_json::from_slice(&bytes).map_err(anki_json_err)?;
        validate_reply_frame(frame)
    }

    /// `version` as a strictly numeric result (the `requestInt` shape).
    pub fn version(&self) -> AnkiResult<i32>
    {
        let result = self.request(AnkiAction::Version, None)?;
        let status = typed_status(TypedReplyKind::RequestInt, &result);
        match status
        {
            TypedReplyStatus::Success => {
                Ok(json_to_i32(&result))
            }
            other => Err(AnkiError::Typed(other)),
        }
    }

    /// `testConnectionAsync`: version + min-version gate
    /// (`check_ankiconnect_version` reuse).
    pub fn test_connection(&self) -> AnkiResult<()>
    {
        let version = self.version()?;
        match check_ankiconnect_version(true, version)
        {
            VersionCheckResult::Ok => Ok(()),
            VersionCheckResult::NotANumber => {
                Err(AnkiError::Typed(TypedReplyStatus::ResultNotDouble))
            }
            VersionCheckResult::VersionTooLow { min_version } => {
                Err(AnkiError::VersionTooLow {
                    version,
                    min: min_version,
                })
            }
        }
    }

    pub fn deck_names(&self) -> AnkiResult<Vec<String>>
    {
        let result =
            self.request_typed(TypedReplyKind::RequestStringList, AnkiAction::DeckNames, None)?;
        as_string_list(&result)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))
    }

    pub fn model_names(&self) -> AnkiResult<Vec<String>>
    {
        let result =
            self.request_typed(TypedReplyKind::RequestStringList, AnkiAction::ModelNames, None)?;
        as_string_list(&result)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))
    }

    /// `modelFieldNames` with `{ "modelName": model }` params.
    pub fn field_names(&self, model: &str) -> AnkiResult<Vec<String>>
    {
        let params = json!({ "modelName": model });
        let result = self.request_typed(
            TypedReplyKind::RequestStringList,
            AnkiAction::FieldNames,
            Some(params),
        )?;
        as_string_list(&result)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))
    }

    /// Calls AnkiConnect `modelFieldNames` with `{ "modelName": model_name }`.
    pub fn model_field_names(&self, model_name: &str) -> AnkiResult<Vec<String>>
    {
        self.field_names(model_name)
    }

    /// `canAddNotes` with `{ "notes": [...] }` params (reuses
    /// `interpret_can_add_notes`-adjacent bool list shape).
    pub fn can_add_notes(&self, notes: &[Value]) -> AnkiResult<Vec<bool>>
    {
        let params = json!({ "notes": notes });
        let result = self.request_typed(
            TypedReplyKind::RequestBoolList,
            AnkiAction::CanAddNotes,
            Some(params),
        )?;
        as_bool_list(&result)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))
    }

    /// `addNote` with `{ "note": ... }`, returning its positive 64-bit note ID.
    pub fn add_note(&self, note: Value) -> AnkiResult<i64>
    {
        let params = json!({ "note": note });
        let result = self.request_typed(
            TypedReplyKind::RequestInt,
            AnkiAction::AddNote,
            Some(params),
        )?;
        as_note_id(&result).ok_or(AnkiError::InvalidNoteId)
    }

    /// `updateNoteFields` with `{ "note": { "id": note_id, "fields": ..., ... } }`.
    /// Conforms to AnkiConnect v6 by omitting `audio`, `picture`, and `video`
    /// when `None` or empty.
    pub fn update_note_fields(
        &self,
        note_id: i64,
        fields: serde_json::Map<String, Value>,
        audio: Option<&[Value]>,
        picture: Option<&[Value]>,
        video: Option<&[Value]>,
    ) -> AnkiResult<()>
    {
        if note_id <= 0
        {
            return Err(AnkiError::InvalidNoteId);
        }
        let mut note = serde_json::Map::new();
        note.insert("id".to_owned(), Value::from(note_id));
        note.insert("fields".to_owned(), Value::Object(fields));
        if let Some(items) = audio.filter(|items| !items.is_empty())
        {
            note.insert("audio".to_owned(), Value::Array(items.to_vec()));
        }
        if let Some(items) = picture.filter(|items| !items.is_empty())
        {
            note.insert("picture".to_owned(), Value::Array(items.to_vec()));
        }
        if let Some(items) = video.filter(|items| !items.is_empty())
        {
            note.insert("video".to_owned(), Value::Array(items.to_vec()));
        }
        let params = json!({ "note": Value::Object(note) });
        let _ = self.request(AnkiAction::UpdateNoteFields, Some(params))?;
        Ok(())
    }

    /// `addTags` with `{ "notes": [...], "tags": "..." }`.
    /// Returns `Ok(())` without sending a request when `notes` is empty or `tags` is blank.
    pub fn add_tags(&self, notes: &[i64], tags: &str) -> AnkiResult<()>
    {
        if notes.iter().any(|&id| id <= 0)
        {
            return Err(AnkiError::InvalidNoteId);
        }
        let trimmed = tags.trim();
        if notes.is_empty() || trimmed.is_empty()
        {
            return Ok(());
        }
        let params = json!({
            "notes": notes,
            "tags": trimmed,
        });
        let _ = self.request(AnkiAction::AddTags, Some(params))?;
        Ok(())
    }

    /// `storeMediaFile` inside `multi` (the `addMedia` flow): the caller
    /// supplies pre-base64-encoded payloads (encoding is
    /// `subomi-util`/`FileUtils::toBase64` business).
    /// Short-circuits without touching the network when `files` is empty, and
    /// raises the transfer timeout to at least [`MEDIA_UPLOAD_TIMEOUT`].
    pub fn store_media_files(&self, files: &[MediaFilePayload]) -> AnkiResult<Vec<String>>
    {
        if files.is_empty()
        {
            return Ok(Vec::new());
        }
        let actions: Vec<Value> = files
            .iter()
            .map(|file| {
                self.build_multi_action(
                    AnkiAction::StoreMediaFile,
                    Some(json!({
                        "data": file.data_base64,
                        "filename": file.filename,
                    })),
                )
            })
            .collect();
        let params = json!({ "actions": actions });
        let media_timeout = self
            .http
            .config()
            .timeout
            .map(|configured| configured.max(MEDIA_UPLOAD_TIMEOUT));
        let result = self.request_typed_with_timeout(
            TypedReplyKind::AddMedia,
            AnkiAction::Multi,
            Some(params),
            Some(media_timeout),
        )?;
        as_string_list(&result)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))
    }

    /// `findNotes` with `{ "query": query }`, returning positive 64-bit note IDs.
    pub fn find_notes(&self, query: &str) -> AnkiResult<Vec<i64>>
    {
        let params = json!({ "query": query });
        let result = self.request_typed(
            TypedReplyKind::OpenBrowse,
            AnkiAction::FindNotes,
            Some(params),
        )?;
        let values = result
            .as_array()
            .ok_or(AnkiError::Typed(TypedReplyStatus::ResultNotArray))?;
        let mut ids = Vec::with_capacity(values.len());
        for value in values
        {
            let id = as_note_id(value)
                .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// `notesInfo` with `{ "notes": [...] }`, decoding structured note metadata.
    pub fn notes_info(&self, notes: &[i64]) -> AnkiResult<Vec<AnkiNoteInfo>>
    {
        let params = json!({ "notes": notes });
        let result = self.request(AnkiAction::NotesInfo, Some(params))?;
        parse_notes_info_result(&result)
    }

    /// `guiBrowse` with `{ "query": query }`, returning note ids
    /// (`openBrowseAsync` shape).
    pub fn open_browse(&self, query: &str) -> AnkiResult<Vec<i64>>
    {
        let params = json!({ "query": query });
        let result = self.request_typed(
            TypedReplyKind::OpenBrowse,
            AnkiAction::GuiBrowse,
            Some(params),
        )?;
        result
            .as_array()
            .ok_or(AnkiError::Typed(TypedReplyStatus::ResultNotArray))
            .map(|values| {
                values
                    .iter()
                    .map(json_to_i64)
                    .collect()
            })
    }

    /// Typed-request wrapper: validates frame + result shape for the kind,
    /// then hands over the raw `result`.
    fn request_typed(
        &self,
        kind: TypedReplyKind,
        action: AnkiAction,
        params: Option<Value>,
    ) -> AnkiResult<Value>
    {
        self.request_typed_with_timeout(kind, action, params, None)
    }

    fn request_typed_with_timeout(
        &self,
        kind: TypedReplyKind,
        action: AnkiAction,
        params: Option<Value>,
        timeout_override: Option<Option<Duration>>,
    ) -> AnkiResult<Value>
    {
        let result = self.request_with_timeout(action, params, timeout_override)?;
        let status = typed_status(kind, &result);
        match status
        {
            TypedReplyStatus::Success | TypedReplyStatus::EmptySuccess => Ok(result),
            other => Err(AnkiError::Typed(other)),
        }
    }
}

fn validate_reply_frame(frame: Value) -> AnkiResult<Value>
{
    match frame
    {
        Value::Null => Err(AnkiError::Frame(ReplyValidation::NotJson)),
        Value::Object(mut obj) => {
            if obj.len() != 2
            {
                return Err(AnkiError::Frame(ReplyValidation::UnexpectedFieldCount));
            }
            let Some(error_val) = obj.get("error")
            else
            {
                return Err(AnkiError::Frame(ReplyValidation::MissingErrorField));
            };
            if !obj.contains_key("result")
            {
                return Err(AnkiError::Frame(ReplyValidation::MissingResultField));
            }
            if !error_val.is_null()
            {
                return Err(AnkiError::Remote(json_str_or_empty(Some(error_val))));
            }
            Ok(obj.remove("result").unwrap_or(Value::Null))
        }
        _ => Err(AnkiError::Frame(ReplyValidation::NotObject)),
    }
}

fn compose_endpoint_url(hostname: &str, port: u16) -> String
{
    let raw = hostname.trim();
    if raw.is_empty()
    {
        return format!("http://:{port}");
    }

    let (scheme, after_scheme) = match raw.find("://")
    {
        Some(idx) if !raw[..idx].contains(['/', '?', '#', '[', ']']) => {
            (&raw[..idx], &raw[idx + 3..])
        }
        _ => ("http", raw),
    };

    let authority_end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    let (authority, rest) = after_scheme.split_at(authority_end);

    let (normalized_authority, has_explicit_port) = if authority.starts_with('[')
    {
        if let Some(close) = authority.rfind(']')
        {
            let tail = &authority[close + 1..];
            if let Some(port_part) = tail.strip_prefix(':')
            {
                if port_part.is_empty()
                {
                    (authority[..=close].to_owned(), false)
                }
                else
                {
                    (authority.to_owned(), true)
                }
            }
            else
            {
                (authority.to_owned(), false)
            }
        }
        else
        {
            (authority.to_owned(), false)
        }
    }
    else if authority.parse::<std::net::Ipv6Addr>().is_ok()
    {
        (format!("[{authority}]"), false)
    }
    else if let Some((host_part, port_part)) = authority.rsplit_once(':')
    {
        if !host_part.contains(':') && port_part.is_empty()
        {
            (host_part.to_owned(), false)
        }
        else if !host_part.contains(':')
            && port_part.bytes().all(|b| b.is_ascii_digit())
        {
            (authority.to_owned(), true)
        }
        else
        {
            (authority.to_owned(), false)
        }
    }
    else
    {
        (authority.to_owned(), false)
    };

    if has_explicit_port
    {
        format!("{scheme}://{normalized_authority}{rest}")
    }
    else
    {
        format!("{scheme}://{normalized_authority}:{port}{rest}")
    }
}

/// Structured note metadata returned by AnkiConnect `notesInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnkiNoteInfo
{
    pub note_id: i64,
    pub model_name: String,
    pub tags: Vec<String>,
    pub fields: HashMap<String, String>,
    /// Name of the note model's first/sort field (minimum `"order"` in `notesInfo`), if reported.
    pub sort_field: Option<String>,
}

fn parse_notes_info_result(result: &Value) -> AnkiResult<Vec<AnkiNoteInfo>>
{
    let entries = result
        .as_array()
        .ok_or(AnkiError::Typed(TypedReplyStatus::ResultNotArray))?;
    let mut infos = Vec::with_capacity(entries.len());
    for entry in entries
    {
        let obj = entry
            .as_object()
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?;
        if obj.is_empty()
        {
            // AnkiConnect returns `{}` for missing/deleted note IDs.
            continue;
        }
        let note_id = obj
            .get("noteId")
            .and_then(as_note_id)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?;
        let model_name = obj
            .get("modelName")
            .and_then(Value::as_str)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?
            .to_owned();
        let tags_array = obj
            .get("tags")
            .and_then(Value::as_array)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?;
        let mut tags = Vec::with_capacity(tags_array.len());
        for tag in tags_array
        {
            let tag_str = tag
                .as_str()
                .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?;
            tags.push(tag_str.to_owned());
        }
        let fields_obj = obj
            .get("fields")
            .and_then(Value::as_object)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?;
        let mut fields = HashMap::with_capacity(fields_obj.len());
        let mut min_order: Option<(i64, String)> = None;
        for (name, field_val) in fields_obj
        {
            let value_str = if let Some(s) = field_val.as_str()
            {
                s
            }
            else if let Some(field_obj) = field_val.as_object()
            {
                if let Some(order) = field_obj.get("order").and_then(Value::as_i64)
                {
                    if min_order.as_ref().is_none_or(|(best, _)| order < *best)
                    {
                        min_order = Some((order, name.clone()));
                    }
                }
                field_obj
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))?
            }
            else
            {
                return Err(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch));
            };
            fields.insert(name.clone(), value_str.to_owned());
        }
        infos.push(AnkiNoteInfo {
            note_id,
            model_name,
            tags,
            fields,
            sort_field: min_order.map(|(_, name)| name),
        });
    }
    Ok(infos)
}

/// Pre-encoded media file for the `multi`/`storeMediaFile` flow. The
/// base64 encoding is done by the caller (`FileUtils::toBase64` lives with
/// the `subomi-util` file helpers); this crate stays byte-stream based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFilePayload
{
    pub filename: String,
    pub data_base64: String,
}

/// Largest accepted reply body: AnkiConnect `multi` media replies with
/// base64 blobs stay well under this.
const MAX_REPLY_BYTES: u64 = 32 * 1024 * 1024;

fn http_transport_err(err: HttpError) -> AnkiError
{
    match err
    {
        HttpError::Url(why) => AnkiError::Transport(format!("url: {why}")),
        _ => AnkiError::Transport(err.to_string()),
    }
}

fn anki_http_err(err: HttpError) -> AnkiError
{
    match err
    {
        HttpError::Url(why) | HttpError::Protocol(why) => {
            AnkiError::Transport(why)
        }
        HttpError::TlsUnsupported => AnkiError::Tls,
        HttpError::TooManyRedirects { .. } | HttpError::RedirectLoop => {
            AnkiError::Transport(err.to_string())
        }
        HttpError::Io(io) => AnkiError::Network(io),
    }
}

fn anki_body_err(err: std::io::Error) -> AnkiError
{
    AnkiError::Network(err)
}

fn anki_json_err(_err: serde_json::Error) -> AnkiError
{
    AnkiError::Frame(ReplyValidation::NotJson)
}

/// Extracts string values cleanly, falling back to an empty string for non-string JSON values.
fn json_str_or_empty(value: Option<&Value>) -> String
{
    match value
    {
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// Computes the typed result-shape status out of the ported validator.
pub fn typed_status(kind: TypedReplyKind, result: &Value) -> TypedReplyStatus
{
    let (result_is_array, result_is_double, elements_match) =
        match kind
        {
            TypedReplyKind::AddMedia | TypedReplyKind::RequestStringList => {
                let elements = result.as_array();
                let is_array = elements.is_some();
                let elements_match = elements
                    .is_some_and(|items| items.iter().all(|element| element.is_string()));
                (is_array, false, elements_match)
            }
            TypedReplyKind::OpenBrowse => {
                let elements = result.as_array();
                let is_array = elements.is_some();
                let elements_match = elements
                    .is_some_and(|items| items.iter().all(|element| element.is_number()));
                (is_array, false, elements_match)
            }
            TypedReplyKind::RequestBoolList => {
                let elements = result.as_array();
                let is_array = elements.is_some();
                let elements_match = elements
                    .is_some_and(|items| items.iter().all(Value::is_boolean));
                (is_array, false, elements_match)
            }
            TypedReplyKind::RequestInt => {
                (false, result.is_number(), true)
            }
        };
    validate_typed_result(
        kind,
        false,   // reply errors were already handled by `processReply`
        false,   // a valid frame always has both fields; empty means error above
        result_is_array,
        result_is_double,
        elements_match,
    )
}

fn as_string_list(result: &Value) -> Option<Vec<String>>
{
    result.as_array().map(|values| {
        values
            .iter()
            .map(|value| match value
            {
                Value::String(text) => text.clone(),
                // requestStringList's known C++ behavior: non-strings pass
                // through `toString()` (empty), mismatches are tolerated.
                _ => String::new(),
            })
            .collect()
    })
}

fn as_bool_list(result: &Value) -> Option<Vec<bool>>
{
    result.as_array().map(|values| {
        values
            .iter()
            .map(|value| match value
            {
                Value::Bool(flag) => *flag,
                _ => false,
            })
            .collect()
    })
}


/// Converts a JSON number to `i32` when it is an exact integer in `i32::MIN..=i32::MAX`, else `0`.
fn json_to_i32(value: &Value) -> i32
{
    if let Some(integer) = value.as_i64()
    {
        return i32::try_from(integer).unwrap_or(0);
    }
    if let Some(integer) = value.as_u64()
    {
        return i32::try_from(integer).unwrap_or(0);
    }
    let Some(number) = value.as_f64() else { return 0 };
    if number.is_finite()
        && number.fract() == 0.0
        && number >= i32::MIN as f64
        && number <= i32::MAX as f64
    {
        number as i32
    }
    else
    {
        0
    }
}

fn json_to_i64(value: &Value) -> i64
{
    if let Some(integer) = value.as_i64()
    {
        return integer;
    }
    if let Some(integer) = value.as_u64()
    {
        return i64::try_from(integer).unwrap_or(0);
    }
    let Some(number) = value.as_f64() else { return 0 };
    if number.is_finite()
        && number.fract() == 0.0
        && (-(1_i64 << 53) as f64..=(1_i64 << 53) as f64).contains(&number)
    {
        number as i64
    }
    else
    {
        0
    }
}

fn as_note_id(value: &Value) -> Option<i64>
{
    if let Some(id) = value.as_i64() {
        return (id > 0).then_some(id);
    }
    if let Some(id) = value.as_u64() {
        return i64::try_from(id).ok().filter(|id| *id > 0);
    }
    let number = value.as_f64()?;
    // JSON floating point integers are exact only through 2^53.
    if number.is_finite() && number.fract() == 0.0
        && (1.0..=(1_u64 << 53) as f64).contains(&number) {
        return Some(number as i64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{as_note_id, json_to_i32, AnkiEndpoint, DEFAULT_TIMEOUT};
    use serde_json::json;

    #[test]
    fn json_to_i32_requires_a_whole_i32_value()
    {
        assert_eq!(json_to_i32(&json!(123)), 123);
        assert_eq!(json_to_i32(&json!(123.0)), 123);
        assert_eq!(json_to_i32(&json!(123.5)), 0);
        assert_eq!(json_to_i32(&json!(i32::MAX as i64 + 1)), 0);
        assert_eq!(json_to_i32(&json!(i32::MIN as i64 - 1)), 0);
    }

    #[test]
    fn note_ids_keep_64_bit_values_and_reject_zero()
    {
        assert_eq!(as_note_id(&json!(1669909161234_i64)), Some(1669909161234));
        assert_eq!(as_note_id(&json!(1669909161234_f64)), Some(1669909161234));
        assert_eq!(as_note_id(&json!(0)), None);
        assert_eq!(as_note_id(&json!(-1)), None);
        assert_eq!(as_note_id(&json!(1.5)), None);
    }


    #[test]
    fn default_endpoint_matches_config_defaults() {
        let endpoint = AnkiEndpoint::default();
        assert_eq!(endpoint.hostname, "http://localhost");
        assert_eq!(endpoint.port, 8765);
        assert_eq!(endpoint.timeout, Some(DEFAULT_TIMEOUT));
    }
    #[test]
    fn enabled_api_key_is_sent_even_when_empty() {
        let endpoint = AnkiEndpoint {
            use_api_key: true,
            api_key: None,
            ..AnkiEndpoint::default()
        };
        assert_eq!(endpoint.api_key_sent(), Some(""));
    }

    #[test]
    fn endpoint_url_handles_ipv6_explicit_port_scheme_and_path()
    {
        use super::AnkiClient;

        let make = |hostname: &str, port: u16| {
            AnkiClient::new(AnkiEndpoint {
                hostname: hostname.to_owned(),
                port,
                ..AnkiEndpoint::default()
            })
            .endpoint_url()
        };

        assert_eq!(make("http://localhost", 8765), "http://localhost:8765");
        assert_eq!(make("localhost", 8765), "http://localhost:8765");
        assert_eq!(make("::1", 8765), "http://[::1]:8765");
        assert_eq!(make("[::1]", 8765), "http://[::1]:8765");
        assert_eq!(make("http://::1", 8765), "http://[::1]:8765");
        assert_eq!(make("http://::1/anki", 8765), "http://[::1]:8765/anki");
        assert_eq!(make("http://[::1]/anki", 8765), "http://[::1]:8765/anki");
        assert_eq!(make("http://[::1]:9000/anki", 8765), "http://[::1]:9000/anki");
        assert_eq!(make("https://anki.example.com/api", 8765), "https://anki.example.com:8765/api");
        assert_eq!(make("http://localhost:9000/api?k=1", 8765), "http://localhost:9000/api?k=1");
        assert_eq!(make("localhost:9000", 8765), "http://localhost:9000");
        assert_eq!(make("http://localhost/", 8765), "http://localhost:8765/");
        assert_eq!(make("http://localhost:", 8765), "http://localhost:8765");
        assert_eq!(make("http://[::1]:", 8765), "http://[::1]:8765");
    }

    #[test]
    fn anki_request_to_json_and_bytes_match()
    {
        use super::AnkiRequest;
        let req = AnkiRequest {
            action: "addNote".to_owned(),
            version: 6,
            key: Some("secret".to_owned()),
            params: Some(json!({ "note": { "deckName": "Default" } })),
        };
        let bytes = req.to_json_bytes().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let owned = req.to_json();
        assert_eq!(parsed, owned);
    }

    #[test]
    fn empty_store_media_files_short_circuits_without_network()
    {
        use super::AnkiClient;
        // Unreachable port 1: proves no HTTP request is issued.
        let client = AnkiClient::new(AnkiEndpoint {
            hostname: "http://127.0.0.1".to_owned(),
            port: 1,
            ..AnkiEndpoint::default()
        });
        assert_eq!(client.store_media_files(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn build_multi_action_with_and_without_api_key()
    {
        use super::{build_multi_action, AnkiAction};

        let without_key = build_multi_action(
            AnkiAction::FindNotes,
            Some(json!({ "query": "deck:\"Default\"" })),
            None,
        );
        assert_eq!(
            without_key,
            json!({
                "action": "findNotes",
                "params": { "query": "deck:\"Default\"" }
            })
        );
        assert!(without_key.get("key").is_none());

        let with_key = build_multi_action(
            AnkiAction::FindNotes,
            Some(json!({ "query": "deck:\"Default\"" })),
            Some("my-secret-key"),
        );
        assert_eq!(
            with_key,
            json!({
                "action": "findNotes",
                "key": "my-secret-key",
                "params": { "query": "deck:\"Default\"" }
            })
        );

        let bare_with_empty_key = build_multi_action(AnkiAction::DeckNames, None, Some(""));
        assert_eq!(
            bare_with_empty_key,
            json!({
                "action": "deckNames",
                "key": ""
            })
        );
    }

    #[test]
    fn endpoint_request_injects_api_key_into_all_multi_sub_actions_including_nested()
    {
        use super::{AnkiAction, AnkiClient};

        let client_with_key = AnkiClient::new(AnkiEndpoint {
            api_key: Some("tok-123".to_owned()),
            use_api_key: true,
            ..AnkiEndpoint::default()
        });
        assert_eq!(client_with_key.api_key(), Some("tok-123"));

        let req = client_with_key
            .endpoint_request(
                AnkiAction::Multi,
                Some(json!({
                    "actions": [
                        { "action": "findNotes", "params": { "query": "a" } },
                        {
                            "action": "multi",
                            "params": {
                                "actions": [
                                    { "action": "deckNames" }
                                ]
                            }
                        }
                    ]
                })),
            )
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({
                "action": "multi",
                "version": 6,
                "key": "tok-123",
                "params": {
                    "actions": [
                        { "action": "findNotes", "key": "tok-123", "params": { "query": "a" } },
                        {
                            "action": "multi",
                            "key": "tok-123",
                            "params": {
                                "actions": [
                                    { "action": "deckNames", "key": "tok-123" }
                                ]
                            }
                        }
                    ]
                }
            })
        );

        let client_without_key = AnkiClient::new(AnkiEndpoint {
            api_key: Some("ignored-when-disabled".to_owned()),
            use_api_key: false,
            ..AnkiEndpoint::default()
        });
        assert_eq!(client_without_key.api_key(), None);
        let req_no_key = client_without_key
            .endpoint_request(
                AnkiAction::Multi,
                Some(json!({
                    "actions": [
                        client_without_key.build_multi_action(
                            AnkiAction::FindNotes,
                            Some(json!({ "query": "a" })),
                        )
                    ]
                })),
            )
            .unwrap();
        let body_no_key: serde_json::Value =
            serde_json::from_slice(req_no_key.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body_no_key,
            json!({
                "action": "multi",
                "version": 6,
                "params": {
                    "actions": [
                        { "action": "findNotes", "params": { "query": "a" } }
                    ]
                }
            })
        );
    }

}
