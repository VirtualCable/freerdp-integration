// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::utils::log;
use freerdp_sys::*;
use std::ffi::CStr;
use std::os::raw::c_char;

extern "C" fn my_message_cb(msg: *const wLogMessage) -> BOOL {
    if msg.is_null() {
        return 0;
    }
    let filename = if !unsafe { (*msg).FileName }.is_null() {
        unsafe { CStr::from_ptr((*msg).FileName as *const c_char).to_string_lossy() }
    } else {
        std::borrow::Cow::Borrowed("unknown")
    };

    let text_ptr = unsafe { (*msg).TextString };
    let text = if !text_ptr.is_null() {
        unsafe { CStr::from_ptr(text_ptr as *const c_char).to_string_lossy() }
    } else {
        std::borrow::Cow::Borrowed("")
    };

    unsafe {
        match (*msg).Level {
            WLOG_FATAL => log::error!(target: "freerdp", "FATAL: {}: {}", filename, text),
            WLOG_ERROR => log::error!(target: "freerdp", "{}: {}", filename, text),
            WLOG_WARN => log::warn!(target: "freerdp", "{}: {}", filename, text),
            WLOG_INFO => log::info!(target: "freerdp", "{}: {}", filename, text),
            WLOG_DEBUG => log::debug!(target: "freerdp", "{}: {}", filename, text),
            WLOG_TRACE => log::trace!(target: "freerdp", "{}: {}", filename, text),
            _ => log::info!(target: "freerdp", "{}: {}", filename, text),
        }
    };

    1 // TRUE
}

#[derive(Copy, Clone)]
pub enum WLogLevel {
    Fatal = WLOG_FATAL as isize,
    Error = WLOG_ERROR as isize,
    Warn = WLOG_WARN as isize,
    Info = WLOG_INFO as isize,
    Debug = WLOG_DEBUG as isize,
    Trace = WLOG_TRACE as isize,
}

pub fn set_wlog_level(tag: Option<&str>, level: WLogLevel) {
    unsafe {
        let log = match tag {
            Some(t) => {
                let c_tag = std::ffi::CString::new(t).unwrap();
                WLog_Get(c_tag.as_ptr())
            }
            None => WLog_GetRoot(),
        };
        if log.is_null() {
            log::error!("WLog_Get returned null for tag {:?}", tag);
            return;
        }
        WLog_SetLogLevel(log, level as u32);
    }
}

#[allow(clippy::manual_c_str_literals)]
pub fn setup_freerdp_logger(level: WLogLevel) {
    unsafe {
        let callbacks = wLogCallbacks {
            data: Some(my_message_cb),
            image: None,
            message: Some(my_message_cb),
            package: Some(my_message_cb),
        };

        let root = WLog_GetRoot();
        WLog_SetLogAppenderType(root, WLOG_APPENDER_CALLBACK);
        let appender = WLog_GetLogAppender(root);

        WLog_ConfigureAppender(
            appender,
            b"callbacks\0".as_ptr() as *const ::std::os::raw::c_char,
            &callbacks as *const _ as *mut _,
        );

        set_wlog_level(None, level);
        // set_wlog_level(Some("com.freerdp.utils.ringbuffer"), WLogLevel::Info);
        // set_wlog_level(Some("com.freerdp.primitives"), WLogLevel::Trace);
    }
}
