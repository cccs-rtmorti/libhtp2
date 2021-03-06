#![deny(missing_docs)]
use crate::log::{HtpLogCode, Log};
use std::{ffi::CString, os::raw::c_char};

/// Get the log's message string
///
/// Returns the log message as a cstring or NULL on error
/// The caller must free this result with htp_free_cstring
#[no_mangle]
pub unsafe extern "C" fn htp_log_message(log: *const Log) -> *mut c_char {
    log.as_ref()
        .and_then(|log| CString::new(log.msg.msg.clone()).ok())
        .map(|msg| msg.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Get a log's message file
///
/// Returns the file as a cstring or NULL on error
/// The caller must free this result with htp_free_cstring
#[no_mangle]
pub unsafe extern "C" fn htp_log_file(log: *const Log) -> *mut c_char {
    log.as_ref()
        .and_then(|log| CString::new(log.msg.file.clone()).ok())
        .map(|msg| msg.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Get a log's message code
///
/// Returns a code or HTP_LOG_CODE_ERROR on error
#[no_mangle]
pub unsafe extern "C" fn htp_log_code(log: *const Log) -> HtpLogCode {
    log.as_ref()
        .map(|log| log.msg.code)
        .unwrap_or(HtpLogCode::ERROR)
}

/// Free log
#[no_mangle]
pub unsafe extern "C" fn htp_log_free(log: *mut Log) {
    if !log.is_null() {
        // log will be dropped when this box goes out of scope
        Box::from_raw(log);
    }
}
