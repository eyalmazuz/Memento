//! Pure Rust protocol, query, and context helpers for AnkiConfig and AnkiClient.

/// Matches `AnkiConfig::load()` lines 108–112:
/// If `host` does not start with `"http://"` and does not start with `"https://"`,
/// prepends `"http://"`, else returns `host` unchanged.
pub fn normalize_hostname(host: &[u16]) -> Vec<u16>
{
    const HTTP_PREFIX: &[u16] = &[
        b'h' as u16, b't' as u16, b't' as u16, b'p' as u16, b':' as u16, b'/' as u16, b'/' as u16,
    ];
    const HTTPS_PREFIX: &[u16] = &[
        b'h' as u16, b't' as u16, b't' as u16, b'p' as u16, b's' as u16, b':' as u16, b'/' as u16, b'/' as u16,
    ];

    if host.starts_with(HTTP_PREFIX) || host.starts_with(HTTPS_PREFIX)
    {
        host.to_vec()
    }
    else
    {
        let mut result = Vec::with_capacity(HTTP_PREFIX.len() + host.len());
        result.extend_from_slice(HTTP_PREFIX);
        result.extend_from_slice(host);
        result
    }
}

/// Matches `AnkiConfig::profileIndex` / `addProfile` / `removeProfile` / `setProfile`
/// binary search (`std::lower_bound` over UTF-16 code unit ordering `profile->name() < name`).
pub fn profile_lower_bound(names: &[&[u16]], target: &[u16]) -> (usize, bool)
{
    let index = names.partition_point(|&name| name < target);
    let exact = index < names.len() && names[index] == target;
    (index, exact)
}

/// Matches `AnkiClient::openBrowseAsync` lines 520–534:
/// - If `duplicate_policy` is `Anki::DuplicatePolicyDifferentDeck` (`1`) or
///   `Anki::DuplicatePolicySameDeck` (`2`), prepends `"\"deck:" + deck + "\" "`.
/// - Appends `'(' + query + ')'`.
pub fn build_browse_query(duplicate_policy: i32, deck: &[u16], query: &[u16]) -> Vec<u16>
{
    let mut result = Vec::new();
    if duplicate_policy == 1 || duplicate_policy == 2
    {
        result.extend([
            b'"' as u16, b'd' as u16, b'e' as u16, b'c' as u16, b'k' as u16, b':' as u16,
        ]);
        result.extend_from_slice(deck);
        result.extend([b'"' as u16, b' ' as u16]);
    }
    result.push(b'(' as u16);
    result.extend_from_slice(query);
    result.push(b')' as u16);
    result
}

fn escape_duplicate_value_into(val: &[u16], out: &mut Vec<u16>)
{
    for &ch in val
    {
        if ch == b'\\' as u16
        {
            out.push(b'\\' as u16);
            out.push(b'\\' as u16);
        }
        else if ch == b'"' as u16
        {
            out.push(b'\\' as u16);
            out.push(b'"' as u16);
        }
        else
        {
            out.push(ch);
        }
    }
}

/// Matches `AnkiClient::openDuplicatesAsync` lines 630–650:
/// - Escapes `\` -> `\\` and `"` -> `\"` in `first_val` and formats `{field_key}:"{escaped_first}"`.
/// - If `has_second_val`, escapes `\` -> `\\` and `"` -> `\"` in `second_val` and appends ` OR {field_key}:"{escaped_second}"`.
pub fn build_duplicate_query(
    field_key: &[u16],
    first_val: &[u16],
    has_second_val: bool,
    second_val: &[u16],
) -> Vec<u16>
{
    let mut result = Vec::new();
    result.extend_from_slice(field_key);
    result.push(b':' as u16);
    result.push(b'"' as u16);
    escape_duplicate_value_into(first_val, &mut result);
    result.push(b'"' as u16);

    if has_second_val
    {
        result.extend([b' ' as u16, b'O' as u16, b'R' as u16, b' ' as u16]);
        result.extend_from_slice(field_key);
        result.push(b':' as u16);
        result.push(b'"' as u16);
        escape_duplicate_value_into(second_val, &mut result);
        result.push(b'"' as u16);
    }

    result
}

#[derive(Clone, Debug)]
pub struct SubtitleCue<'a>
{
    pub text: &'a [u16],
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggregatedContext
{
    pub raw_start: f64,
    pub raw_end: f64,
    pub text: Vec<u16>,
}

/// Matches `AnkiClient::populate` context selection aggregation (lines 959–995 and 1003–1028)
/// given pre-filtered item texts and selected row indices:
/// - Sorts `selected_rows` ascending (`lhs < rhs`).
/// - Iterates sorted rows: if `items[row].text` is empty, skips; otherwise appends
///   `items[row].text` to `contextList` and updates `contextEndTime = contextEndTime.max(items[row].end)`
///   (with `contextEndTime` initialized to `0.0`).
/// - If `contextList` is non-empty, `out_raw_start = items[sorted_rows[0]].start` (note: in C++,
///   `primaryListItems[primarySelection.front().row()].start` uses `primarySelection.front().row()` even if row 0's text was empty!),
///   `out_raw_end = contextEndTime`, and `out_text = contextList.join('\n')`.
pub fn aggregate_context(
    items: &[SubtitleCue<'_>],
    selected_rows: &[i32],
) -> Result<Option<AggregatedContext>, ()>
{
    if selected_rows.is_empty()
    {
        return Ok(None);
    }

    for &row in selected_rows
    {
        if row < 0 || (row as usize) >= items.len()
        {
            return Err(());
        }
    }

    let mut sorted_rows = selected_rows.to_vec();
    sorted_rows.sort_unstable();

    let mut context_list: Vec<&[u16]> = Vec::new();
    let mut context_end_time = 0.0f64;

    for &row in &sorted_rows
    {
        let item = &items[row as usize];
        if item.text.is_empty()
        {
            continue;
        }
        context_list.push(item.text);
        if item.end > context_end_time
        {
            context_end_time = item.end;
        }
    }

    if context_list.is_empty()
    {
        return Ok(None);
    }

    let raw_start = items[sorted_rows[0] as usize].start;
    let raw_end = context_end_time;

    let total_len: usize =
        context_list.iter().map(|s| s.len()).sum::<usize>() + (context_list.len() - 1);
    let mut joined = Vec::with_capacity(total_len);
    for (i, line) in context_list.iter().enumerate()
    {
        if i > 0
        {
            joined.push(b'\n' as u16);
        }
        joined.extend_from_slice(line);
    }

    Ok(Some(AggregatedContext {
        raw_start,
        raw_end,
        text: joined,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedField<T = Vec<u16>>
{
    pub name: T,
    pub value: T,
}

pub type ExtractedFieldOwned = ExtractedField<Vec<u16>>;
pub type ExtractedFieldView<'a> = ExtractedField<&'a [u16]>;

#[derive(Clone, Debug, PartialEq)]
pub enum FieldArrayCandidate<'a>
{
    Pair
    {
        name: &'a [u16],
        value: &'a [u16],
    },
    Invalid,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldObjectCandidate<'a>
{
    Entry
    {
        key: &'a [u16],
        value: &'a [u16],
    },
    NonString,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldSetInput<'a>
{
    ArrayCandidates(&'a [FieldArrayCandidate<'a>]),
    ObjectCandidates(&'a [FieldObjectCandidate<'a>]),
    Json(&'a crate::json::JsonValue),
}

/// Matches `AnkiFieldListModel::setItems` JSON field extraction for both array
/// and object payloads:
/// - In array mode: filters to 2-element arrays of strings `[name, value]`.
/// - In object mode: filters to string values, preserving key order.
pub fn extract_fields<'a>(input: FieldSetInput<'a>) -> Vec<ExtractedFieldView<'a>>
{
    match input
    {
        FieldSetInput::ArrayCandidates(candidates) => candidates
            .iter()
            .filter_map(|c| match c {
                FieldArrayCandidate::Pair { name, value } => Some(ExtractedField {
                    name: *name,
                    value: *value,
                }),
                FieldArrayCandidate::Invalid => None,
            })
            .collect(),
        FieldSetInput::ObjectCandidates(candidates) => candidates
            .iter()
            .filter_map(|c| match c {
                FieldObjectCandidate::Entry { key, value } => Some(ExtractedField {
                    name: *key,
                    value: *value,
                }),
                FieldObjectCandidate::NonString => None,
            })
            .collect(),
        FieldSetInput::Json(val) => match val
        {
            crate::json::JsonValue::Array(arr) => arr
                .iter()
                .filter_map(|item| match item {
                    crate::json::JsonValue::Array(pair) if pair.len() == 2 => {
                        match (&pair[0], &pair[1]) {
                            (
                                crate::json::JsonValue::String(name),
                                crate::json::JsonValue::String(value),
                            ) => Some(ExtractedField {
                                name: name.as_slice(),
                                value: value.as_slice(),
                            }),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect(),
            crate::json::JsonValue::Object(obj) => obj
                .iter()
                .filter_map(|(key, v)| match v {
                    crate::json::JsonValue::String(value) => Some(ExtractedField {
                        name: key.as_slice(),
                        value: value.as_slice(),
                    }),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
    }
}

pub fn extract_fields_from_array_candidates<'a>(
    candidates: &'a [FieldArrayCandidate<'a>],
) -> Vec<ExtractedFieldView<'a>>
{
    extract_fields(FieldSetInput::ArrayCandidates(candidates))
}

pub fn extract_fields_from_object_candidates<'a>(
    candidates: &'a [FieldObjectCandidate<'a>],
) -> Vec<ExtractedFieldView<'a>>
{
    extract_fields(FieldSetInput::ObjectCandidates(candidates))
}

/// Extracts fields from a `JsonValue`. Returns `Some(vec)` if `val` is an Array
/// or Object, or `None` if `val` is any other JSON type (matching
/// `AnkiFieldListModel::setItems(const QJsonValue &)` return boolean).
pub fn extract_fields_from_json<'a>(
    val: &'a crate::json::JsonValue,
) -> Option<Vec<ExtractedFieldView<'a>>>
{
    match val
    {
        crate::json::JsonValue::Array(_) | crate::json::JsonValue::Object(_) => {
            Some(extract_fields(FieldSetInput::Json(val)))
        }
        _ => None,
    }
}

/// Matches `AnkiClient::processReply` status codes:
/// - 0 = Ok
/// - 1 = NotJson ("Reply was not JSON")
/// - 2 = NotObject ("Reply was not an object")
/// - 3 = UnexpectedFieldCount ("Anki response has unexpected number of fields")
/// - 4 = MissingErrorField ("Anki response is missing error field")
/// - 5 = MissingResultField ("Anki response is missing result field")
/// - 6 = HasErrorString (error string is non-null)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ReplyValidation
{
    Ok = 0,
    NotJson = 1,
    NotObject = 2,
    UnexpectedFieldCount = 3,
    MissingErrorField = 4,
    MissingResultField = 5,
    HasErrorString = 6,
}

impl ReplyValidation
{
    pub const fn to_c_status(self) -> i32
    {
        self as i32
    }

    pub const fn from_c_status(val: i32) -> Option<Self>
    {
        match val
        {
            0 => Some(Self::Ok),
            1 => Some(Self::NotJson),
            2 => Some(Self::NotObject),
            3 => Some(Self::UnexpectedFieldCount),
            4 => Some(Self::MissingErrorField),
            5 => Some(Self::MissingResultField),
            6 => Some(Self::HasErrorString),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplyMetadata<'a>
{
    pub is_null_doc: bool,
    pub is_object: bool,
    pub field_count: i32,
    pub has_error_key: bool,
    pub has_result_key: bool,
    pub error_is_null: bool,
    pub error_str: &'a [u16],
}

pub fn validate_reply_metadata(meta: &ReplyMetadata<'_>) -> ReplyValidation
{
    if meta.is_null_doc
    {
        ReplyValidation::NotJson
    }
    else if !meta.is_object
    {
        ReplyValidation::NotObject
    }
    else if meta.field_count != 2
    {
        ReplyValidation::UnexpectedFieldCount
    }
    else if !meta.has_error_key
    {
        ReplyValidation::MissingErrorField
    }
    else if !meta.has_result_key
    {
        ReplyValidation::MissingResultField
    }
    else if !meta.error_is_null
    {
        ReplyValidation::HasErrorString
    }
    else
    {
        ReplyValidation::Ok
    }
}

pub fn validate_reply_json(val: &crate::json::JsonValue) -> ReplyValidation
{
    match val
    {
        crate::json::JsonValue::Null | crate::json::JsonValue::Undefined => {
            ReplyValidation::NotJson
        }
        crate::json::JsonValue::Object(obj) => {
            if obj.len() != 2
            {
                ReplyValidation::UnexpectedFieldCount
            }
            else
            {
                let error_key: Vec<u16> = "error".encode_utf16().collect();
                let result_key: Vec<u16> = "result".encode_utf16().collect();
                if !obj.contains_key(&error_key)
                {
                    ReplyValidation::MissingErrorField
                }
                else if !obj.contains_key(&result_key)
                {
                    ReplyValidation::MissingResultField
                }
                else if !matches!(obj.get(&error_key), Some(crate::json::JsonValue::Null))
                {
                    ReplyValidation::HasErrorString
                }
                else
                {
                    ReplyValidation::Ok
                }
            }
        }
        _ => ReplyValidation::NotObject,
    }
}

/// Validates an AnkiConnect reply JSON value and, if valid, extracts the
/// `result` field value. Returns `Err(validation_error)` if validation fails.
pub fn parse_reply_json<'a>(
    val: &'a crate::json::JsonValue,
) -> Result<&'a crate::json::JsonValue, ReplyValidation>
{
    let status = validate_reply_json(val);
    if status != ReplyValidation::Ok
    {
        return Err(status);
    }
    match val
    {
        crate::json::JsonValue::Object(obj) =>
        {
            let result_key: Vec<u16> = "result".encode_utf16().collect();
            obj.get(&result_key).ok_or(ReplyValidation::MissingResultField)
        }
        _ => Err(ReplyValidation::NotObject),
    }
}

/// If the reply contains an error string, returns it as a UTF-16 slice.
pub fn extract_reply_error<'a>(
    val: &'a crate::json::JsonValue,
) -> Option<&'a [u16]>
{
    match val
    {
        crate::json::JsonValue::Object(obj) =>
        {
            let error_key: Vec<u16> = "error".encode_utf16().collect();
            match obj.get(&error_key)
            {
                Some(crate::json::JsonValue::String(s)) => Some(s.as_slice()),
                _ => None,
            }
        }
        _ => None,
    }
}

pub const MIN_ANKICONNECT_VERSION: i32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionCheckResult
{
    Ok,
    NotANumber,
    VersionTooLow
    {
        min_version: i32
    },
}

impl VersionCheckResult
{
    pub const fn to_c_status(self) -> i32
    {
        match self
        {
            Self::Ok => 0,
            Self::NotANumber => 1,
            Self::VersionTooLow { .. } => 2,
        }
    }
}

pub fn check_ankiconnect_version(result_is_double: bool, version_int: i32) -> VersionCheckResult
{
    if !result_is_double
    {
        VersionCheckResult::NotANumber
    }
    else if version_int < MIN_ANKICONNECT_VERSION
    {
        VersionCheckResult::VersionTooLow
        {
            min_version: MIN_ANKICONNECT_VERSION,
        }
    }
    else
    {
        VersionCheckResult::Ok
    }
}

/// Matches `AnkiClient::makeRequest` lines 978–980:
/// Formats `{hostname}:{port}` into a UTF-16 buffer.
pub fn format_endpoint_url(hostname: &[u16], port: &[u16]) -> Vec<u16>
{
    let mut result = Vec::with_capacity(hostname.len() + 1 + port.len());
    result.extend_from_slice(hostname);
    result.push(b':' as u16);
    result.extend_from_slice(port);
    result
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanAddNotesResult
{
    pub first: bool,
    pub second: bool,
}

/// Matches `AnkiClient::noteAddableAsync` lines 314–323 and 368–375:
/// Given bool list of flags from `canAddNotes`, extracts first and second note addability.
pub fn interpret_can_add_notes(flags: &[bool]) -> CanAddNotesResult
{
    let mut it = flags.iter().copied();
    CanAddNotesResult {
        first: it.next().unwrap_or(false),
        second: it.next().unwrap_or(false),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum TypedReplyKind
{
    AddMedia = 0,
    OpenBrowse = 1,
    RequestStringList = 2,
    RequestBoolList = 3,
    RequestInt = 4,
}

impl TypedReplyKind
{
    pub const fn from_c_kind(val: i32) -> Option<Self>
    {
        match val
        {
            0 => Some(Self::AddMedia),
            1 => Some(Self::OpenBrowse),
            2 => Some(Self::RequestStringList),
            3 => Some(Self::RequestBoolList),
            4 => Some(Self::RequestInt),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum TypedReplyStatus
{
    Success = 0,
    EmptySuccess = 1,
    ReplyError = 2,
    ResultNotArray = 3,
    ElementTypeMismatch = 4,
    EmptyError = 5,
    ResultNotDouble = 6,
    InvalidKind = 7,
}

impl TypedReplyStatus
{
    pub const fn to_c_status(self) -> i32
    {
        self as i32
    }

    pub const fn from_c_status(val: i32) -> Option<Self>
    {
        match val
        {
            0 => Some(Self::Success),
            1 => Some(Self::EmptySuccess),
            2 => Some(Self::ReplyError),
            3 => Some(Self::ResultNotArray),
            4 => Some(Self::ElementTypeMismatch),
            5 => Some(Self::EmptyError),
            6 => Some(Self::ResultNotDouble),
            7 => Some(Self::InvalidKind),
            _ => None,
        }
    }
}

/// Matches `AnkiClient` validation of reply objects across `addMedia`, `openBrowseAsync`,
/// `requestStringList`, `requestBoolList`, and `requestInt`.
pub fn validate_typed_result(
    kind: TypedReplyKind,
    has_reply_error: bool,
    reply_obj_empty: bool,
    result_is_array: bool,
    result_is_double: bool,
    all_elements_match_type: bool,
) -> TypedReplyStatus
{
    if has_reply_error
    {
        return TypedReplyStatus::ReplyError;
    }
    match kind
    {
        TypedReplyKind::AddMedia =>
        {
            if reply_obj_empty
            {
                TypedReplyStatus::EmptySuccess
            }
            else if !result_is_array
            {
                TypedReplyStatus::ResultNotArray
            }
            else if !all_elements_match_type
            {
                TypedReplyStatus::ElementTypeMismatch
            }
            else
            {
                TypedReplyStatus::Success
            }
        }
        TypedReplyKind::OpenBrowse =>
        {
            if reply_obj_empty
            {
                TypedReplyStatus::EmptySuccess
            }
            else if !result_is_array
            {
                TypedReplyStatus::ResultNotArray
            }
            else if !all_elements_match_type
            {
                TypedReplyStatus::ElementTypeMismatch
            }
            else
            {
                TypedReplyStatus::Success
            }
        }
        TypedReplyKind::RequestStringList =>
        {
            if reply_obj_empty
            {
                TypedReplyStatus::EmptySuccess
            }
            else if !result_is_array
            {
                TypedReplyStatus::ResultNotArray
            }
            else
            {
                // In C++ requestStringList lines 863–872:
                // !string.isString() constructs a local unused QVariantMap and still
                // appends string.toString() without aborting.
                TypedReplyStatus::Success
            }
        }
        TypedReplyKind::RequestBoolList =>
        {
            if reply_obj_empty
            {
                TypedReplyStatus::EmptySuccess
            }
            else if !result_is_array
            {
                TypedReplyStatus::ResultNotArray
            }
            else if !all_elements_match_type
            {
                TypedReplyStatus::ElementTypeMismatch
            }
            else
            {
                TypedReplyStatus::Success
            }
        }
        TypedReplyKind::RequestInt =>
        {
            if reply_obj_empty
            {
                TypedReplyStatus::EmptyError
            }
            else if !result_is_double
            {
                TypedReplyStatus::ResultNotDouble
            }
            else
            {
                TypedReplyStatus::Success
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonStringCandidate<'a>
{
    pub is_string: bool,
    pub text: &'a [u16],
}

/// Matches `AnkiConfig::load()` JSON string-array filtering (lines 180–204):
/// Given candidate elements `is_string` and `text`, filters to entries where
/// `is_string == true`, preserving order.
pub fn filter_json_strings<'a>(
    candidates: &[JsonStringCandidate<'a>],
) -> Vec<&'a [u16]>
{
    candidates
        .iter()
        .filter(|c| c.is_string)
        .map(|c| c.text)
        .collect()
}

/// Builds the standard AnkiConnect JSON request payload matching `AnkiClient::makeRequest`:
/// `action`, `version: 6`, optional `key`, and optional `params`.
pub fn build_anki_payload(
    action: &[u16],
    api_key: Option<&[u16]>,
    params: Option<crate::json::JsonObject>,
) -> crate::json::JsonObject
{
    let mut msg = crate::json::JsonObject::new();
    let action_key: Vec<u16> = "action".encode_utf16().collect();
    let version_key: Vec<u16> = "version".encode_utf16().collect();
    msg.insert(action_key, crate::json::JsonValue::String(action.to_vec()));
    msg.insert(
        version_key,
        crate::json::JsonValue::Number(MIN_ANKICONNECT_VERSION as f64),
    );

    if let Some(key) = api_key
    {
        if !key.is_empty()
        {
            let key_key: Vec<u16> = "key".encode_utf16().collect();
            msg.insert(key_key, crate::json::JsonValue::String(key.to_vec()));
        }
    }

    if let Some(p) = params
    {
        if !p.is_empty()
        {
            let params_key: Vec<u16> = "params".encode_utf16().collect();
            msg.insert(params_key, crate::json::JsonValue::Object(p));
        }
    }

    msg
}

/// Builds params object for `canAddNotes`: `{"notes": [...]}` matching
/// `AnkiClient::noteAddableAsync` and `AnkiConnect::Note::CAN_ADD_NOTES_PARAM`.
pub fn build_can_add_notes_params(notes: Vec<crate::json::JsonObject>) -> crate::json::JsonObject
{
    let mut params = crate::json::JsonObject::new();
    let key: Vec<u16> = "notes".encode_utf16().collect();
    params.insert(
        key,
        crate::json::JsonValue::Array(
            notes.into_iter().map(crate::json::JsonValue::Object).collect(),
        ),
    );
    params
}

/// Builds complete AnkiConnect request payload for `canAddNotes`.
pub fn build_can_add_notes_payload(
    notes: Vec<crate::json::JsonObject>,
    api_key: Option<&[u16]>,
) -> crate::json::JsonObject
{
    let action: Vec<u16> = "canAddNotes".encode_utf16().collect();
    let params = build_can_add_notes_params(notes);
    build_anki_payload(&action, api_key, Some(params))
}

/// Builds params object for `addNote`: `{"note": {...}}` matching
/// `AnkiClient::addNoteHelper` and `AnkiConnect::Param::ADD_NOTE`.
pub fn build_add_note_params(note: crate::json::JsonObject) -> crate::json::JsonObject
{
    let mut params = crate::json::JsonObject::new();
    let key: Vec<u16> = "note".encode_utf16().collect();
    params.insert(key, crate::json::JsonValue::Object(note));
    params
}

/// Builds complete AnkiConnect request payload for `addNote`.
pub fn build_add_note_payload(
    note: crate::json::JsonObject,
    api_key: Option<&[u16]>,
) -> crate::json::JsonObject
{
    let action: Vec<u16> = "addNote".encode_utf16().collect();
    let params = build_add_note_params(note);
    build_anki_payload(&action, api_key, Some(params))
}

/// Builds params object for `guiBrowse`: `{"query": "..."}` matching
/// `AnkiClient::openBrowseAsync` and `AnkiConnect::Param::QUERY`.
pub fn build_gui_browse_params(query: &[u16]) -> crate::json::JsonObject
{
    let mut params = crate::json::JsonObject::new();
    let key: Vec<u16> = "query".encode_utf16().collect();
    params.insert(key, crate::json::JsonValue::String(query.to_vec()));
    params
}

/// Builds complete AnkiConnect request payload for `guiBrowse`.
pub fn build_gui_browse_payload(
    query: &[u16],
    api_key: Option<&[u16]>,
) -> crate::json::JsonObject
{
    let action: Vec<u16> = "guiBrowse".encode_utf16().collect();
    let params = build_gui_browse_params(query);
    build_anki_payload(&action, api_key, Some(params))
}

/// Builds params object for `modelFieldNames`: `{"modelName": "..."}` matching
/// `AnkiClient::getFieldNamesAsync` and `AnkiConnect::Param::MODEL_NAME`.
pub fn build_model_field_names_params(model_name: &[u16]) -> crate::json::JsonObject
{
    let mut params = crate::json::JsonObject::new();
    let key: Vec<u16> = "modelName".encode_utf16().collect();
    params.insert(key, crate::json::JsonValue::String(model_name.to_vec()));
    params
}

/// Builds complete AnkiConnect request payload for `modelFieldNames`.
pub fn build_model_field_names_payload(
    model_name: &[u16],
    api_key: Option<&[u16]>,
) -> crate::json::JsonObject
{
    let action: Vec<u16> = "modelFieldNames".encode_utf16().collect();
    let params = build_model_field_names_params(model_name);
    build_anki_payload(&action, api_key, Some(params))
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn to_u16(s: &str) -> Vec<u16>
    {
        s.encode_utf16().collect()
    }

    fn from_u16(units: &[u16]) -> String
    {
        String::from_utf16_lossy(units)
    }

    #[test]
    fn test_normalize_hostname()
    {
        assert_eq!(
            from_u16(&normalize_hostname(&to_u16("127.0.0.1:8765"))),
            "http://127.0.0.1:8765"
        );
        assert_eq!(
            from_u16(&normalize_hostname(&to_u16("http://127.0.0.1:8765"))),
            "http://127.0.0.1:8765"
        );
        assert_eq!(
            from_u16(&normalize_hostname(&to_u16("https://anki.example.com"))),
            "https://anki.example.com"
        );
        assert_eq!(
            from_u16(&normalize_hostname(&to_u16(""))),
            "http://"
        );
        assert_eq!(
            from_u16(&normalize_hostname(&to_u16("http"))),
            "http://http"
        );
    }

    #[test]
    fn test_profile_lower_bound()
    {
        let n1 = to_u16("Apple");
        let n2 = to_u16("Banana");
        let n3 = to_u16("Cherry");
        let names = vec![n1.as_slice(), n2.as_slice(), n3.as_slice()];

        // Before first
        let (idx, exact) = profile_lower_bound(&names, &to_u16("Aardvark"));
        assert_eq!(idx, 0);
        assert!(!exact);

        // Exact first
        let (idx, exact) = profile_lower_bound(&names, &to_u16("Apple"));
        assert_eq!(idx, 0);
        assert!(exact);

        // Between first and second
        let (idx, exact) = profile_lower_bound(&names, &to_u16("Avocado"));
        assert_eq!(idx, 1);
        assert!(!exact);

        // Exact middle
        let (idx, exact) = profile_lower_bound(&names, &to_u16("Banana"));
        assert_eq!(idx, 1);
        assert!(exact);

        // Exact last
        let (idx, exact) = profile_lower_bound(&names, &to_u16("Cherry"));
        assert_eq!(idx, 2);
        assert!(exact);

        // After last
        let (idx, exact) = profile_lower_bound(&names, &to_u16("Date"));
        assert_eq!(idx, 3);
        assert!(!exact);

        // Empty slice
        let empty: Vec<&[u16]> = Vec::new();
        let (idx, exact) = profile_lower_bound(&empty, &to_u16("Apple"));
        assert_eq!(idx, 0);
        assert!(!exact);
    }

    #[test]
    fn test_build_browse_query()
    {
        let deck = to_u16("Default");
        let query = to_u16("tag:marked");

        // Policy 0 (None)
        assert_eq!(
            from_u16(&build_browse_query(0, &deck, &query)),
            "(tag:marked)"
        );

        // Policy 1 (DifferentDeck)
        assert_eq!(
            from_u16(&build_browse_query(1, &deck, &query)),
            "\"deck:Default\" (tag:marked)"
        );

        // Policy 2 (SameDeck)
        assert_eq!(
            from_u16(&build_browse_query(2, &deck, &query)),
            "\"deck:Default\" (tag:marked)"
        );

        // Other policy
        assert_eq!(
            from_u16(&build_browse_query(99, &deck, &query)),
            "(tag:marked)"
        );
    }

    #[test]
    fn test_build_duplicate_query()
    {
        let key = to_u16("Expression");
        let val1 = to_u16(r#"hello "world" \ test"#);
        let val2 = to_u16(r#"second \"val\""#);

        let q1 = build_duplicate_query(&key, &val1, false, &[]);
        assert_eq!(
            from_u16(&q1),
            r#"Expression:"hello \"world\" \\ test""#
        );

        let q2 = build_duplicate_query(&key, &val1, true, &val2);
        assert_eq!(
            from_u16(&q2),
            r#"Expression:"hello \"world\" \\ test" OR Expression:"second \\\"val\\\"""#
        );
    }

    #[test]
    fn test_aggregate_context()
    {
        let t0 = to_u16("");
        let t1 = to_u16("Line 1");
        let t2 = to_u16("Line 2");
        let cues = vec![
            SubtitleCue { text: &t0, start: 1.0, end: 2.0 },
            SubtitleCue { text: &t1, start: 3.0, end: 5.0 },
            SubtitleCue { text: &t2, start: 6.0, end: 8.0 },
        ];

        // Empty selection
        assert_eq!(aggregate_context(&cues, &[]), Ok(None));

        // Out of bounds
        assert_eq!(aggregate_context(&cues, &[3]), Err(()));
        assert_eq!(aggregate_context(&cues, &[-1]), Err(()));

        // Out of order: [2, 0, 1]
        // Row 0 has empty text but start=1.0. Row 1 has text, Row 2 has text.
        // Sorted rows: [0, 1, 2]
        // raw_start must be items[sorted_rows[0]].start == 1.0!
        let res = aggregate_context(&cues, &[2, 0, 1]).unwrap().unwrap();
        assert_eq!(res.raw_start, 1.0);
        assert_eq!(res.raw_end, 8.0);
        assert_eq!(from_u16(&res.text), "Line 1\nLine 2");

        // All empty text
        let cues_empty = vec![
            SubtitleCue { text: &t0, start: 1.0, end: 2.0 },
        ];
        assert_eq!(aggregate_context(&cues_empty, &[0]), Ok(None));
    }

    #[test]
    fn test_extract_fields_array_candidates()
    {
        let n1 = to_u16("Field1");
        let v1 = to_u16("Val1");
        let n2 = to_u16("Field2");
        let v2 = to_u16("Val2");

        let candidates = [
            FieldArrayCandidate::Invalid,
            FieldArrayCandidate::Pair {
                name: &n1,
                value: &v1,
            },
            FieldArrayCandidate::Invalid,
            FieldArrayCandidate::Pair {
                name: &n2,
                value: &v2,
            },
        ];

        let extracted = extract_fields_from_array_candidates(&candidates);
        assert_eq!(extracted.len(), 2);
        assert_eq!(from_u16(extracted[0].name), "Field1");
        assert_eq!(from_u16(extracted[0].value), "Val1");
        assert_eq!(from_u16(extracted[1].name), "Field2");
        assert_eq!(from_u16(extracted[1].value), "Val2");
    }

    #[test]
    fn test_extract_fields_object_candidates()
    {
        let k1 = to_u16("KeyA");
        let v1 = to_u16("ValA");
        let k2 = to_u16("KeyB");
        let _v2 = to_u16("123");

        let candidates = [
            FieldObjectCandidate::Entry {
                key: &k1,
                value: &v1,
            },
            FieldObjectCandidate::NonString,
            FieldObjectCandidate::Entry {
                key: &k2,
                value: &v1,
            },
        ];

        let extracted = extract_fields_from_object_candidates(&candidates);
        assert_eq!(extracted.len(), 2);
        assert_eq!(from_u16(extracted[0].name), "KeyA");
        assert_eq!(from_u16(extracted[1].name), "KeyB");
    }

    #[test]
    fn test_extract_fields_from_json()
    {
        use crate::json::{JsonObject, JsonValue};

        // Non-array / non-object returns None
        assert_eq!(extract_fields_from_json(&JsonValue::Null), None);
        assert_eq!(extract_fields_from_json(&JsonValue::Number(42.0)), None);
        assert_eq!(extract_fields_from_json(&JsonValue::String(to_u16("hi"))), None);

        // Array
        let arr = JsonValue::Array(vec![
            JsonValue::Number(1.0),
            JsonValue::Array(vec![JsonValue::String(to_u16("F1")), JsonValue::String(to_u16("V1"))]),
            JsonValue::Array(vec![JsonValue::String(to_u16("F2"))]), // length 1 -> skipped
            JsonValue::Array(vec![JsonValue::String(to_u16("F3")), JsonValue::Number(2.0)]), // non-string -> skipped
            JsonValue::Array(vec![JsonValue::String(to_u16("F4")), JsonValue::String(to_u16("V4"))]),
        ]);
        let extracted_arr = extract_fields_from_json(&arr).unwrap();
        assert_eq!(extracted_arr.len(), 2);
        assert_eq!(from_u16(extracted_arr[0].name), "F1");
        assert_eq!(from_u16(extracted_arr[0].value), "V1");
        assert_eq!(from_u16(extracted_arr[1].name), "F4");
        assert_eq!(from_u16(extracted_arr[1].value), "V4");

        // Object (keys sorted in BTreeMap)
        let mut obj = JsonObject::new();
        obj.insert(to_u16("B"), JsonValue::String(to_u16("valB")));
        obj.insert(to_u16("A"), JsonValue::String(to_u16("valA")));
        obj.insert(to_u16("C"), JsonValue::Number(123.0)); // non-string -> skipped
        let obj_val = JsonValue::Object(obj);
        let extracted_obj = extract_fields_from_json(&obj_val).unwrap();
        assert_eq!(extracted_obj.len(), 2);
        assert_eq!(from_u16(extracted_obj[0].name), "A");
        assert_eq!(from_u16(extracted_obj[0].value), "valA");
        assert_eq!(from_u16(extracted_obj[1].name), "B");
        assert_eq!(from_u16(extracted_obj[1].value), "valB");
    }

    #[test]
    fn test_reply_validation()
    {
        let empty = to_u16("");
        let err_msg = to_u16("some error");

        // Null doc -> NotJson (1)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: true,
                is_object: false,
                field_count: 0,
                has_error_key: false,
                has_result_key: false,
                error_is_null: true,
                error_str: &empty,
            }),
            ReplyValidation::NotJson
        );

        // Not object -> NotObject (2)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: false,
                is_object: false,
                field_count: 0,
                has_error_key: false,
                has_result_key: false,
                error_is_null: true,
                error_str: &empty,
            }),
            ReplyValidation::NotObject
        );

        // Field count != 2 -> UnexpectedFieldCount (3)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: false,
                is_object: true,
                field_count: 3,
                has_error_key: true,
                has_result_key: true,
                error_is_null: true,
                error_str: &empty,
            }),
            ReplyValidation::UnexpectedFieldCount
        );

        // Missing error field -> MissingErrorField (4)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: false,
                is_object: true,
                field_count: 2,
                has_error_key: false,
                has_result_key: true,
                error_is_null: true,
                error_str: &empty,
            }),
            ReplyValidation::MissingErrorField
        );

        // Missing result field -> MissingResultField (5)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: false,
                is_object: true,
                field_count: 2,
                has_error_key: true,
                has_result_key: false,
                error_is_null: true,
                error_str: &empty,
            }),
            ReplyValidation::MissingResultField
        );

        // Has error string -> HasErrorString (6)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: false,
                is_object: true,
                field_count: 2,
                has_error_key: true,
                has_result_key: true,
                error_is_null: false,
                error_str: &err_msg,
            }),
            ReplyValidation::HasErrorString
        );

        // Ok -> Ok (0)
        assert_eq!(
            validate_reply_metadata(&ReplyMetadata {
                is_null_doc: false,
                is_object: true,
                field_count: 2,
                has_error_key: true,
                has_result_key: true,
                error_is_null: true,
                error_str: &empty,
            }),
            ReplyValidation::Ok
        );
    }

    #[test]
    fn test_reply_validation_json()
    {
        use crate::json::{JsonObject, JsonValue};

        assert_eq!(validate_reply_json(&JsonValue::Null), ReplyValidation::NotJson);
        assert_eq!(validate_reply_json(&JsonValue::Array(vec![])), ReplyValidation::NotObject);

        let mut obj = JsonObject::new();
        assert_eq!(validate_reply_json(&JsonValue::Object(obj.clone())), ReplyValidation::UnexpectedFieldCount);

        obj.insert(to_u16("foo"), JsonValue::Null);
        obj.insert(to_u16("bar"), JsonValue::Null);
        assert_eq!(validate_reply_json(&JsonValue::Object(obj.clone())), ReplyValidation::MissingErrorField);

        let mut obj2 = JsonObject::new();
        obj2.insert(to_u16("error"), JsonValue::Null);
        obj2.insert(to_u16("other"), JsonValue::Null);
        assert_eq!(validate_reply_json(&JsonValue::Object(obj2)), ReplyValidation::MissingResultField);

        let mut obj3 = JsonObject::new();
        obj3.insert(to_u16("error"), JsonValue::String(to_u16("some error")));
        obj3.insert(to_u16("result"), JsonValue::Null);
        assert_eq!(validate_reply_json(&JsonValue::Object(obj3)), ReplyValidation::HasErrorString);

        let mut obj4 = JsonObject::new();
        obj4.insert(to_u16("error"), JsonValue::Null);
        obj4.insert(to_u16("result"), JsonValue::Number(6.0));
        assert_eq!(validate_reply_json(&JsonValue::Object(obj4)), ReplyValidation::Ok);
    }

    #[test]
    fn test_check_version()
    {
        assert_eq!(check_ankiconnect_version(false, 6), VersionCheckResult::NotANumber);
        assert_eq!(
            check_ankiconnect_version(true, 5),
            VersionCheckResult::VersionTooLow { min_version: 6 }
        );
        assert_eq!(
            check_ankiconnect_version(true, 0),
            VersionCheckResult::VersionTooLow { min_version: 6 }
        );
        assert_eq!(check_ankiconnect_version(true, 6), VersionCheckResult::Ok);
        assert_eq!(check_ankiconnect_version(true, 7), VersionCheckResult::Ok);
    }

    #[test]
    fn test_format_endpoint_url()
    {
        assert_eq!(
            from_u16(&format_endpoint_url(&to_u16("http://127.0.0.1"), &to_u16("8765"))),
            "http://127.0.0.1:8765"
        );
        assert_eq!(
            from_u16(&format_endpoint_url(&to_u16("localhost"), &to_u16("8080"))),
            "localhost:8080"
        );
        assert_eq!(
            from_u16(&format_endpoint_url(&to_u16(""), &to_u16(""))),
            ":"
        );
    }

    #[test]
    fn test_interpret_can_add_notes()
    {
        assert_eq!(
            interpret_can_add_notes(&[]),
            CanAddNotesResult { first: false, second: false }
        );
        assert_eq!(
            interpret_can_add_notes(&[true]),
            CanAddNotesResult { first: true, second: false }
        );
        assert_eq!(
            interpret_can_add_notes(&[false]),
            CanAddNotesResult { first: false, second: false }
        );
        assert_eq!(
            interpret_can_add_notes(&[true, false]),
            CanAddNotesResult { first: true, second: false }
        );
        assert_eq!(
            interpret_can_add_notes(&[false, true]),
            CanAddNotesResult { first: false, second: true }
        );
        assert_eq!(
            interpret_can_add_notes(&[true, true]),
            CanAddNotesResult { first: true, second: true }
        );
        assert_eq!(
            interpret_can_add_notes(&[true, false, true]),
            CanAddNotesResult { first: true, second: false }
        );
    }

    #[test]
    fn test_validate_typed_result()
    {
        // 1. Reply error overrides all
        assert_eq!(
            validate_typed_result(TypedReplyKind::AddMedia, true, false, true, false, true),
            TypedReplyStatus::ReplyError
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestInt, true, true, false, false, false),
            TypedReplyStatus::ReplyError
        );

        // 2. AddMedia
        assert_eq!(
            validate_typed_result(TypedReplyKind::AddMedia, false, true, false, false, false),
            TypedReplyStatus::EmptySuccess
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::AddMedia, false, false, false, false, true),
            TypedReplyStatus::ResultNotArray
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::AddMedia, false, false, true, false, false),
            TypedReplyStatus::ElementTypeMismatch
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::AddMedia, false, false, true, false, true),
            TypedReplyStatus::Success
        );

        // 3. OpenBrowse
        assert_eq!(
            validate_typed_result(TypedReplyKind::OpenBrowse, false, true, false, false, false),
            TypedReplyStatus::EmptySuccess
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::OpenBrowse, false, false, false, false, true),
            TypedReplyStatus::ResultNotArray
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::OpenBrowse, false, false, true, false, false),
            TypedReplyStatus::ElementTypeMismatch
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::OpenBrowse, false, false, true, false, true),
            TypedReplyStatus::Success
        );

        // 4. RequestStringList (Note: element mismatch still yields Success!)
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestStringList, false, true, false, false, false),
            TypedReplyStatus::EmptySuccess
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestStringList, false, false, false, false, true),
            TypedReplyStatus::ResultNotArray
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestStringList, false, false, true, false, false),
            TypedReplyStatus::Success
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestStringList, false, false, true, false, true),
            TypedReplyStatus::Success
        );

        // 5. RequestBoolList
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestBoolList, false, true, false, false, false),
            TypedReplyStatus::EmptySuccess
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestBoolList, false, false, false, false, true),
            TypedReplyStatus::ResultNotArray
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestBoolList, false, false, true, false, false),
            TypedReplyStatus::ElementTypeMismatch
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestBoolList, false, false, true, false, true),
            TypedReplyStatus::Success
        );

        // 6. RequestInt
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestInt, false, true, false, false, false),
            TypedReplyStatus::EmptyError
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestInt, false, false, false, false, true),
            TypedReplyStatus::ResultNotDouble
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestInt, false, false, false, true, true),
            TypedReplyStatus::Success
        );
    }

    #[test]
    fn test_filter_json_strings()
    {
        let t1 = to_u16("first");
        let t2 = to_u16("second");
        let t3 = to_u16("third");

        let candidates = [
            JsonStringCandidate { is_string: true, text: &t1 },
            JsonStringCandidate { is_string: false, text: &[][..] },
            JsonStringCandidate { is_string: true, text: &t2 },
            JsonStringCandidate { is_string: false, text: &t3 },
        ];

        let filtered = filter_json_strings(&candidates);
        assert_eq!(filtered.len(), 2);
        assert_eq!(from_u16(filtered[0]), "first");
        assert_eq!(from_u16(filtered[1]), "second");

        let empty: [JsonStringCandidate<'_>; 0] = [];
        assert!(filter_json_strings(&empty).is_empty());
    }

    #[test]
    fn test_anki_payload_construction()
    {
        // 1. canAddNotes payload
        let mut note1 = crate::json::JsonObject::new();
        note1.insert(to_u16("deckName"), crate::json::JsonValue::String(to_u16("Default")));
        let mut note2 = crate::json::JsonObject::new();
        note2.insert(to_u16("deckName"), crate::json::JsonValue::String(to_u16("Japanese")));

        let can_add_payload = build_can_add_notes_payload(vec![note1.clone(), note2.clone()], None);
        assert_eq!(
            can_add_payload.get(&to_u16("action")),
            Some(&crate::json::JsonValue::String(to_u16("canAddNotes")))
        );
        assert_eq!(
            can_add_payload.get(&to_u16("version")),
            Some(&crate::json::JsonValue::Number(6.0))
        );
        assert_eq!(can_add_payload.get(&to_u16("key")), None);
        let params = can_add_payload.get(&to_u16("params")).and_then(|v| v.as_object()).unwrap();
        let notes_arr = params.get(&to_u16("notes")).and_then(|v| v.as_array()).unwrap();
        assert_eq!(notes_arr.len(), 2);
        assert_eq!(notes_arr[0], crate::json::JsonValue::Object(note1.clone()));
        assert_eq!(notes_arr[1], crate::json::JsonValue::Object(note2));

        // canAddNotes with apiKey
        let key = to_u16("secret123");
        let can_add_with_key = build_can_add_notes_payload(vec![note1.clone()], Some(&key));
        assert_eq!(
            can_add_with_key.get(&to_u16("key")),
            Some(&crate::json::JsonValue::String(to_u16("secret123")))
        );

        // 2. addNote payload
        let add_note_payload = build_add_note_payload(note1.clone(), None);
        assert_eq!(
            add_note_payload.get(&to_u16("action")),
            Some(&crate::json::JsonValue::String(to_u16("addNote")))
        );
        assert_eq!(
            add_note_payload.get(&to_u16("version")),
            Some(&crate::json::JsonValue::Number(6.0))
        );
        let note_params = add_note_payload.get(&to_u16("params")).and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            note_params.get(&to_u16("note")),
            Some(&crate::json::JsonValue::Object(note1))
        );

        // 3. guiBrowse payload
        let query = to_u16("\"deck:Default\" (expression:test)");
        let browse_payload = build_gui_browse_payload(&query, Some(&key));
        assert_eq!(
            browse_payload.get(&to_u16("action")),
            Some(&crate::json::JsonValue::String(to_u16("guiBrowse")))
        );
        assert_eq!(
            browse_payload.get(&to_u16("version")),
            Some(&crate::json::JsonValue::Number(6.0))
        );
        assert_eq!(
            browse_payload.get(&to_u16("key")),
            Some(&crate::json::JsonValue::String(to_u16("secret123")))
        );
        let browse_params = browse_payload.get(&to_u16("params")).and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            browse_params.get(&to_u16("query")),
            Some(&crate::json::JsonValue::String(query))
        );

        // 4. modelFieldNames payload
        let model = to_u16("Basic");
        let model_payload = build_model_field_names_payload(&model, None);
        assert_eq!(
            model_payload.get(&to_u16("action")),
            Some(&crate::json::JsonValue::String(to_u16("modelFieldNames")))
        );
        assert_eq!(
            model_payload.get(&to_u16("version")),
            Some(&crate::json::JsonValue::Number(6.0))
        );
        let model_params = model_payload.get(&to_u16("params")).and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            model_params.get(&to_u16("modelName")),
            Some(&crate::json::JsonValue::String(model))
        );
    }

    #[test]
    fn test_parse_reply_and_extract_error()
    {
        // Valid reply
        let mut valid_obj = crate::json::JsonObject::new();
        valid_obj.insert(to_u16("error"), crate::json::JsonValue::Null);
        valid_obj.insert(to_u16("result"), crate::json::JsonValue::Number(42.0));
        let valid_val = crate::json::JsonValue::Object(valid_obj);

        assert_eq!(
            parse_reply_json(&valid_val),
            Ok(&crate::json::JsonValue::Number(42.0))
        );
        assert_eq!(extract_reply_error(&valid_val), None);

        // Error reply
        let mut err_obj = crate::json::JsonObject::new();
        err_obj.insert(to_u16("error"), crate::json::JsonValue::String(to_u16("deck was not found")));
        err_obj.insert(to_u16("result"), crate::json::JsonValue::Null);
        let err_val = crate::json::JsonValue::Object(err_obj);

        assert_eq!(
            parse_reply_json(&err_val),
            Err(ReplyValidation::HasErrorString)
        );
        assert_eq!(
            extract_reply_error(&err_val).map(from_u16),
            Some("deck was not found".to_string())
        );

        // Missing fields
        let mut incomplete = crate::json::JsonObject::new();
        incomplete.insert(to_u16("error"), crate::json::JsonValue::Null);
        assert_eq!(
            parse_reply_json(&crate::json::JsonValue::Object(incomplete)),
            Err(ReplyValidation::UnexpectedFieldCount)
        );
    }
}


