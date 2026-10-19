////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2022, 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Typed AnkiConnect client, replacing the `QNetworkAccessManager` half of
//! `AnkiClient` (`makeRequest` / `processReply` in `src/anki/ankiclient.cpp`).
//!
//! Reply shapes are validated by reusing the already-ported core in
//! `rust/memento-anki/src/client.rs`:
//!
//! * `validate_reply_json` — the `{"error": ..., "result": ...}`
//!   twenty-two checks (field count 2, error field present, error null, …)
//!   behind `AnkiClient::processReply`;
//! * `validate_typed_result` — the per-request-kind result-shape checks
//!   behind `requestStringList`, `requestBoolList`, `requestInt`,
//!   `addMedia`, and `openBrowseAsync`;
//! * `check_ankiconnect_version` — the `testConnectionAsync` gate.
//! * `normalize_hostname` / `format_endpoint_url` — the URL composition.
//!
//! The verbs are the ones `ankiclient.cpp` actually issues (mirroring the
//! constants in `ankiconnect.h`): `version`, `deckNames`, `modelNames`,
//! `modelFieldNames`, `canAddNotes`, `addNote`, `guiBrowse`,
//! `storeMediaFile` (via `multi`).

use std::time::Duration;

use serde_json::{json, Value};

use memento_anki::client::{
    check_ankiconnect_version, format_endpoint_url, normalize_hostname,
    validate_reply_json, validate_typed_result, ReplyValidation, TypedReplyKind,
    TypedReplyStatus, VersionCheckResult,
};

use crate::error::HttpError;
use crate::http::{HttpClient, HttpClientConfig, Request, Url};

/// Mirrors `AnkiClient`'s `m_manager.setTransferTimeout(TIMEOUT)` with
/// `TIMEOUT = 5000`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);

/// Re-exported from the ported validation core (`ankiclient.cpp`'s
/// `MIN_ANKICONNECT_VERSION`).
pub use memento_anki::client::MIN_ANKICONNECT_VERSION;


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
    fn api_key_sent(&self) -> Option<&str>
    {
        self.use_api_key.then(|| self.api_key.as_deref().unwrap_or(""))
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
}

impl AnkiError
{
    /// Mirrors the `ConnectionRefusedError` classification of failed
    /// `MementoNetReply` instances; C API HTTP-status failures use that
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
    /// optional `params` object.
    pub fn to_json(&self) -> Value
    {
        let mut message = json!({
            "action": self.action,
            "version": self.version,
        });
        if let Some(key) = &self.key
        {
            message["key"] = json!(key);
        }
        if let Some(params) = &self.params
        {
            message["params"] = params.clone();
        }
        message
    }
}

pub struct AnkiClient
{
    endpoint: AnkiEndpoint,
    http: HttpClient,
}

impl AnkiClient
{
    /// Endpoint with the ported `ankiclient.cpp` semantics: 5000 ms
    /// transfer timeout, transport kept on tap through settings.
    pub fn new(endpoint: AnkiEndpoint) -> Self
    {
        let config = HttpClientConfig {
            timeout: endpoint.timeout,
            ..HttpClientConfig::default()
        };
        Self::with_transport(endpoint, HttpClient::new(config))
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

    /// `hostname:port` composition through the ported
    /// `normalize_hostname` + `format_endpoint_url` (`makeRequest`
    /// URL formation).
    pub fn endpoint_url(&self) -> String
    {
        let normalized = normalize_hostname(&u16_of(&self.endpoint.hostname));
        let port = self.endpoint.port.to_string();
        String::from_utf16_lossy(&format_endpoint_url(&normalized, &u16_of(&port)))
    }

    fn endpoint_request(&self, action: AnkiAction, params: Option<Value>) -> AnkiResult<Request>
    {
        let request = AnkiRequest {
            action: action.as_wire().to_owned(),
            version: MIN_ANKICONNECT_VERSION,
            key: self.endpoint.api_key_sent().map(str::to_owned),
            params,
        };
        // `Url::parse` canonicalizes a pathless URL to `/`, while preserving
        // any path already present in the profile URL as Qt does.
        let url = Url::parse(&self.endpoint_url()).map_err(http_transport_err)?;
        let body = serde_json::to_vec(&request.to_json())
            .map_err(|err| AnkiError::Transport(err.to_string()))?;
        Ok(
            Request::post(url, body).header("Content-Type", "application/json"),
        )
    }

    /// One `makeRequest(processReply)` cycle: sends the action, validates
    /// the frame with the ported validation, and returns the decoded
    /// `result` value.
    pub fn request(
        &self,
        action: AnkiAction,
        params: Option<Value>,
    ) -> AnkiResult<Value>
    {
        let request = self.endpoint_request(action, params)?;
        let response = self.http.send(&request).map_err(anki_http_err)?;
        if response.status() >= 400
        {
            return Err(AnkiError::HttpStatus {
                status: response.status(),
                reason: response.reason().to_owned(),
            });
        }
        let mut response = response;
        let bytes = response
            .read_all(MAX_REPLY_BYTES)
            .map_err(anki_body_err)?;
        let frame: Value =
            serde_json::from_slice(&bytes).map_err(anki_json_err)?;

        // Reuse the ported reply-shape validator over the UTF-16-shaped
        // value it was authored against.
        let qjson = to_qjson(&frame);
        match validate_reply_json(&qjson)
        {
            ReplyValidation::Ok => Ok(frame
                .get("result")
                .cloned()
                .unwrap_or(Value::Null)),
            ReplyValidation::HasErrorString => {
                Err(AnkiError::Remote(qt_string_of(frame.get("error"))))
            }
            status => Err(AnkiError::Frame(status)),
        }
    }

    /// `version` as a strictly numeric result (the `requestInt` shape).
    pub fn version(&self) -> AnkiResult<i32>
    {
        let result = self.request(AnkiAction::Version, None)?;
        let status = typed_status(TypedReplyKind::RequestInt, &result);
        match status
        {
            TypedReplyStatus::Success => {
                Ok(as_qint32(&result))
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

    /// `addNote` with `{ "note": ... }`, returning the note id
    /// (`requestInt`).
    pub fn add_note(&self, note: Value) -> AnkiResult<i64>
    {
        let params = json!({ "note": note });
        let result = self.request_typed(
            TypedReplyKind::RequestInt,
            AnkiAction::AddNote,
            Some(params),
        )?;
        Ok(as_qint32(&result) as i64)
    }

    /// `storeMediaFile` inside `multi` (the `addMedia` flow): the caller
    /// supplies pre-base64-encoded payloads (encoding is
    /// `memento-util`/`FileUtils::toBase64` business).
    pub fn store_media_files(&self, files: &[MediaFilePayload]) -> AnkiResult<Vec<String>>
    {
        let actions: Vec<Value> = files
            .iter()
            .map(|file| {
                json!({
                    "action": AnkiAction::StoreMediaFile.as_wire(),
                    "params": {
                        "data": file.data_base64,
                        "filename": file.filename,
                    },
                })
            })
            .collect();
        let params = json!({ "actions": actions });
        let result = self
            .request_typed(TypedReplyKind::AddMedia, AnkiAction::Multi, Some(params))?;
        as_string_list(&result)
            .ok_or(AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch))
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
                    .map(|value| as_qint32(value) as i64)
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
        let result = self.request(action, params)?;
        let status = typed_status(kind, &result);
        match status
        {
            TypedReplyStatus::Success | TypedReplyStatus::EmptySuccess => Ok(result),
            other => Err(AnkiError::Typed(other)),
        }
    }
}

/// Pre-encoded media file for the `multi`/`storeMediaFile` flow. The
/// base64 encoding is done by the caller (`FileUtils::toBase64` lives with
/// the `memento-util` file helpers); this crate stays byte-stream based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFilePayload
{
    pub filename: String,
    pub data_base64: String,
}

/// Largest accepted reply body: AnkiConnect `multi` media replies with
/// base64 blobs stay well under this.
const MAX_REPLY_BYTES: u64 = 32 * 1024 * 1024;

fn u16_of(text: &str) -> Vec<u16>
{
    text.encode_utf16().collect()
}

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

/// Converts `serde_json::Value` to the ported UTF-16 `JsonValue` the
/// validators consume.
pub fn to_qjson(value: &Value) -> memento_anki::json::JsonValue
{
    match value
    {
        Value::Null => memento_anki::json::JsonValue::Null,
        Value::Bool(flag) => memento_anki::json::JsonValue::Bool(*flag),
        Value::Number(number) => memento_anki::json::JsonValue::Number(
            number.as_f64().unwrap_or(0.0),
        ),
        Value::String(text) => memento_anki::json::JsonValue::String(u16_of(text)),
        Value::Array(values) => memento_anki::json::JsonValue::Array(
            values.iter().map(to_qjson).collect(),
        ),
        Value::Object(entries) => {
            let mut object = memento_anki::json::JsonObject::new();
            for (key, value) in entries
            {
                object.insert(u16_of(key), to_qjson(value));
            }
            memento_anki::json::JsonValue::Object(object)
        }
    }
}

/// `QJsonValue::toString()` semantics for the `error` field: strings pass
/// through, everything else (numers, objects, arrays) comes back empty.
fn qt_string_of(value: Option<&Value>) -> String
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
                let is_array = result.is_array();
                let elements_match = is_array
                    && result
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|element| element.is_string());
                (is_array, false, elements_match)
            }
            TypedReplyKind::OpenBrowse => {
                let is_array = result.is_array();
                let elements_match = is_array
                    && result
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|element| element.is_number());
                (is_array, false, elements_match)
            }
            TypedReplyKind::RequestBoolList => {
                let is_array = result.is_array();
                let elements_match = is_array
                    && result
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(Value::is_boolean);
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


/// `QJsonValue::toInt()` with its default value: only whole numbers that fit
/// in a signed 32-bit integer convert; other numeric values become zero.
fn as_qint32(value: &Value) -> i32
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

#[cfg(test)]
mod tests {
    use super::{as_qint32, AnkiEndpoint, DEFAULT_TIMEOUT};
    use serde_json::json;

    #[test]
    fn qjson_to_int_requires_a_whole_i32_value()
    {
        assert_eq!(as_qint32(&json!(123)), 123);
        assert_eq!(as_qint32(&json!(123.0)), 123);
        assert_eq!(as_qint32(&json!(123.5)), 0);
        assert_eq!(as_qint32(&json!(i32::MAX as i64 + 1)), 0);
        assert_eq!(as_qint32(&json!(i32::MIN as i64 - 1)), 0);
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

}
