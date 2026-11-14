// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

mod callbacks_c;
mod traits;

use crate::utils;

pub use callbacks_c::register_cliprdr_callbacks;
pub use traits::ClipboardHandler;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum RdpClipboardFormat {
    TextUnicode, // Unicode Text, UTF-16 LE
    TextAscii,   // ASCII Text, ANSI
    TextOem,     // OEM Text
    Image,
}

impl RdpClipboardFormat {
    pub fn from_format(format: &freerdp_sys::CLIPRDR_FORMAT) -> Option<Self> {
        Self::from_format_id(format.formatId)
    }

    pub fn from_format_id(format_id: u32) -> Option<Self> {
        match format_id {
            freerdp_sys::CF_UNICODETEXT => Some(RdpClipboardFormat::TextUnicode),
            freerdp_sys::CF_TEXT => Some(RdpClipboardFormat::TextAscii),
            freerdp_sys::CF_OEMTEXT => Some(RdpClipboardFormat::TextOem),
            freerdp_sys::CF_DIB => Some(RdpClipboardFormat::Image),
            _ => None,
        }
    }

    pub fn to_format_id(&self) -> freerdp_sys::UINT32 {
        match self {
            RdpClipboardFormat::TextUnicode => freerdp_sys::CF_UNICODETEXT,
            RdpClipboardFormat::TextAscii => freerdp_sys::CF_TEXT,
            RdpClipboardFormat::TextOem => freerdp_sys::CF_OEMTEXT,
            RdpClipboardFormat::Image => freerdp_sys::CF_DIB,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RdpClipboard {
    ptr: Option<utils::SafePtr<freerdp_sys::CliprdrClientContext>>,

    /// Liveness token shared by every clone of this handle. The registry
    /// (`clear_cliprdr`) flips it to false BEFORE the library frees the
    /// CliprdrClientContext, so any clone already handed to the consumer
    /// short-circuits its FFI calls instead of dereferencing freed memory.
    /// Fail-closed: a call on a stopped handle returns CHANNEL_RC_OK without
    /// touching the pointer.
    alive: Arc<AtomicBool>,

    pub caps_flags: u32,
    pub remote_formats: Vec<RdpClipboardFormat>, // Currently, we only support text format, but can be extended
    pub text: Arc<RwLock<String>>,               // Cached text from local clipboard
}

impl RdpClipboard {
    pub fn new(ptr: *mut freerdp_sys::CliprdrClientContext) -> Self {
        Self {
            ptr: utils::SafePtr::new(ptr),
            alive: Arc::new(AtomicBool::new(true)),
            caps_flags: 0, // Currently we dont use the caps_flags from remote server, we assume server supports text formats :)
            remote_formats: Vec::new(),
            text: Arc::new(RwLock::new(String::new())),
        }
    }

    /// Invalidate every clone of this handle. Called by the registry on
    /// channel disconnect, before the library frees the context. Idempotent.
    pub fn stop(&self) {
        self.alive.store(false, Ordering::Release);
    }

    /// Whether this handle may still touch the underlying context.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn client_capabilities(&self, capabilities: &freerdp_sys::CLIPRDR_CAPABILITIES) -> u32 {
        if !self.is_alive() {
            crate::utils::log::debug!("cliprdr: client_capabilities suppressed (channel stopped)");
            return freerdp_sys::CHANNEL_RC_OK;
        }
        if let Some(ptr) = &self.ptr
            && let Some(func) = ptr.ClientCapabilities
        {
            unsafe {
                func(
                    ptr.as_mut_ptr(),
                    capabilities as *const freerdp_sys::CLIPRDR_CAPABILITIES,
                )
            }
        } else {
            freerdp_sys::CHANNEL_RC_OK
        }
    }

    pub fn send_client_capabilities(&self, flags: u32) -> u32 {
        let general_capability_set = freerdp_sys::CLIPRDR_GENERAL_CAPABILITY_SET {
            capabilitySetType: freerdp_sys::CB_CAPSTYPE_GENERAL as u16,
            capabilitySetLength: std::mem::size_of::<freerdp_sys::CLIPRDR_GENERAL_CAPABILITY_SET>()
                as u16,
            version: freerdp_sys::CB_CAPS_VERSION_2,
            generalFlags: flags,
        };

        let capabilities = freerdp_sys::CLIPRDR_CAPABILITIES {
            common: freerdp_sys::CLIPRDR_HEADER {
                msgType: 0,
                msgFlags: 0,
                dataLen: 0,
            },

            cCapabilitiesSets: 1,
            capabilitySets: &general_capability_set as *const _ as *mut _,
        };
        self.client_capabilities(&capabilities)
    }

    // Internal helper to send client format list
    pub fn client_format_list(&self, format_list: &freerdp_sys::CLIPRDR_FORMAT_LIST) -> u32 {
        if !self.is_alive() {
            crate::utils::log::debug!("cliprdr: client_format_list suppressed (channel stopped)");
            return freerdp_sys::CHANNEL_RC_OK;
        }
        if let Some(ptr) = &self.ptr
            && let Some(func) = ptr.ClientFormatList
        {
            unsafe {
                func(
                    ptr.as_mut_ptr(),
                    format_list as *const freerdp_sys::CLIPRDR_FORMAT_LIST,
                )
            }
        } else {
            freerdp_sys::CHANNEL_RC_OK
        }
    }

    // Helper to send client format list but list is a slice of ClipboardFormat
    // When data is locally avaiable on clipboard, use this to send supported formats on the clipboard to server
    // Example. On a text editor press CTRL+C, "this" app receives the notificiation, obtains what formats are available on clipboard,
    // and calls this method to inform server about available formats. So the server also knows what formats are available to request data from.
    pub fn send_client_format_list(&self, formats: &[RdpClipboardFormat]) -> u32 {
        let mut cliprdr_formats: Vec<freerdp_sys::CLIPRDR_FORMAT> = Vec::new();

        for format in formats {
            cliprdr_formats.push(freerdp_sys::CLIPRDR_FORMAT {
                formatId: format.to_format_id(),
                formatName: std::ptr::null_mut(),
            });
        }

        let format_list = freerdp_sys::CLIPRDR_FORMAT_LIST {
            common: freerdp_sys::CLIPRDR_HEADER {
                msgType: 0,
                msgFlags: 0,
                dataLen: 0,
            },
            numFormats: cliprdr_formats.len() as u32,
            formats: cliprdr_formats.as_ptr() as *mut _,
        };

        self.client_format_list(&format_list)
    }

    // Helper to set send format list response to server and store text
    pub fn send_text_is_available(&self, text: &str) -> u32 {
        // Store locally
        {
            let mut local_text = self.text.write().unwrap();
            *local_text = text.to_string();
        }
        // Send format list to server
        self.send_client_format_list(&[RdpClipboardFormat::TextUnicode])
    }

    pub fn get_local_text(&self) -> String {
        let local_text = self.text.read().unwrap();
        local_text.clone()
    }

    pub fn send_format_list_response(&self, success: bool) -> u32 {
        if !self.is_alive() {
            crate::utils::log::debug!(
                "cliprdr: send_format_list_response suppressed (channel stopped)"
            );
            return freerdp_sys::CHANNEL_RC_OK;
        }
        if let Some(ptr) = &self.ptr
            && let Some(func) = ptr.ClientFormatListResponse
        {
            let response = freerdp_sys::CLIPRDR_FORMAT_LIST_RESPONSE {
                common: freerdp_sys::CLIPRDR_HEADER {
                    msgType: 0,
                    msgFlags: if success {
                        freerdp_sys::CB_RESPONSE_OK as u16
                    } else {
                        freerdp_sys::CB_RESPONSE_FAIL as u16
                    },
                    dataLen: 0,
                },
            };
            unsafe {
                func(
                    ptr.as_mut_ptr(),
                    &response as *const freerdp_sys::CLIPRDR_FORMAT_LIST_RESPONSE,
                )
            }
        } else {
            freerdp_sys::CHANNEL_RC_OK
        }
    }

    pub fn send_client_format_data_response(
        &self,
        format_data_response: &freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE,
    ) -> u32 {
        if !self.is_alive() {
            crate::utils::log::debug!(
                "cliprdr: send_client_format_data_response suppressed (channel stopped)"
            );
            return freerdp_sys::CHANNEL_RC_OK;
        }
        if let Some(ptr) = &self.ptr
            && let Some(func) = ptr.ClientFormatDataResponse
        {
            unsafe {
                func(
                    ptr.as_mut_ptr(),
                    format_data_response as *const freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE,
                )
            }
        } else {
            freerdp_sys::CHANNEL_RC_OK
        }
    }

    // Used to request clipboard data in specific format from server
    pub fn client_format_data_request(
        &self,
        format_data_request: &freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST,
    ) -> u32 {
        if !self.is_alive() {
            crate::utils::log::debug!(
                "cliprdr: client_format_data_request suppressed (channel stopped)"
            );
            return freerdp_sys::CHANNEL_RC_OK;
        }
        if let Some(ptr) = &self.ptr
            && let Some(func) = ptr.ClientFormatDataRequest
        {
            unsafe {
                func(
                    ptr.as_mut_ptr(),
                    format_data_request as *const freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST,
                )
            }
        } else {
            freerdp_sys::CHANNEL_RC_OK
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_format_id_unicode() {
        assert_eq!(
            RdpClipboardFormat::from_format_id(freerdp_sys::CF_UNICODETEXT),
            Some(RdpClipboardFormat::TextUnicode)
        );
    }

    #[test]
    fn from_format_id_text() {
        assert_eq!(
            RdpClipboardFormat::from_format_id(freerdp_sys::CF_TEXT),
            Some(RdpClipboardFormat::TextAscii)
        );
    }

    #[test]
    fn from_format_id_oem() {
        assert_eq!(
            RdpClipboardFormat::from_format_id(freerdp_sys::CF_OEMTEXT),
            Some(RdpClipboardFormat::TextOem)
        );
    }

    #[test]
    fn from_format_id_image() {
        assert_eq!(
            RdpClipboardFormat::from_format_id(freerdp_sys::CF_DIB),
            Some(RdpClipboardFormat::Image)
        );
    }

    #[test]
    fn from_format_id_unknown() {
        assert_eq!(RdpClipboardFormat::from_format_id(99999), None);
    }

    #[test]
    fn to_format_id_roundtrip() {
        for fmt in [
            RdpClipboardFormat::TextUnicode,
            RdpClipboardFormat::TextAscii,
            RdpClipboardFormat::TextOem,
            RdpClipboardFormat::Image,
        ] {
            let id = fmt.to_format_id();
            assert_eq!(RdpClipboardFormat::from_format_id(id), Some(fmt));
        }
    }

    #[test]
    fn test_clipboard_format_debug() {
        let fmt = RdpClipboardFormat::TextUnicode;
        let debug = format!("{:?}", fmt);
        assert!(debug.starts_with("TextUnicode"));
    }

    #[test]
    fn test_clipboard_format_clone_eq() {
        let a = RdpClipboardFormat::TextUnicode;
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_clipboard_format_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(RdpClipboardFormat::TextUnicode);
        set.insert(RdpClipboardFormat::TextUnicode);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_rdp_clipboard_new_null() {
        let cb = RdpClipboard::new(std::ptr::null_mut());
        assert!(cb.ptr.is_none());
        assert_eq!(cb.caps_flags, 0);
        assert!(cb.remote_formats.is_empty());
        assert_eq!(*cb.text.read().unwrap(), "");
    }

    #[test]
    fn test_rdp_clipboard_text_cache() {
        let cb = RdpClipboard::new(std::ptr::null_mut());
        assert_eq!(cb.get_local_text(), "");
        cb.send_text_is_available("hello clipboard");
        assert_eq!(cb.get_local_text(), "hello clipboard");
        cb.send_text_is_available("updated");
        assert_eq!(cb.get_local_text(), "updated");
    }

    #[test]
    fn test_rdp_clipboard_with_null_ptr_does_not_panic() {
        let cb = RdpClipboard::new(std::ptr::null_mut());
        assert_eq!(cb.send_client_capabilities(0), freerdp_sys::CHANNEL_RC_OK);
        assert_eq!(
            cb.send_client_format_list(&[RdpClipboardFormat::TextUnicode]),
            freerdp_sys::CHANNEL_RC_OK
        );
        assert_eq!(
            cb.send_format_list_response(true),
            freerdp_sys::CHANNEL_RC_OK
        );
    }
}

#[cfg(test)]
mod liveness_tests {
    //! Lifetime guarantee (Fix E): a clone of RdpClipboard handed to the
    //! consumer must not touch the CliprdrClientContext after the registry
    //! stops it (clear_cliprdr), because the library frees the context right
    //! after channel disconnect. Fail-closed: stopped clones return
    //! CHANNEL_RC_OK without invoking any callback.
    use super::*;
    use std::sync::Mutex;

    /// Per-test sink recovered from the fake context's `custom` pointer.
    struct CallSink {
        calls: Mutex<Vec<&'static str>>,
    }

    unsafe extern "C" fn rec_capabilities(
        ctx: *mut freerdp_sys::CliprdrClientContext,
        _caps: *const freerdp_sys::CLIPRDR_CAPABILITIES,
    ) -> u32 {
        let sink = unsafe { &*((*ctx).custom as *const CallSink) };
        sink.calls.lock().unwrap().push("capabilities");
        freerdp_sys::CHANNEL_RC_OK
    }
    unsafe extern "C" fn rec_format_list(
        ctx: *mut freerdp_sys::CliprdrClientContext,
        _fl: *const freerdp_sys::CLIPRDR_FORMAT_LIST,
    ) -> u32 {
        let sink = unsafe { &*((*ctx).custom as *const CallSink) };
        sink.calls.lock().unwrap().push("format_list");
        freerdp_sys::CHANNEL_RC_OK
    }
    unsafe extern "C" fn rec_format_list_response(
        ctx: *mut freerdp_sys::CliprdrClientContext,
        _r: *const freerdp_sys::CLIPRDR_FORMAT_LIST_RESPONSE,
    ) -> u32 {
        let sink = unsafe { &*((*ctx).custom as *const CallSink) };
        sink.calls.lock().unwrap().push("format_list_response");
        freerdp_sys::CHANNEL_RC_OK
    }
    unsafe extern "C" fn rec_format_data_request(
        ctx: *mut freerdp_sys::CliprdrClientContext,
        _r: *const freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST,
    ) -> u32 {
        let sink = unsafe { &*((*ctx).custom as *const CallSink) };
        sink.calls.lock().unwrap().push("format_data_request");
        freerdp_sys::CHANNEL_RC_OK
    }
    unsafe extern "C" fn rec_format_data_response(
        ctx: *mut freerdp_sys::CliprdrClientContext,
        _r: *const freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE,
    ) -> u32 {
        let sink = unsafe { &*((*ctx).custom as *const CallSink) };
        sink.calls.lock().unwrap().push("format_data_response");
        freerdp_sys::CHANNEL_RC_OK
    }

    struct FakeCtx {
        ctx: *mut freerdp_sys::CliprdrClientContext,
        sink: *mut CallSink,
    }
    impl FakeCtx {
        fn new() -> Self {
            let sink = Box::into_raw(Box::new(CallSink {
                calls: Mutex::new(Vec::new()),
            }));
            let ctx = Box::into_raw(Box::new(unsafe {
                std::mem::zeroed::<freerdp_sys::CliprdrClientContext>()
            }));
            unsafe {
                (*ctx).custom = sink as *mut std::ffi::c_void;
                (*ctx).ClientCapabilities = Some(rec_capabilities);
                (*ctx).ClientFormatList = Some(rec_format_list);
                (*ctx).ClientFormatListResponse = Some(rec_format_list_response);
                (*ctx).ClientFormatDataRequest = Some(rec_format_data_request);
                (*ctx).ClientFormatDataResponse = Some(rec_format_data_response);
            }
            Self { ctx, sink }
        }
        fn calls(&self) -> Vec<&'static str> {
            unsafe { (*self.sink).calls.lock().unwrap().clone() }
        }
    }
    impl Drop for FakeCtx {
        fn drop(&mut self) {
            unsafe {
                let _ = Box::from_raw(self.ctx);
                let _ = Box::from_raw(self.sink);
            }
        }
    }

    fn all_calls(cb: &RdpClipboard) {
        cb.send_client_capabilities(0);
        cb.send_client_format_list(&[RdpClipboardFormat::TextUnicode]);
        cb.send_format_list_response(true);
        let req = unsafe { std::mem::zeroed::<freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST>() };
        cb.client_format_data_request(&req);
        let resp = unsafe { std::mem::zeroed::<freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE>() };
        cb.send_client_format_data_response(&resp);
    }

    #[test]
    fn live_handle_invokes_callbacks() {
        let fake = FakeCtx::new();
        let cb = RdpClipboard::new(fake.ctx);
        assert!(cb.is_alive());
        all_calls(&cb);
        assert_eq!(
            fake.calls(),
            vec![
                "capabilities",
                "format_list",
                "format_list_response",
                "format_data_request",
                "format_data_response",
            ]
        );
    }

    #[test]
    fn stopped_clone_never_invokes_callbacks() {
        let fake = FakeCtx::new();
        let original = RdpClipboard::new(fake.ctx);
        // Clone handed to the consumer (like on_monitor_ready does).
        let consumer_clone = original.clone();
        assert!(consumer_clone.is_alive());

        // Registry disconnect: stop invalidates EVERY clone.
        original.stop();
        assert!(!consumer_clone.is_alive());
        assert!(!original.is_alive());

        all_calls(&consumer_clone);
        all_calls(&original);
        assert!(
            fake.calls().is_empty(),
            "a stopped clone must not touch the (freed) context"
        );
    }

    #[test]
    fn stop_is_idempotent_and_fail_closed() {
        let fake = FakeCtx::new();
        let cb = RdpClipboard::new(fake.ctx);
        cb.stop();
        cb.stop();
        assert_eq!(cb.send_client_capabilities(0), freerdp_sys::CHANNEL_RC_OK);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn registry_clear_cliprdr_invalidates_outstanding_clones() {
        let fake = FakeCtx::new();
        let mut channels = crate::channels::RdpChannels::new();
        channels.set_cliprdr_ptr(fake.ctx);
        let consumer_clone = channels.cliprdr().unwrap();
        assert!(consumer_clone.is_alive());

        // Disconnect path: clear_cliprdr must stop the handle before drop.
        channels.clear_cliprdr();
        assert!(!consumer_clone.is_alive());
        all_calls(&consumer_clone);
        assert!(
            fake.calls().is_empty(),
            "clear_cliprdr must invalidate clones already handed out"
        );
    }
}
