// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use std::sync::{Mutex, OnceLock};

use freerdp_sys::{
    AUDIN_CHANNEL_NAME, BOOL, DWORD, FREERDP_LOAD_CHANNEL_ADDIN_ENTRY_FN, LPCSTR,
    PDEVICE_SERVICE_ENTRY_POINTS, PVIRTUALCHANNELENTRY, RDPSND_CHANNEL_NAME, UINT,
    freerdp_get_current_addin_provider, freerdp_register_addin_provider,
};

use super::{audio_input, audio_output, printer, smartcard, webcam};
use crate::utils::log;

static FREERDP_ADDIN_PROVIDER: OnceLock<FREERDP_LOAD_CHANNEL_ADDIN_ENTRY_FN> = OnceLock::new();

/// Serializes the (context_new + provider re-assert) sequence across sessions.
///
/// `freerdp_client_context_new` re-registers upstream's stock static-table
/// loader into a single process-global slot on EVERY context creation; only
/// afterwards do we re-install our policy-enforcing `custom_addin_provider`.
/// Without serialization, another session could load an addin in that window
/// and resolve through the stock loader, bypassing the gateway's policy. This
/// mutex makes the two-step sequence atomic with respect to other sessions'
/// context creation + re-assert, so the slot is never observed in the stock
/// state by a concurrent registration. (Addin loads themselves happen later,
/// during connect, by which point the custom provider is re-asserted.)
static PROVIDER_INSTALL_LOCK: Mutex<()> = Mutex::new(());

fn secure_cstr_from_lpcstr(psz: LPCSTR) -> String {
    if psz.is_null() {
        "<null>".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(psz).to_string_lossy().to_string() }
    }
}

unsafe extern "C" fn custom_addin_provider(
    psz_name: LPCSTR,
    psz_subsystem: LPCSTR,
    psz_type: LPCSTR,
    dw_flags: DWORD,
) -> PVIRTUALCHANNELENTRY {
    unsafe {
        let name = secure_cstr_from_lpcstr(psz_name);
        let subsystem = secure_cstr_from_lpcstr(psz_subsystem);

        log::debug!(
            "Addin provider called with name: '{}', subsystem: '{}', type: '{}', flags: {}",
            name,
            subsystem,
            secure_cstr_from_lpcstr(psz_type),
            dw_flags
        );

        // Setup our custom rdpsnd channel addin if requested, otherwise delegate to the original provider.
        // Fail closed (null) if the OnceLock is not yet initialized rather than
        // panicking: a panic on FreeRDP's extern "C" C-stack aborts the process.
        if let Some(freerdp_addin_provider) = FREERDP_ADDIN_PROVIDER.get().copied().flatten() {
            if name.as_bytes() == &RDPSND_CHANNEL_NAME[..RDPSND_CHANNEL_NAME.len() - 1]
                && subsystem == super::RDPSND_SUBSYSTEM_CUSTOM
            {
                log::info!("rdpsnd channel addin requested.");
                Some(std::mem::transmute::<
                    unsafe extern "C" fn(freerdp_sys::PFREERDP_RDPSND_DEVICE_ENTRY_POINTS) -> UINT,
                    unsafe extern "C" fn(*mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL,
                >(audio_output::sound_entry))
            } else if name.as_bytes() == &AUDIN_CHANNEL_NAME[..AUDIN_CHANNEL_NAME.len() - 1]
                && subsystem == super::AUDIN_SUBSYSTEM_CUSTOM
            {
                log::info!("audin channel addin requested.");
                Some(std::mem::transmute::<
                    unsafe extern "C" fn(freerdp_sys::PFREERDP_AUDIN_DEVICE_ENTRY_POINTS) -> UINT,
                    unsafe extern "C" fn(*mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL,
                >(audio_input::mic_entry))
            } else if name == "rdpecam"
                && (subsystem == super::WEBCAM_SUBSYSTEM_CUSTOM || psz_subsystem.is_null())
            {
                log::info!("rdpecam channel addin requested.");
                Some(std::mem::transmute::<
                    unsafe extern "C" fn(*mut freerdp_sys::IDRDYNVC_ENTRY_POINTS) -> UINT,
                    unsafe extern "C" fn(*mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL,
                >(webcam::webcam_entry))
            } else if name == "smartcard"
                && secure_cstr_from_lpcstr(psz_type) == "DeviceServiceEntry"
            {
                log::info!("smartcard DeviceServiceEntry intercepted via custom_addin_provider.");
                Some(std::mem::transmute::<
                    unsafe extern "C" fn(PDEVICE_SERVICE_ENTRY_POINTS) -> UINT,
                    unsafe extern "C" fn(*mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL,
                >(smartcard::device_service_entry))
            } else if name == "printer" && secure_cstr_from_lpcstr(psz_type) == "DeviceServiceEntry"
            {
                log::info!("printer DeviceServiceEntry intercepted via custom_addin_provider.");
                Some(std::mem::transmute::<
                    unsafe extern "C" fn(PDEVICE_SERVICE_ENTRY_POINTS) -> UINT,
                    unsafe extern "C" fn(*mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL,
                >(printer::device_service_entry))
            } else {
                log::debug!("Delegating to underlying addin provider");
                freerdp_addin_provider(psz_name, psz_subsystem, psz_type, dw_flags)
            }
        } else {
            log::error!("No underlying addin provider found.");
            None
        }
    }
}

pub fn register_channel_addin_loader() {
    // Save the upstream provider exactly once (the first call wins; later
    // calls must not overwrite it with our own custom provider).
    let _ = FREERDP_ADDIN_PROVIDER.set(unsafe { freerdp_get_current_addin_provider() });
    // Re-assert our policy-enforcing provider into the process-global slot.
    // Called under PROVIDER_INSTALL_LOCK (held by RdpContext::create across
    // context_new + this re-assert) so no concurrent session observes the
    // stock loader that freerdp_client_context_new just re-installed.
    unsafe {
        freerdp_register_addin_provider(Some(custom_addin_provider), 0);
    }
}

/// Acquire the global provider-install lock. `RdpContext::create` holds this
/// across `freerdp_client_context_new` + `register_channel_addin_loader` so the
/// (stock-loader re-register -> custom re-assert) sequence is atomic with
/// respect to other sessions' context creation.
pub(crate) fn lock_provider_install() -> std::sync::MutexGuard<'static, ()> {
    PROVIDER_INSTALL_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    //! Addin-provider contract (Fix N): the custom provider must dispatch our
    //! addins, delegate the rest, never panic on an uninitialized OnceLock,
    //! and registration must be idempotent (the saved upstream provider is
    //! captured once and never overwritten by our own custom provider).
    use super::*;
    use std::ffi::CString;

    /// A stand-in upstream provider that records it was called and returns a
    /// sentinel pointer (never dereferenced).
    unsafe extern "C" fn sentinel_entry(_ep: *mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL {
        0
    }

    unsafe extern "C" fn sentinel_provider(
        _name: LPCSTR,
        _subsystem: LPCSTR,
        _type: LPCSTR,
        _flags: DWORD,
    ) -> PVIRTUALCHANNELENTRY {
        Some(sentinel_entry)
    }

    fn cstr(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn register_is_idempotent_and_sticky() {
        // Seed the saved upstream provider with a known sentinel, then call
        // register twice. The OnceLock must keep the FIRST value (not be
        // overwritten by our custom provider on the second call).
        let _ = FREERDP_ADDIN_PROVIDER.set(Some(sentinel_provider));
        register_channel_addin_loader();
        register_channel_addin_loader();
        let saved = FREERDP_ADDIN_PROVIDER.get().copied().flatten();
        assert!(saved.is_some());
        // The saved provider must still be the sentinel, not custom_addin_provider.
        assert!(std::ptr::eq(
            saved.unwrap() as *const (),
            sentinel_provider as *const ()
        ));
    }

    #[test]
    fn custom_provider_dispatches_our_addins() {
        let _ = FREERDP_ADDIN_PROVIDER.set(Some(sentinel_provider));
        let rdpsnd = cstr("rdpsnd");
        let subsys = cstr(super::super::RDPSND_SUBSYSTEM_CUSTOM);
        let ty = cstr("");
        let entry =
            unsafe { custom_addin_provider(rdpsnd.as_ptr(), subsys.as_ptr(), ty.as_ptr(), 0) };
        assert!(entry.is_some(), "rdpsnd custom addin must be provided");

        let audin = cstr("audin");
        let asys = cstr(super::super::AUDIN_SUBSYSTEM_CUSTOM);
        let entry = unsafe { custom_addin_provider(audin.as_ptr(), asys.as_ptr(), ty.as_ptr(), 0) };
        assert!(entry.is_some(), "audin custom addin must be provided");

        let cam = cstr("rdpecam");
        let csys = cstr(super::super::WEBCAM_SUBSYSTEM_CUSTOM);
        let entry = unsafe { custom_addin_provider(cam.as_ptr(), csys.as_ptr(), ty.as_ptr(), 0) };
        assert!(entry.is_some(), "rdpecam custom addin must be provided");
    }

    #[test]
    fn custom_provider_delegates_unknown_to_upstream() {
        let _ = FREERDP_ADDIN_PROVIDER.set(Some(sentinel_provider));
        let name = cstr("some_other_channel");
        let subsys = cstr("whatever");
        let ty = cstr("");
        let entry =
            unsafe { custom_addin_provider(name.as_ptr(), subsys.as_ptr(), ty.as_ptr(), 0) };
        // Delegated to the sentinel provider (returns sentinel_entry).
        assert!(entry.is_some());
        assert!(std::ptr::eq(
            entry.unwrap() as *const (),
            sentinel_entry as *const ()
        ));
    }

    #[test]
    fn provider_install_lock_is_reentrant_across_calls() {
        // The lock must be acquirable and releasable repeatedly (serializes
        // context creation). Two sequential acquisitions must not deadlock.
        {
            let _g = lock_provider_install();
        }
        {
            let _g = lock_provider_install();
        }
    }
}
