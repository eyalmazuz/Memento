////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
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

//! AnkiConnect wire tests against an ephemeral server: asserts the exact
//! JSON body shapes `AnkiClient::makeRequest` produces (via the ported
//! request model) and the reply-shape taxonomy of `processReply` /
//! `requestStringList` / `requestBoolList` / `requestInt` / `addMedia` /
//! `openBrowseAsync`, all reusing the already-ported validators in
//! `memento_anki::client`.

mod common;

use std::time::Duration;

use common::{Action, CapturedRequest, TestServer};
use serde_json::{json, Value};

use memento_anki::client::{ReplyValidation, TypedReplyStatus};
use memento_net::anki::{AnkiClient, AnkiEndpoint, AnkiError, MediaFilePayload};

// ---- request shapes -----------------------------------------------------

fn endpoint_for(server: &TestServer) -> AnkiEndpoint
{
    AnkiEndpoint {
        hostname: "http://127.0.0.1".to_owned(),
        port: server.port(),
        api_key: None,
        timeout: Some(Duration::from_millis(2000)),
        use_api_key: false,
    }
}

fn json_reply(body: &str) -> Vec<u8>
{
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

fn reply_of(result: Value) -> Vec<u8>
{
    let body = json!({ "error": Value::Null, "result": result }).to_string();
    json_reply(&body)
}

// ---- request shapes -----------------------------------------------------

#[test]
fn version_body_and_reply_shape()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({ "action": "version", "version": 6 }),
            "makeRequest must emit exactly action+version (no params key)"
        );
        Action::Respond(reply_of(json!(6)))
    });

    let client = AnkiClient::new(endpoint_for(&server));
    assert_eq!(client.version().unwrap(), 6);
    client.test_connection().expect("version 6 >= min 6");
}

#[test]
fn api_key_is_sent_only_when_enabled()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({ "action": "version", "version": 6, "key": "secret" })
        );
        Action::Respond(reply_of(json!(6)))
    });

    let client = AnkiClient::new(AnkiEndpoint {
        api_key: Some("secret".to_owned()),
        use_api_key: true,
        ..endpoint_for(&server)
    });
    client.test_connection().unwrap();
}

#[test]
fn deck_names_and_model_names_bodies_are_bare_actions()
{
    let server = TestServer::start();
    server.spawn(move |index, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        match index
        {
            0 => {
                assert_eq!(body, json!({ "action": "deckNames", "version": 6 }));
                Action::Respond(reply_of(json!(["Default", "日本語"])))
            }
            _ => {
                assert_eq!(body, json!({ "action": "modelNames", "version": 6 }));
                Action::Respond(reply_of(json!(["Basic", "Japanese"])))
            }
        }
    });

    let client = AnkiClient::new(endpoint_for(&server));
    assert_eq!(client.deck_names().unwrap(), vec!["Default", "日本語"]);
    assert_eq!(client.model_names().unwrap(), vec!["Basic", "Japanese"]);
}

#[test]
fn field_names_body_carries_modelname_param()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({
                "action": "modelFieldNames",
                "version": 6,
                "params": { "modelName": "Basic" }
            })
        );
        Action::Respond(reply_of(json!(["Front", "Back"])))
    });

    let client = AnkiClient::new(endpoint_for(&server));
    assert_eq!(client.field_names("Basic").unwrap(), vec!["Front", "Back"]);
}

#[test]
fn can_add_notes_body_and_bool_reply()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({
                "action": "canAddNotes",
                "version": 6,
                "params": { "notes": [json!({"deckName": "D"}), json!({"deckName": "D2"})] }
            })
        );
        Action::Respond(reply_of(json!([true, false])))
    });

    let client = AnkiClient::new(endpoint_for(&server));
    let notes = vec![json!({"deckName": "D"}), json!({"deckName": "D2"})];
    assert_eq!(client.can_add_notes(&notes).unwrap(), vec![true, false]);
}

#[test]
fn add_note_body_and_int_reply()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let expected = json!({
            "action": "addNote",
            "version": 6,
            "params": {
                "note": {
                    "deckName": "Default",
                    "modelName": "Basic",
                    "fields": { "Front": "引き燃やし", "Back": "to kindle" },
                    "options": { "allowDuplicate": false }
                }
            }
        });
        assert_eq!(body, expected, "addNote body must mirror makeRequest exactly");
        assert_eq!(request.header("content-length").unwrap(), &request.body.len().to_string());
        Action::Respond(reply_of(json!(1669909161234_i64 as f64)))
    });

    let client = AnkiClient::new(endpoint_for(&server));
    let note = json!({
        "deckName": "Default",
        "modelName": "Basic",
        "fields": { "Front": "引き燃やし", "Back": "to kindle" },
        "options": { "allowDuplicate": false }
    });
    assert_eq!(client.add_note(note).unwrap(), 1669909161234);
}

#[test]
fn multi_store_media_file_body_and_string_reply()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({
                "action": "multi",
                "version": 6,
                "params": {
                    "actions": [
                        {
                            "action": "storeMediaFile",
                            "params": { "data": "aGVsbG8=", "filename": "audio/x.mp3" }
                        },
                        {
                            "action": "storeMediaFile",
                            "params": { "data": "aGVsbG8y", "filename": "audio/y.mp3" }
                        }
                    ]
                }
            })
        );
        Action::Respond(reply_of(json!(["_x.mp3", "_y.mp3"])))
    });

    let client = AnkiClient::new(endpoint_for(&server));
    let files = vec![
        MediaFilePayload {
            filename: "audio/x.mp3".to_owned(),
            data_base64: "aGVsbG8=".to_owned(),
        },
        MediaFilePayload {
            filename: "audio/y.mp3".to_owned(),
            data_base64: "aGVsbG8y".to_owned(),
        },
    ];
    assert_eq!(
        client.store_media_files(&files).unwrap(),
        vec!["_x.mp3", "_y.mp3"]
    );
}

#[test]
fn gui_browse_body_and_int_list_reply()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({
                "action": "guiBrowse",
                "version": 6,
                "params": { "query": "(Front:引き)" }
            })
        );
        Action::Respond(reply_of(json!([1669909161234.0, 1669909161239.0])))
    });

    let client = AnkiClient::new(endpoint_for(&server));
    assert_eq!(
        client.open_browse("(Front:引き)").unwrap(),
        vec![0, 0],
        "QJsonValue::toInt() returns its default for IDs outside the C++ int range"
    );
}

#[test]
fn gui_browse_coerces_only_whole_in_range_integer_ids()
{
    let server = TestServer::start();
    server.spawn(move |_, _| Action::Respond(reply_of(json!([1.5, 42, 2147483648_i64]))));
    let client = AnkiClient::new(endpoint_for(&server));
    assert_eq!(client.open_browse("query").unwrap(), vec![0, 42, 0]);
}

// ---- reply taxonomy -----------------------------------------------------

#[test]
fn remote_error_string_is_surfaced()
{
    let server = TestServer::start();
    server.spawn(move |_, _| {
        Action::Respond(
            json_reply(&json!({ "error": "deck was not found", "result": Value::Null }).to_string()),
        )
    });
    let client = AnkiClient::new(endpoint_for(&server));
    let error = client.deck_names().unwrap_err();
    assert!(matches!(error, AnkiError::Remote(ref m) if m == "deck was not found"));
}

#[test]
fn malformed_reply_bodies_map_to_the_ported_reply_validation()
{
    let cases: [(&str, ReplyValidation); 5] = [
        (r#"not json {{"#, ReplyValidation::NotJson),
        (r#"["an", "array"]"#, ReplyValidation::NotObject),
        (
            r#"{"error": null, "result": null, "extra": 1}"#,
            ReplyValidation::UnexpectedFieldCount,
        ),
        (
            r#"{"result": null, "other": 1}"#,
            ReplyValidation::MissingErrorField,
        ),
        (
            r#"{"error": null, "other": 1}"#,
            ReplyValidation::MissingResultField,
        ),
    ];

    for (index, (reply_text, expected)) in cases.into_iter().enumerate()
    {
        let server = TestServer::start();
        let reply_text = reply_text.to_owned();
        server.spawn(move |_, _| {
            Action::Respond(json_reply(&reply_text))
        });
        let client = AnkiClient::new(endpoint_for(&server));
        let error = client.deck_names().unwrap_err();
        match &error
        {
            AnkiError::Frame(status) => {
                assert_eq!(status, &expected, "case {index}");
            }
            other => panic!("case {index}: expected Frame error, got {other:?}"),
        }
    }
}

#[test]
fn result_shape_mismatches_map_to_the_ported_typed_validators()
{
    // deckNames expects an array of strings.
    let server = TestServer::start();
    server.spawn(move |_, _| {
        Action::Respond(reply_of(json!({"not": "an array"})))
    });
    let client = AnkiClient::new(endpoint_for(&server));
    match client.deck_names().unwrap_err()
    {
        AnkiError::Typed(TypedReplyStatus::ResultNotArray) => {}
        other => panic!("expected ResultNotArray, got {other:?}"),
    }

    // canAddNotes expects booleans.
    let server = TestServer::start();
    server.spawn(move |_, _| {
        Action::Respond(reply_of(json!([true, 1])))
    });
    let client = AnkiClient::new(endpoint_for(&server));
    match client.can_add_notes(&[json!({"x": 1})]).unwrap_err()
    {
        AnkiError::Typed(TypedReplyStatus::ElementTypeMismatch) => {}
        other => panic!("expected ElementTypeMismatch, got {other:?}"),
    }

    // addNote's requestInt wants a double.
    let server = TestServer::start();
    server.spawn(move |_, _| {
        Action::Respond(reply_of(json!("not-a-number")))
    });
    let client = AnkiClient::new(endpoint_for(&server));
    match client.add_note(json!({"deckName": "x"})).unwrap_err()
    {
        AnkiError::Typed(TypedReplyStatus::ResultNotDouble) => {}
        other => panic!("expected ResultNotDouble, got {other:?}"),
    }
}

#[test]
fn min_version_failure_is_reported_as_a_version_error()
{
    let server = TestServer::start();
    server.spawn(move |_, _| Action::Respond(reply_of(json!(5))));
    let client = AnkiClient::new(endpoint_for(&server));
    match client.test_connection().unwrap_err()
    {
        AnkiError::VersionTooLow { version, min } => {
            assert_eq!(version, 5);
            assert_eq!(min, 6);
        }
        other => panic!("expected VersionTooLow, got {other:?}"),
    }
}

#[test]
fn timeouts_surface_as_network_errors()
{
    let server = TestServer::start();
    server.spawn(|_, _| Action::Stall);
    let client = AnkiClient::new(AnkiEndpoint {
        timeout: Some(Duration::from_millis(200)),
        ..endpoint_for(&server)
    });
    let error = client.version().unwrap_err();
    assert!(error.is_network(), "timeout must be Network-typed, got {error:?}");
}
#[test]
fn http_error_status_is_not_accepted_as_a_valid_anki_frame()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        let body = r#"{"result":6,"error":null}"#;
        Action::Respond(
            format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
        )
    });
    let client = AnkiClient::new(endpoint_for(&server));
    let error = client.version().unwrap_err();
    assert!(
        error.is_network(),
        "the C++ MementoNetReply reports failures as ConnectionRefusedError"
    );
    match error
    {
        AnkiError::HttpStatus { status: 500, .. } => {}
        other => panic!("expected HTTP status error, got {other:?}"),
    }
}


#[test]
fn forbidden_action_body_stays_outside_the_verb_set()
{
    // The wire strings are exactly the constants in `ankiconnect.h`; the
    // exhaustive enum prevents accidental drift.
    use memento_net::anki::AnkiAction::*;
    assert_eq!(Version.as_wire(), "version");
    assert_eq!(DeckNames.as_wire(), "deckNames");
    assert_eq!(ModelNames.as_wire(), "modelNames");
    assert_eq!(FieldNames.as_wire(), "modelFieldNames");
    assert_eq!(CanAddNotes.as_wire(), "canAddNotes");
    assert_eq!(AddNote.as_wire(), "addNote");
    assert_eq!(GuiBrowse.as_wire(), "guiBrowse");
    assert_eq!(StoreMediaFile.as_wire(), "storeMediaFile");
    assert_eq!(Multi.as_wire(), "multi");
}
