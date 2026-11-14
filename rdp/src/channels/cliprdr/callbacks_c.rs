// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::utils::log;

use crate::{Rdp, channels::cliprdr::traits::ClipboardHandler, context::RdpContext};

/// Hard ceiling for a host-supplied clipboard FORMAT_DATA_RESPONSE payload.
/// The bytes are re-allocated ~2x Rust-side (UTF-16 -> u16 Vec -> String), so
/// an uncapped host length is a per-session resource-exhaustion vector.
/// 32 MiB of UTF-16 text (~16M chars) is far beyond any legitimate clipboard
/// paste; larger responses are discarded. Not configurable by design.
const MAX_CLIPBOARD_DATA_BYTES: usize = 32 * 1024 * 1024;

fn get_owner<'a>(context: *mut freerdp_sys::CliprdrClientContext) -> Option<&'a mut Rdp> {
    if context.is_null() {
        log::error!("CliprdrClientContext is null");
        return None;
    }

    let owner_ptr = unsafe { (*context).custom };
    if owner_ptr.is_null() {
        log::error!("CliprdrClientContext.custom (owner) is null");
        return None;
    }

    let rdp_context = unsafe { &mut *(owner_ptr as *mut RdpContext) };
    unsafe { rdp_context.owner.as_mut() }
}

unsafe extern "C" fn monitor_ready(
    context: *mut freerdp_sys::CliprdrClientContext,
    monitor_ready: *const freerdp_sys::CLIPRDR_MONITOR_READY,
) -> freerdp_sys::UINT {
    log::debug!(
        "Clipboard Monitor Ready callback called: context={:?}, monitor_ready={:?}",
        context,
        monitor_ready
    );
    if let Some(rdp) = get_owner(context) {
        return rdp.on_monitor_ready(unsafe { &*monitor_ready });
    }
    freerdp_sys::CHANNEL_RC_OK
}

unsafe extern "C" fn receive_server_capabilities(
    context: *mut freerdp_sys::CliprdrClientContext,
    capabilities: *const freerdp_sys::CLIPRDR_CAPABILITIES,
) -> freerdp_sys::UINT {
    log::debug!(
        "Clipboard Receive Server Capabilities callback called: context={:?}, capabilities={:?}",
        context,
        capabilities
    );
    if let Some(rdp) = get_owner(context) {
        return rdp.on_receive_server_capabilities(unsafe { &*capabilities });
    }
    freerdp_sys::CHANNEL_RC_OK
}

unsafe extern "C" fn receive_server_format_list(
    context: *mut freerdp_sys::CliprdrClientContext,
    format_list: *const freerdp_sys::CLIPRDR_FORMAT_LIST,
) -> freerdp_sys::UINT {
    log::debug!(
        "Clipboard Receive Server Format List callback called: context={:?}, format_list={:?}",
        context,
        format_list
    );
    if let Some(rdp) = get_owner(context) {
        return rdp.on_receive_server_format_list(unsafe { &*format_list });
    }
    freerdp_sys::CHANNEL_RC_OK
}

unsafe extern "C" fn receive_format_list_response(
    context: *mut freerdp_sys::CliprdrClientContext,
    format_list_response: *const freerdp_sys::CLIPRDR_FORMAT_LIST_RESPONSE,
) -> freerdp_sys::UINT {
    log::debug!(
        "Clipboard Receive Format List Response callback called: context={:?}, format_list_response={:?}",
        context,
        format_list_response
    );
    if let Some(rdp) = get_owner(context) {
        // If not failed, send success. No other data is needed.
        return rdp.on_receive_format_list_response(unsafe {
            (*format_list_response).common.msgFlags & freerdp_sys::CB_RESPONSE_FAIL as u16 == 0
        });
    }
    freerdp_sys::CHANNEL_RC_OK
}

// Invoked by serer to request clipboard data in specific format
unsafe extern "C" fn receive_format_data_request(
    context: *mut freerdp_sys::CliprdrClientContext,
    format_data_request: *const freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST,
) -> freerdp_sys::UINT {
    log::debug!(
        "Clipboard Receive Format Data Request callback called: context={:?}, format_data_request={:?}",
        context,
        format_data_request
    );
    if let Some(rdp) = get_owner(context) {
        return rdp.on_receive_format_data_request(unsafe { &*format_data_request });
    }
    freerdp_sys::CHANNEL_RC_OK
}

// Invoked by server to send clipboard data in response to a client format data request (sent by us)
unsafe extern "C" fn receive_format_data_response(
    context: *mut freerdp_sys::CliprdrClientContext,
    format_data_response: *const freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE,
) -> freerdp_sys::UINT {
    log::debug!(
        "Clipboard Receive Format Data Response callback called: context={:?}, format_data_response={:?}",
        context,
        format_data_response
    );
    // Note: Currently we only request text data, so we expect text here
    if let Some(rdp) = get_owner(context) {
        // MS-RDPEBC 2.2.2.4: CB_RESPONSE_FAIL means the server could not
        // render the requested format. Discard it and keep the local
        // clipboard state — never decode it as content (an empty/failed
        // response decoded as "" would wipe the local clipboard).
        let msg_flags = unsafe { (*format_data_response).common.msgFlags };
        if msg_flags & freerdp_sys::CB_RESPONSE_FAIL as u16 != 0 {
            log::warn!("Clipboard: server FORMAT_DATA_RESPONSE failed, discarding");
            return freerdp_sys::CHANNEL_RC_OK;
        }
        // Compose a const slice from buffer pointer and length
        let data = unsafe {
            if !(*format_data_response).requestedFormatData.is_null()
                && (*format_data_response).common.dataLen > 0
            {
                std::slice::from_raw_parts(
                    (*format_data_response).requestedFormatData,
                    (*format_data_response).common.dataLen as usize,
                )
            } else {
                &[]
            }
        };
        // Resource-exhaustion ceiling: the data is re-allocated ~2x Rust-side
        // (UTF-16 -> u16 Vec -> String). Cap the host-supplied length before
        // any allocation; an empty response is also dropped (no local wipe).
        if data.is_empty() {
            log::debug!("Clipboard: empty FORMAT_DATA_RESPONSE, keeping local clipboard");
            return freerdp_sys::CHANNEL_RC_OK;
        }
        if data.len() > MAX_CLIPBOARD_DATA_BYTES {
            log::warn!(
                "Clipboard: FORMAT_DATA_RESPONSE too large ({} bytes > {} cap), discarding",
                data.len(),
                MAX_CLIPBOARD_DATA_BYTES
            );
            return freerdp_sys::CHANNEL_RC_OK;
        }
        return rdp.on_receive_format_data_response(data);
    }
    freerdp_sys::CHANNEL_RC_OK
}

pub fn register_cliprdr_callbacks(cliprdr: &mut freerdp_sys::CliprdrClientContext) {
    cliprdr.MonitorReady = Some(monitor_ready);
    cliprdr.ServerCapabilities = Some(receive_server_capabilities);
    cliprdr.ServerFormatList = Some(receive_server_format_list);
    cliprdr.ServerFormatListResponse = Some(receive_format_list_response);
    cliprdr.ServerFormatDataRequest = Some(receive_format_data_request);
    cliprdr.ServerFormatDataResponse = Some(receive_format_data_response);
}

#[cfg(test)]
mod tests {
    //! FORMAT_DATA_RESPONSE ingest contract (Fix K): CB_RESPONSE_FAIL and
    //! empty responses are discarded (no local clipboard wipe), and oversized
    //! host payloads are capped before the ~2x Rust-side re-allocation.
    use super::*;
    use crate::integrations::clipboard::{ClipboardCallback, ClipboardIntegration};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct RecordingClipboard {
        set_texts: Mutex<Vec<String>>,
    }
    impl ClipboardIntegration for RecordingClipboard {
        fn start(&self, _cb: Arc<dyn ClipboardCallback>) {}
        fn stop(&self) {}
        fn set_text(&self, text: &str) -> anyhow::Result<()> {
            self.set_texts.lock().unwrap().push(text.to_string());
            Ok(())
        }
        fn get_text(&self) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    struct Fixture {
        rdp: *mut crate::Rdp,
        ctx: *mut RdpContext,
        clip_ctx: *mut freerdp_sys::CliprdrClientContext,
        clipboard: Arc<RecordingClipboard>,
    }
    impl Fixture {
        fn new() -> Self {
            let clipboard = Arc::new(RecordingClipboard {
                set_texts: Mutex::new(Vec::new()),
            });
            let integrations = crate::integrations::RdpIntegrations {
                audio_output: None,
                audio_input: None,
                webcam: None,
                clipboard: Some(clipboard.clone()),
                smartcard: None,
                printer: None,
            };
            let (update_tx, _rx) = flume::unbounded();
            let (rdp, _cmd) = crate::Rdp::new(
                crate::settings::RdpSettings::default(),
                update_tx,
                true,
                None,
                integrations,
            );
            let rdp = Box::into_raw(Box::new(rdp));
            let ctx = Box::into_raw(Box::new(RdpContext {
                common: unsafe { std::mem::zeroed() },
                owner: rdp,
            }));
            let clip_ctx = Box::into_raw(Box::new(unsafe {
                std::mem::zeroed::<freerdp_sys::CliprdrClientContext>()
            }));
            unsafe {
                (*clip_ctx).custom = ctx as *mut std::ffi::c_void;
            }
            Self {
                rdp,
                ctx,
                clip_ctx,
                clipboard,
            }
        }
        fn set_texts(&self) -> Vec<String> {
            self.clipboard.set_texts.lock().unwrap().clone()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                let _ = Box::from_raw(self.clip_ctx);
                let _ = Box::from_raw(self.ctx);
                let _ = Box::from_raw(self.rdp);
            }
        }
    }

    /// Build a response with the given flags and UTF-16LE payload.
    fn response(flags: u16, data: &[u8]) -> freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE {
        freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE {
            common: freerdp_sys::CLIPRDR_HEADER {
                msgType: 0,
                msgFlags: flags,
                dataLen: data.len() as u32,
            },
            requestedFormatData: if data.is_empty() {
                std::ptr::null()
            } else {
                data.as_ptr()
            },
        }
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect()
    }

    #[test]
    fn valid_text_is_set_to_local_clipboard() {
        let f = Fixture::new();
        let data = utf16("hello");
        let resp = response(freerdp_sys::CB_RESPONSE_OK as u16, &data);
        let rc = unsafe { receive_format_data_response(f.clip_ctx, &resp) };
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        assert_eq!(f.set_texts(), vec!["hello".to_string()]);
    }

    #[test]
    fn fail_response_is_discarded_no_wipe() {
        let f = Fixture::new();
        // FAIL with a non-empty payload: must NOT be decoded/set.
        let data = utf16("attacker");
        let resp = response(freerdp_sys::CB_RESPONSE_FAIL as u16, &data);
        let rc = unsafe { receive_format_data_response(f.clip_ctx, &resp) };
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        assert!(
            f.set_texts().is_empty(),
            "FAIL response must not touch clipboard"
        );
    }

    #[test]
    fn empty_response_is_discarded_no_wipe() {
        let f = Fixture::new();
        let resp = response(freerdp_sys::CB_RESPONSE_OK as u16, &[]);
        let rc = unsafe { receive_format_data_response(f.clip_ctx, &resp) };
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        assert!(
            f.set_texts().is_empty(),
            "empty response must not wipe clipboard"
        );
    }

    #[test]
    fn oversized_response_is_discarded() {
        let f = Fixture::new();
        // dataLen beyond the cap; pointer need not be valid because the cap
        // check uses the length and discards before reading the payload.
        let mut resp = response(freerdp_sys::CB_RESPONSE_OK as u16, &utf16("x"));
        resp.common.dataLen = (MAX_CLIPBOARD_DATA_BYTES + 1) as u32;
        let rc = unsafe { receive_format_data_response(f.clip_ctx, &resp) };
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        assert!(
            f.set_texts().is_empty(),
            "oversized response must be discarded"
        );
    }
}
