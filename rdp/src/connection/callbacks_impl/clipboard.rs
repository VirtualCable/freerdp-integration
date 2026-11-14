// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use std::collections::HashSet;
use std::sync::Arc;

use crate::utils::log;

use super::Rdp;

use crate::channels::cliprdr::{ClipboardHandler, RdpClipboard, RdpClipboardFormat};
use crate::integrations::ClipboardCallback;

struct RdpClipboardCallback {
    rdp_clipboard: RdpClipboard,
}

impl ClipboardCallback for RdpClipboardCallback {
    fn send_text_is_available(&self, text: &str) {
        self.rdp_clipboard.send_text_is_available(text);
    }
}

impl ClipboardHandler for Rdp {
    fn on_monitor_ready(&mut self, monitor_ready: &freerdp_sys::CLIPRDR_MONITOR_READY) -> u32 {
        log::debug!(
            "Clipboard Monitor Ready event received in Rdp impl: {:?}",
            monitor_ready
        );
        if let Some(context) = self.channels.read().unwrap().cliprdr() {
            // Server expect capabilities + initial clipboard status available
            context.send_client_capabilities(freerdp_sys::CB_USE_LONG_FORMAT_NAMES);

            if let Some(ref clipboard_integration) = self.config.integrations.clipboard {
                clipboard_integration.start(Arc::new(RdpClipboardCallback {
                    rdp_clipboard: context.clone(),
                }));
                // Always announce our formats (even with no text — e.g. an image on
                // the local clipboard). The server only starts pushing its own format
                // list once the client has sent an initial one, so otherwise
                // remote→local clipboard stops working until the local clipboard has
                // text (observed with an image in the local clipboard, 2026-08-09).
                if let Ok(text) = clipboard_integration.get_text()
                    && !text.is_empty()
                {
                    context.send_text_is_available(&text);
                } else {
                    context.send_client_format_list(&[RdpClipboardFormat::TextUnicode]);
                }
            }

            freerdp_sys::CHANNEL_RC_OK
        } else {
            log::error!("Clipboard context is null in Monitor Ready");
            freerdp_sys::CHANNEL_RC_TOO_MANY_CHANNELS
        }
    }

    fn on_receive_format_list_response(&mut self, success: bool) -> u32 {
        log::debug!(
            "Clipboard Receive Format List Response event received in Rdp impl: {:?}",
            success
        );
        if !success {
            log::warn!("Clipboard format list response: NOT OK");
        }

        freerdp_sys::CHANNEL_RC_OK
    }

    fn on_receive_server_capabilities(
        &mut self,
        capabilities: &freerdp_sys::CLIPRDR_CAPABILITIES,
    ) -> u32 {
        if let Some(mut context) = self.channels.write().unwrap().cliprdr() {
            log::debug!(
                "Clipboard Receive Server Capabilities event received in Rdp impl: {:?}",
                capabilities
            );
            let mut cap_set_ptr = capabilities.capabilitySets as *const u8;

            // Extract capability sets (a list with variable lengths)
            unsafe {
                for _ in 0..capabilities.cCapabilitiesSets {
                    let caps = cap_set_ptr as *const freerdp_sys::CLIPRDR_CAPABILITY_SET;
                    let cap_set = &*caps;
                    if cap_set.capabilitySetType == freerdp_sys::CB_CAPSTYPE_GENERAL as u16 {
                        let general_caps =
                            cap_set_ptr as *const freerdp_sys::CLIPRDR_GENERAL_CAPABILITY_SET;
                        let general_caps = &*general_caps;
                        log::debug!("General Capability Set: {:?}", general_caps);
                        context.caps_flags = general_caps.generalFlags;
                        break;
                    } else {
                        log::debug!("Other Capability Set Type: {:?}", cap_set);
                    }
                    cap_set_ptr = cap_set_ptr.add(cap_set.capabilitySetLength as usize);
                }
            }
        }

        freerdp_sys::CHANNEL_RC_OK
    }

    // This is called by server to inform client about available clipboard formats
    // on remote side. Our implementation should take into account only text formats for now.
    fn on_receive_server_format_list(
        &mut self,
        format_list: &freerdp_sys::CLIPRDR_FORMAT_LIST,
    ) -> u32 {
        if let Some(mut context) = self.channels.read().unwrap().cliprdr() {
            log::debug!(
                "Clipboard Receive Server Format List event received in Rdp impl: {:?}",
                format_list
            );
            context.remote_formats.clear();
            unsafe {
                let formats_ptr = format_list.formats as *const freerdp_sys::CLIPRDR_FORMAT;
                let mut supported: HashSet<RdpClipboardFormat> = HashSet::new();
                for i in 0..format_list.numFormats {
                    let format = &*formats_ptr.add(i as usize);
                    if let Some(clip_format) = RdpClipboardFormat::from_format(format) {
                        supported.insert(clip_format);
                    }
                }

                // Note: We are only interested right now on text formats
                context.remote_formats = supported.into_iter().collect(); // Convert HashSet to Vec

                log::debug!(
                    "Supported clipboard formats from remote: {:?}",
                    context.remote_formats
                );
            }
            context.send_format_list_response(!context.remote_formats.is_empty());
            // Request clipboard data in text format if available
            if context
                .remote_formats
                .contains(&RdpClipboardFormat::TextUnicode)
            {
                let format_data_request = freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST {
                    common: freerdp_sys::CLIPRDR_HEADER {
                        msgType: 0,
                        msgFlags: 0,
                        dataLen: std::mem::size_of::<freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST>()
                            as u32,
                    },
                    requestedFormatId: RdpClipboardFormat::TextUnicode.to_format_id(),
                };
                context.client_format_data_request(&format_data_request);
            }
        }
        freerdp_sys::CHANNEL_RC_OK
    }

    fn on_receive_format_data_request(
        &mut self,
        format_data_request: &freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST,
    ) -> u32 {
        if let Some(context) = self.channels.read().unwrap().cliprdr() {
            log::debug!(
                "Clipboard Receive Format Data Request event received in Rdp impl: {:?}",
                format_data_request
            );
            if let Some(format) =
                RdpClipboardFormat::from_format_id(format_data_request.requestedFormatId)
            {
                match format {
                    RdpClipboardFormat::TextUnicode => {
                        // Serve the ANNOUNCED snapshot, not a live re-read of
                        // the local clipboard: a host FORMAT_DATA_REQUEST must
                        // only receive what was announced (announce==serve
                        // invariant). Re-reading get_text() live would disclose
                        // un-announced local clipboard changes at host-chosen
                        // moments. The snapshot is written by
                        // send_text_is_available on announce.
                        let clipboard_text = context.get_local_text();
                        if clipboard_text.is_empty() {
                            // No text was announced: refuse the request rather
                            // than pulling live local content.
                            log::debug!(
                                "Clipboard: no announced text snapshot, refusing format data request"
                            );
                            let response_header = freerdp_sys::CLIPRDR_HEADER {
                                msgType: 0,
                                msgFlags: freerdp_sys::CB_RESPONSE_FAIL as u16,
                                dataLen: 0,
                            };
                            let format_data_response = freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE {
                                common: response_header,
                                requestedFormatData: std::ptr::null(),
                            };
                            return context.send_client_format_data_response(&format_data_response);
                        }

                        log::debug!("Providing announced clipboard text snapshot");

                        let mut text_bytes = clipboard_text.encode_utf16().collect::<Vec<u16>>();
                        text_bytes.push(0); // Add null terminator
                        let byte_len = (text_bytes.len() * 2) as u32;

                        let response_header = freerdp_sys::CLIPRDR_HEADER {
                            msgType: 0,
                            msgFlags: freerdp_sys::CB_RESPONSE_OK as u16,
                            dataLen: byte_len,
                        };

                        let format_data_response = freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE {
                            common: response_header,
                            requestedFormatData: text_bytes.as_ptr() as *mut u8,
                        };

                        context.send_client_format_data_response(&format_data_response);
                    }
                    _ => {
                        log::warn!("Requested clipboard format not supported");
                    }
                }
            } else {
                log::warn!(
                    "Requested clipboard format id {} not recognized",
                    format_data_request.requestedFormatId
                );
            }
        }
        freerdp_sys::CHANNEL_RC_OK
    }

    // This is called by server in response to our format data request. It contains the actual clipboard data.
    fn on_receive_format_data_response(&mut self, data: &[u8]) -> u32 {
        log::debug!(
            "Clipboard Receive Format Data Response event received in Rdp impl: {:?}",
            data
        );
        // Assume data is UTF-16LE encoded text
        if !data.len().is_multiple_of(2) {
            log::warn!("Received clipboard data length is not even, cannot be valid UTF-16");
            return freerdp_sys::CHANNEL_RC_OK;
        }
        let u16_data: Vec<u16> = data
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        // Now to String, trimming null terminator if present
        let u16_data_trimmed = match u16_data.iter().position(|&v| v == 0) {
            Some(pos) => &u16_data[..pos],
            None => &u16_data,
        };

        if let Ok(text) = String::from_utf16(u16_data_trimmed) {
            log::debug!("Received clipboard text data: {}", text);
            // Set to local clipboard
            if let Some(ref clipboard_integration) = self.config.integrations.clipboard
                && let Err(e) = clipboard_integration.set_text(&text)
            {
                log::error!("Failed to set local clipboard text: {}", e);
            }
        } else {
            log::warn!("Failed to decode received clipboard data as UTF-16");
        }

        freerdp_sys::CHANNEL_RC_OK
    }
}

#[cfg(test)]
mod announce_serve_tests {
    //! announce==serve invariant (Fix V): a host FORMAT_DATA_REQUEST must be
    //! served from the ANNOUNCED snapshot (RdpClipboard.text), never a live
    //! re-read of the local clipboard. With no announced text, the request is
    //! refused (CB_RESPONSE_FAIL) rather than pulling live local content.
    use super::*;
    use crate::integrations::clipboard::{ClipboardCallback, ClipboardIntegration};
    use std::sync::Mutex;

    /// Clipboard whose LIVE get_text() differs from the announced snapshot,
    /// so serving live vs snapshot is observable.
    #[derive(Debug)]
    struct LiveClipboard {
        live: Mutex<String>,
    }
    impl ClipboardIntegration for LiveClipboard {
        fn start(&self, _cb: Arc<dyn ClipboardCallback>) {}
        fn stop(&self) {}
        fn set_text(&self, text: &str) -> anyhow::Result<()> {
            *self.live.lock().unwrap() = text.to_string();
            Ok(())
        }
        fn get_text(&self) -> anyhow::Result<String> {
            Ok(self.live.lock().unwrap().clone())
        }
    }

    /// Fake CliprdrClientContext capturing the FormatDataResponse bytes.
    struct RespSink {
        responses: Mutex<Vec<(u16, Vec<u8>)>>, // (msgFlags, payload)
    }
    unsafe extern "C" fn rec_response(
        ctx: *mut freerdp_sys::CliprdrClientContext,
        resp: *const freerdp_sys::CLIPRDR_FORMAT_DATA_RESPONSE,
    ) -> u32 {
        let sink = unsafe { &*((*ctx).custom as *const RespSink) };
        let flags = unsafe { (*resp).common.msgFlags };
        let len = unsafe { (*resp).common.dataLen } as usize;
        let ptr = unsafe { (*resp).requestedFormatData };
        let bytes = if ptr.is_null() || len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
        };
        sink.responses.lock().unwrap().push((flags, bytes));
        freerdp_sys::CHANNEL_RC_OK
    }

    struct Fixture {
        rdp: *mut crate::Rdp,
        ctx: *mut freerdp_sys::CliprdrClientContext,
        sink: *mut RespSink,
        live: Arc<LiveClipboard>,
    }
    impl Fixture {
        fn new(live_text: &str) -> Self {
            let live = Arc::new(LiveClipboard {
                live: Mutex::new(live_text.to_string()),
            });
            let integrations = crate::integrations::RdpIntegrations {
                audio_output: None,
                audio_input: None,
                webcam: None,
                clipboard: Some(live.clone()),
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
            let sink = Box::into_raw(Box::new(RespSink {
                responses: Mutex::new(Vec::new()),
            }));
            let ctx = Box::into_raw(Box::new(unsafe {
                std::mem::zeroed::<freerdp_sys::CliprdrClientContext>()
            }));
            unsafe {
                (*ctx).custom = sink as *mut std::ffi::c_void;
                (*ctx).ClientFormatDataResponse = Some(rec_response);
            }
            rdp.channels.write().unwrap().set_cliprdr_ptr(ctx);
            let rdp = Box::into_raw(Box::new(rdp));
            Self {
                rdp,
                ctx,
                sink,
                live,
            }
        }
        fn responses(&self) -> Vec<(u16, Vec<u8>)> {
            unsafe { (*self.sink).responses.lock().unwrap().clone() }
        }
        fn request(&self) -> u32 {
            let req = freerdp_sys::CLIPRDR_FORMAT_DATA_REQUEST {
                common: unsafe { std::mem::zeroed() },
                requestedFormatId: freerdp_sys::CF_UNICODETEXT,
            };
            unsafe { (*self.rdp).on_receive_format_data_request(&req) }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                let _ = Box::from_raw(self.rdp);
                let _ = Box::from_raw(self.ctx);
                let _ = Box::from_raw(self.sink);
            }
        }
    }

    fn utf16_bytes(s: &str) -> Vec<u8> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v.iter().flat_map(|c| c.to_le_bytes()).collect()
    }

    #[test]
    fn serves_announced_snapshot_not_live() {
        let f = Fixture::new("LIVE-SECRET");
        // Announce a snapshot distinct from the live clipboard.
        unsafe {
            (*f.rdp)
                .channels
                .read()
                .unwrap()
                .cliprdr()
                .unwrap()
                .send_text_is_available("ANNOUNCED");
        }
        // Live clipboard changes AFTER the announce (must not be disclosed).
        *f.live.live.lock().unwrap() = "LIVE-CHANGED".to_string();
        let rc = f.request();
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        let resp = f.responses();
        assert_eq!(resp.len(), 1);
        assert_eq!(resp[0].0, freerdp_sys::CB_RESPONSE_OK as u16);
        assert_eq!(
            resp[0].1,
            utf16_bytes("ANNOUNCED"),
            "must serve the announced snapshot, not the live clipboard"
        );
    }

    #[test]
    fn no_announced_text_refuses_request() {
        let f = Fixture::new("LIVE-SECRET");
        // No announce: snapshot is empty.
        let rc = f.request();
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        let resp = f.responses();
        assert_eq!(resp.len(), 1);
        assert_eq!(
            resp[0].0,
            freerdp_sys::CB_RESPONSE_FAIL as u16,
            "with no announced text the request must be refused, not served live"
        );
        assert!(resp[0].1.is_empty());
    }
}
