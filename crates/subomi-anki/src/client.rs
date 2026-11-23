//! AnkiConnect protocol version and reply validation helpers.

/// Minimum supported AnkiConnect protocol version.
pub const MIN_ANKICONNECT_VERSION: i32 = 6;

/// Status codes returned when validating an AnkiConnect JSON reply envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyValidation {
    Ok,
    NotJson,
    NotObject,
    UnexpectedFieldCount,
    MissingErrorField,
    MissingResultField,
    HasErrorString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionCheckResult {
    Ok,
    NotANumber,
    VersionTooLow { min_version: i32 },
}

pub fn check_ankiconnect_version(result_is_number: bool, version_int: i32) -> VersionCheckResult {
    if !result_is_number {
        VersionCheckResult::NotANumber
    } else if version_int < MIN_ANKICONNECT_VERSION {
        VersionCheckResult::VersionTooLow {
            min_version: MIN_ANKICONNECT_VERSION,
        }
    } else {
        VersionCheckResult::Ok
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedReplyKind {
    AddMedia,
    OpenBrowse,
    RequestStringList,
    RequestBoolList,
    RequestInt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedReplyStatus {
    Success,
    EmptySuccess,
    ReplyError,
    ResultNotArray,
    ElementTypeMismatch,
    EmptyError,
    ResultNotDouble,
    InvalidKind,
}

/// Validates reply objects across `addMedia`, `openBrowse`,
/// `requestStringList`, `requestBoolList`, and `requestInt`.
pub fn validate_typed_result(
    kind: TypedReplyKind,
    has_reply_error: bool,
    reply_obj_empty: bool,
    result_is_array: bool,
    result_is_number: bool,
    all_elements_match_type: bool,
) -> TypedReplyStatus {
    if has_reply_error {
        return TypedReplyStatus::ReplyError;
    }
    match kind {
        TypedReplyKind::AddMedia | TypedReplyKind::OpenBrowse | TypedReplyKind::RequestBoolList => {
            if reply_obj_empty {
                TypedReplyStatus::EmptySuccess
            } else if !result_is_array {
                TypedReplyStatus::ResultNotArray
            } else if !all_elements_match_type {
                TypedReplyStatus::ElementTypeMismatch
            } else {
                TypedReplyStatus::Success
            }
        }
        TypedReplyKind::RequestStringList => {
            if reply_obj_empty {
                TypedReplyStatus::EmptySuccess
            } else if !result_is_array {
                TypedReplyStatus::ResultNotArray
            } else {
                TypedReplyStatus::Success
            }
        }
        TypedReplyKind::RequestInt => {
            if reply_obj_empty {
                TypedReplyStatus::EmptyError
            } else if !result_is_number {
                TypedReplyStatus::ResultNotDouble
            } else {
                TypedReplyStatus::Success
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_version() {
        assert_eq!(
            check_ankiconnect_version(false, 6),
            VersionCheckResult::NotANumber
        );
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
    fn test_validate_typed_result() {
        assert_eq!(
            validate_typed_result(TypedReplyKind::AddMedia, true, false, true, false, true),
            TypedReplyStatus::ReplyError
        );
        assert_eq!(
            validate_typed_result(TypedReplyKind::RequestInt, true, true, false, false, false),
            TypedReplyStatus::ReplyError
        );
        for kind in [
            TypedReplyKind::AddMedia,
            TypedReplyKind::OpenBrowse,
            TypedReplyKind::RequestBoolList,
        ] {
            assert_eq!(
                validate_typed_result(kind, false, true, false, false, false),
                TypedReplyStatus::EmptySuccess
            );
            assert_eq!(
                validate_typed_result(kind, false, false, false, false, true),
                TypedReplyStatus::ResultNotArray
            );
            assert_eq!(
                validate_typed_result(kind, false, false, true, false, false),
                TypedReplyStatus::ElementTypeMismatch
            );
            assert_eq!(
                validate_typed_result(kind, false, false, true, false, true),
                TypedReplyStatus::Success
            );
        }
        assert_eq!(
            validate_typed_result(
                TypedReplyKind::RequestStringList,
                false,
                true,
                false,
                false,
                false
            ),
            TypedReplyStatus::EmptySuccess
        );
        assert_eq!(
            validate_typed_result(
                TypedReplyKind::RequestStringList,
                false,
                false,
                false,
                false,
                true
            ),
            TypedReplyStatus::ResultNotArray
        );
        assert_eq!(
            validate_typed_result(
                TypedReplyKind::RequestStringList,
                false,
                false,
                true,
                false,
                false
            ),
            TypedReplyStatus::Success
        );
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
}
