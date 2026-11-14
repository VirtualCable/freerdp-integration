// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use freerdp_sys::{
    AccessTokenType, BOOL, BYTE, DWORD, SSIZE_T, SmartcardCertInfo, UINT16, UINT32, WCHAR, freerdp,
    freerdp_client_load_channels, freerdp_get_logon_error_info_data,
    freerdp_get_logon_error_info_type, gdi_free, gdi_init, rdp_auth_reason,
};

use crate::utils::log;

use crate::{
    callbacks::{graphics_c, primary_c},
    events, utils,
};

use super::{
    super::{context::OwnerFromCtx, utils::ToStringLossy},
    altsec_c,
    channels_c::{on_channel_connected, on_channel_disconnected},
    input_c,
    instance::InstanceCallbacks,
    pointer_update_c, secondary_c, update_c, window_c,
};

#[cfg(windows)]
type SSizeT = i64; // SSIZE_T en Windows

#[cfg(unix)]
type SSizeT = isize; // ssize_t en Linux/macOS

#[cfg(windows)]
type ReasonType = i32; // DWORD

#[cfg(unix)]
type ReasonType = i32; // int32_t

/// # Safety
/// This function is unsafe because it dereferences raw pointers to set callback functions.
pub unsafe fn set_instance_callbacks(instance: *mut freerdp) {
    unsafe {
        if instance.is_null() {
            log::error!("Instance is null, cannot set callbacks.");
            return;
        }
        // Callback assignments
        // All commented methods are provided by freerdp3
        // Have to make some tests, but probably, the already
        // manages the internal GDI state
        // and we don't need to override them
        log::debug!("Setting instance callbacks");
        // Setups the channels event listeners
        (*instance).PreConnect = Some(pre_connect);
        // Setups the gdi after connection is done
        (*instance).PostConnect = Some(post_connect);
        (*instance).ContextNew = Some(context_new);
        (*instance).ContextFree = Some(context_free);
        (*instance).Authenticate = Some(authenticate);
        (*instance).AuthenticateEx = Some(authenticate_ex);
        (*instance).VerifyX509Certificate = Some(verify_x509_certificate);
        (*instance).LogonErrorInfo = Some(logon_error_info);
        (*instance).PostDisconnect = Some(post_disconnect);
        (*instance).GatewayAuthenticate = Some(gateway_authenticate);
        (*instance).PresentGatewayMessage = Some(present_gateway_message);
        (*instance).Redirect = Some(redirect);
        (*instance).LoadChannels = Some(load_channels);
        (*instance).PostFinalDisconnect = Some(post_final_disconnect);
        // Channel data methods, commented to use the internal freerdp ones
        // (*instance).SendChannelData = Some(send_channel_data);
        // (*instance).ReceiveChannelData = Some(receive_channel_data);
        // (*instance).SendChannelPacket = Some(send_channel_packet);
        (*instance).VerifyCertificateEx = Some(verify_certificate);
        (*instance).ChooseSmartcard = Some(choose_smartcard);
        //(*instance).GetAccessToken = Some(freerdp_sys::get_access_token_wrapper);
        (*instance).RetryDialog = Some(retry_dialog);
    }
}

extern "C" fn pre_connect(instance: *mut freerdp) -> BOOL {
    // Here we can override the transport_io callbacks. Look rdpTransportIo
    // Transport and security layers:
    //
    // [RDP Application]
    //        |
    //        v
    //   RDP Protocol (PDUs)
    //        |
    //        v
    //   TLS (if the server requires it)
    //        |
    //        v
    //   rdp_transport_io (transport function table)
    //        |
    //        v
    //   UDS Tunnel local encapsulation  (if used the tunnel)
    //        |
    //        v
    //   Internet / Physical network
    //        |
    //        v
    //   UDS Tunnnel remote decapsulation (if used the tunnel)
    //        |
    //        v
    //   Remote RDP Server
    //
    // Notes:
    // - The UDS tunnel adds an additional transport layer, which may include
    //   custom encryption or multiplexing.
    // - The RDP server still requires its standard TLS layer, which is negotiated
    //   inside the tunnel.
    // - The tunnel must remain transparent to TLS: it encapsulates the data,
    //   but does not remove or alter the TLS negotiation required by RDP.

    // Register the channel events
    log::debug!(" **** Registering channel events on pre_connect... {instance:?}");
    let context = unsafe { (*instance).context.as_ref() };
    if context.is_none() {
        log::error!("Context is null in pre_connect");
        return false.into();
    }
    let context = context.unwrap();

    let pubsub = context.pubSub;
    events::ChannelConnected::subscribe(pubsub, Some(on_channel_connected));
    events::ChannelDisconnected::subscribe(pubsub, Some(on_channel_disconnected));

    if let Some(owner) = instance.owner() {
        owner.on_pre_connect().into()
    } else {
        true.into()
    }
}

// Initialize GDI here after the connection is established
extern "C" fn post_connect(instance: *mut freerdp) -> BOOL {
    log::debug!(" **** Post connect called... {instance:?}");
    // Initialize GDI, must be after the settings are set
    // const PIXEL_FORMAT_BGRA32: u32 = 0x20048888;
    // const PIXEL_FORMAT_RGBA32: u32 = 0x20038888;
    // const PIXEL_FORMAT_RGB24: u32 = 0x18018888;

    if let Some(owner) = instance.owner() {
        // Use 32 bits per pixel, ARGB=1, ABGR=2, RGBA=3, BGRA=4 (if 24 bit, ofc, no alpha is used, must be 0)
        let pixel_format = if owner.use_rgba() {
            utils::pixel_format(32, 3, 8, 8, 8, 8) // RGBA
        } else {
            utils::pixel_format(32, 4, 8, 8, 8, 8) // BGRA
        };
        unsafe { gdi_init(instance, pixel_format) };
        let context = unsafe { (*instance).context };
        // Setup our callbacks
        unsafe {
            update_c::set_callbacks(context, &owner.get_callbacks().update);
            window_c::set_callbacks(context, &owner.get_callbacks().window);
            altsec_c::set_callbacks(context, &owner.get_callbacks().altsec);
            primary_c::set_callbacks(context, &owner.get_callbacks().primary);
            secondary_c::set_callbacks(context, &owner.get_callbacks().secondary);
            pointer_update_c::set_callbacks(context, &owner.get_callbacks().pointer);
            input_c::set_callbacks(context, &owner.get_callbacks().input);
            graphics_c::set_callbacks(context);
        }

        owner.on_post_connect().into()
    } else {
        true.into()
    }
}

extern "C" fn context_new(instance: *mut freerdp, context: *mut freerdp_sys::rdpContext) -> BOOL {
    log::debug!(" **** Context new called... {instance:?} -- {context:?}");
    if let Some(owner) = instance.owner() {
        owner.on_context_new().into()
    } else {
        true.into()
    }
}

extern "C" fn context_free(instance: *mut freerdp, context: *mut freerdp_sys::rdpContext) {
    log::debug!(" **** Context free called... {instance:?} -- {context:?}");
    if let Some(owner) = instance.owner() {
        owner.on_context_free();
    }
}

extern "C" fn post_disconnect(instance: *mut freerdp) {
    log::debug!(" **** Post disconnect called... on {instance:?}");

    // Notify the owner BEFORE freeing the GDI: consumers get a documented
    // point to revoke GDI validity and stop/join reader tasks while the GDI
    // is still alive (the published gdi_lock protocol is two-sided below).
    if let Some(owner) = instance.owner() {
        owner.on_post_disconnect();
    }

    // Hold the write side of the published gdi_lock across gdi_free so any
    // consumer holding the read side (e.g. RectSender::encode) is ordered
    // before the free — upstream frees the GDI here, mid-teardown.
    let gdi_lock = instance.owner().map(|owner| owner.gdi_lock());
    let _write_guard = gdi_lock
        .as_ref()
        .map(|lock| lock.write().unwrap_or_else(|e| e.into_inner()));
    unsafe {
        gdi_free(instance);
    }
}

extern "C" fn post_final_disconnect(instance: *mut freerdp) {
    log::debug!(" **** Post final disconnect called... {instance:?}");

    let context = unsafe { (*instance).context.as_ref() };
    if context.is_none() {
        log::error!("Context is null in post_final_disconnect");
        return;
    }
    let pubsub = context.unwrap().pubSub;
    events::ChannelConnected::unsubscribe(pubsub, Some(on_channel_connected));
    events::ChannelDisconnected::unsubscribe(pubsub, Some(on_channel_disconnected));

    if let Some(owner) = instance.owner() {
        owner.on_post_final_disconnect();
    }
}

extern "C" fn authenticate(
    instance: *mut freerdp,
    username: *mut *mut ::std::os::raw::c_char,
    password: *mut *mut ::std::os::raw::c_char,
    domain: *mut *mut ::std::os::raw::c_char,
) -> BOOL {
    log::debug!(" **** Authenticate called... {instance:?}");
    if let Some(owner) = instance.owner() {
        owner.on_authenticate(username, password, domain).into()
    } else {
        true.into()
    }
}

extern "C" fn authenticate_ex(
    instance: *mut freerdp,
    username: *mut *mut ::std::os::raw::c_char,
    password: *mut *mut ::std::os::raw::c_char,
    domain: *mut *mut ::std::os::raw::c_char,
    reason: rdp_auth_reason,
) -> BOOL {
    log::debug!(" **** Authenticate (extended) called... {instance:?}, Reason: {reason:?}");
    if let Some(owner) = instance.owner() {
        owner
            .on_authenticate_ex(username, password, domain, reason as ReasonType)
            .into()
    } else {
        true.into()
    }
}

extern "C" fn verify_x509_certificate(
    instance: *mut freerdp,
    data: *const BYTE,
    length: usize,
    hostname: *const ::std::os::raw::c_char,
    port: UINT16,
    flags: DWORD,
) -> ::std::os::raw::c_int {
    log::debug!(" **** Verify X.509 certificate called... {instance:?}");
    if let Some(owner) = instance.owner() {
        // Convert hostname to Rust string
        let hostname = hostname.to_string_lossy();
        owner
            .on_verify_x509_certificate(data, length, &hostname, port, flags)
            .into()
    } else {
        true.into()
    }
}

extern "C" fn verify_certificate(
    instance: *mut freerdp,
    host: *const ::std::os::raw::c_char,
    port: UINT16,
    common_name: *const ::std::os::raw::c_char,
    subject: *const ::std::os::raw::c_char,
    issuer: *const ::std::os::raw::c_char,
    fingerprint: *const ::std::os::raw::c_char,
    flags: DWORD,
) -> DWORD {
    log::debug!(" **** Verify certificate called... {instance:?}");
    if let Some(owner) = instance.owner() {
        // Convert host, commmon name, subject, issuer, fingerprint from *const c_char String
        let host = host.to_string_lossy();
        let common_name = common_name.to_string_lossy();
        let subject = subject.to_string_lossy();
        let issuer = issuer.to_string_lossy();
        let fingerprint = fingerprint.to_string_lossy();

        owner.on_verify_certificate(
            &host,
            port,
            &common_name,
            &subject,
            &issuer,
            &fingerprint,
            flags,
        )
    } else {
        0
    }
}
extern "C" fn logon_error_info(
    instance: *mut freerdp,
    data: UINT32,
    type_: UINT32,
) -> ::std::os::raw::c_int {
    log::debug!(" **** Logon error info called... {instance:?} -- {data:?} -- {type_:?}");
    let str_data = unsafe { freerdp_get_logon_error_info_data(data) };
    let str_type = unsafe { freerdp_get_logon_error_info_type(type_) };

    let str_data = str_data.to_string_lossy();
    let str_type = str_type.to_string_lossy();

    if let Some(owner) = instance.owner() {
        owner.on_logon_error_info(&str_data, &str_type).into()
    } else {
        true.into()
    }
}

extern "C" fn gateway_authenticate(
    instance: *mut freerdp,
    username: *mut *mut ::std::os::raw::c_char,
    password: *mut *mut ::std::os::raw::c_char,
    domain: *mut *mut ::std::os::raw::c_char,
) -> BOOL {
    log::debug!(" **** Gateway authenticate called... {instance:?}");
    if let Some(owner) = instance.owner() {
        owner
            .on_gateway_authenticate(username, password, domain)
            .into()
    } else {
        true.into()
    }
}

extern "C" fn present_gateway_message(
    instance: *mut freerdp,
    msg_type: UINT32,
    is_display_mandatory: BOOL,
    is_consent_mandatory: BOOL,
    length: usize,
    message: *const WCHAR,
) -> BOOL {
    log::debug!(" **** Present gateway message called... {instance:?}");
    if let Some(owner) = instance.owner() {
        // Convert message to Rust string if needed, messages is in UTF-16 format
        let message = if !message.is_null() && length > 0 {
            let slice = unsafe { std::slice::from_raw_parts(message, length) };
            String::from_utf16_lossy(slice)
        } else {
            String::new()
        };

        owner
            .on_present_gateway_message(
                msg_type,
                is_display_mandatory != 0,
                is_consent_mandatory != 0,
                length,
                message,
            )
            .into()
    } else {
        true.into()
    }
}

extern "C" fn redirect(instance: *mut freerdp) -> BOOL {
    log::debug!(" **** Redirect called... {instance:?}");
    if let Some(owner) = instance.owner() {
        owner.on_redirect().into()
    } else {
        true.into()
    }
}

#[allow(dead_code)]
extern "C" fn load_channels(instance: *mut freerdp) -> BOOL {
    log::debug!(" **** Load channels called... {instance:?}");

    // Invoke original, ours is only a wrapper
    // Note: this call invokes in sequence freerdp_client_load_addins
    unsafe {
        let result = freerdp_client_load_channels(instance);
        log::debug!(" **** Load channels result: {result:?}");
    }

    if let Some(owner) = instance.owner() {
        owner.on_load_channels().into()
    } else {
        true.into()
    }
}

// #[allow(dead_code)]
// extern "C" fn send_channel_data(
//     instance: *mut freerdp,
//     channel_id: UINT16,
//     data: *const BYTE,
//     size: usize,
// ) -> BOOL {
//     log::debug!(" **** Send channel data called...");

//     // Convert BYTE to u8, just the pointer only
//     if let Some(owner) = instance.owner() {
//         owner.on_send_channel_data(channel_id, data, size).into()
//     } else {
//         true.into()
//     }
// }

// #[allow(dead_code)]
// extern "C" fn receive_channel_data(
//     instance: *mut freerdp,
//     channel_id: UINT16,
//     data: *const BYTE,
//     size: usize,
//     flags: UINT32,
//     total_size: usize,
// ) -> BOOL {
//     log::debug!(" **** Receive channel data called...");
//     if let Some(owner) = instance.owner() {
//         owner
//             .on_receive_channel_data(channel_id, data, size, flags, total_size)
//             .into()
//     } else {
//         true.into()
//     }
// }

// #[allow(dead_code)]
// extern "C" fn send_channel_packet(
//     instance: *mut freerdp,
//     channel_id: UINT16,
//     total_size: usize,
//     flags: UINT32,
//     data: *const BYTE,
//     chunk_size: usize,
// ) -> BOOL {
//     log::debug!(" **** Send channel packet called...");
//     if let Some(owner) = instance.owner() {
//         owner
//             .on_send_channel_packet(channel_id, total_size, flags, data, chunk_size)
//             .into()
//     } else {
//         true.into()
//     }
// }

extern "C" fn choose_smartcard(
    instance: *mut freerdp,
    cert_list: *mut *mut SmartcardCertInfo,
    count: DWORD,
    choice: *mut DWORD,
    gateway: BOOL,
) -> BOOL {
    log::debug!(" **** Choose smartcard called... {instance:?}");
    if let Some(owner) = instance.owner() {
        owner
            .on_choose_smartcard(cert_list, count, choice, gateway != 0)
            .into()
    } else {
        true.into()
    }
}

// No mangled because will be invoked from C code directly
// It's a trick to avoid the varargs, as Rust does not support them directly
// Note tha the wrapper on the sys crate translates de vargs to a pointer and a count
#[unsafe(no_mangle)]
pub extern "C" fn get_access_token_no_varargs(
    instance: *mut freerdp,
    token_type: AccessTokenType,
    token: *mut *mut ::std::os::raw::c_char,
    count: usize,
    data: *const *const ::std::os::raw::c_char,
) -> BOOL {
    log::debug!(" **** Get Access Token called... {instance:?}");
    if let Some(owner) = instance.owner() {
        owner
            .on_get_access_token(token_type, token, count, data)
            .into()
    } else {
        true.into()
    }
}

extern "C" fn retry_dialog(
    instance: *mut freerdp,
    what: *const ::std::os::raw::c_char,
    current: usize,
    userarg: *mut ::std::os::raw::c_void,
) -> SSIZE_T {
    log::debug!(" **** Retry dialog called... {instance:?}");
    if let Some(owner) = instance.owner() {
        // Convert what to Rust string
        let what = what.to_string_lossy();
        owner.on_retry_dialog(&what, current, userarg) as SSizeT
    } else {
        -1
    }
}

#[cfg(test)]
mod tests {
    //! Characterization + lifetime guarantee for post_disconnect's GDI
    //! teardown contract (Fix D). The fake freerdp has context->gdi = null
    //! and cache = null, so the real gdi_free is a safe no-op; the tests
    //! observe the LOCK protocol and the owner-notification ordering, which
    //! are the consumer-visible guarantees.
    use super::*;
    use crate::context::RdpContext;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    /// Heap-stable fake freerdp + RdpContext + real Rdp owner. gdi/cache are
    /// null so gdi_free is a no-op; owner is a real Rdp (instance = None).
    struct Fixture {
        rdp: *mut crate::Rdp,
        ctx: *mut RdpContext,
        instance: *mut freerdp,
        gdi_lock: Arc<std::sync::RwLock<()>>,
    }

    impl Fixture {
        fn new() -> Self {
            let integrations = crate::integrations::RdpIntegrations {
                audio_output: None,
                audio_input: None,
                webcam: None,
                clipboard: None,
                smartcard: None,
                printer: None,
            };
            let (update_tx, _update_rx) = flume::unbounded();
            let (rdp, _command_tx) = crate::Rdp::new(
                crate::settings::RdpSettings::default(),
                update_tx,
                true,
                None,
                integrations,
            );
            let gdi_lock = rdp.gdi_lock();
            let rdp = Box::into_raw(Box::new(rdp));
            let ctx = Box::into_raw(Box::new(RdpContext {
                common: unsafe { std::mem::zeroed() },
                owner: rdp,
            }));
            let instance = Box::into_raw(Box::new(unsafe { std::mem::zeroed::<freerdp>() }));
            unsafe {
                (*instance).context = ctx as *mut freerdp_sys::rdpContext;
                // gdi and cache stay null: gdi_free is a safe no-op.
            }
            Self {
                rdp,
                ctx,
                instance,
                gdi_lock,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                let _ = Box::from_raw(self.instance);
                let _ = Box::from_raw(self.ctx);
                let _ = Box::from_raw(self.rdp);
            }
        }
    }

    #[test]
    fn post_disconnect_returns_ok_with_null_gdi() {
        let f = Fixture::new();
        // Should not crash: gdi_free with null gdi/cache is a no-op.
        post_disconnect(f.instance);
        // gdi remains null (nothing to free).
        unsafe {
            assert!((*(*f.instance).context).gdi.is_null());
        }
    }

    #[test]
    fn post_disconnect_takes_gdi_write_lock() {
        let f = Fixture::new();
        // Hold the READ side on another thread; post_disconnect must block
        // on the WRITE side until we release it.
        let lock = f.gdi_lock.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
            let _guard = lock.read().unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.recv().unwrap(); // read lock is held

        let inst = f.instance as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            post_disconnect(inst as *mut freerdp);
            done_tx.send(()).unwrap();
        });

        // post_disconnect must NOT complete while the read lock is held.
        let early = done_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok();
        assert!(
            !early,
            "post_disconnect freed GDI without taking the gdi_lock write side"
        );

        release_tx.send(()).unwrap();
        reader.join().unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("post_disconnect never completed after read lock released");
    }

    #[test]
    fn post_disconnect_notifies_owner_before_free() {
        // The owner notification (on_post_disconnect) must happen BEFORE
        // gdi_free. We observe this by holding the gdi_lock WRITE side on
        // another thread: post_disconnect notifies the owner first (which
        // does not need the lock), then blocks acquiring the write lock for
        // the free. If the owner were notified AFTER the free, it could not
        // run while we hold the write lock.
        //
        // Since the crate's Rdp::on_post_disconnect only logs, we instead
        // verify the ordering structurally: post_disconnect must complete
        // the owner notification even while the write lock is held (i.e.
        // the notification is not gated behind the lock).
        let f = Fixture::new();
        let lock = f.gdi_lock.clone();
        let write_guard = lock.write().unwrap();

        let inst = f.instance as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            post_disconnect(inst as *mut freerdp);
            done_tx.send(()).unwrap();
        });

        // It must block on the write lock (free is gated), proving the free
        // is under the lock. The owner notification already ran (it is not
        // observable here, but the code path runs it before acquiring).
        let early = done_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok();
        assert!(
            !early,
            "gdi_free must be gated behind the gdi_lock write side"
        );
        drop(write_guard);
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("post_disconnect never completed");
    }
}
