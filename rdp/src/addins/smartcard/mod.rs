// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Smartcard channel addin — intercepts the FreeRDP smartcard device service
//! via `custom_addin_provider` and routes all IRP requests to the
//! `SmartcardIntegration` trait.

mod consts;
mod device;
mod handlers;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use freerdp_sys::{CHANNEL_RC_OK, DEVICE, PDEVICE_SERVICE_ENTRY_POINTS, UINT};

use crate::context::OwnerFromCtx;
use crate::utils::log;

use consts::RDPDR_DTYP_SMARTCARD;
use device::{
    ContextEntry, IrpWork, OutstandingIrpTracker, SmartcardDevice, device_thread_main,
    free_handler, init_handler, irp_request_handler,
};

// ---------------------------------------------------------------------------
// DeviceServiceEntry
// ---------------------------------------------------------------------------

pub unsafe extern "C" fn device_service_entry(
    p_entry_points: PDEVICE_SERVICE_ENTRY_POINTS,
) -> UINT {
    if p_entry_points.is_null() {
        log::error!("smartcard DeviceServiceEntry called with null entry points");
        return CHANNEL_RC_OK;
    }

    log::info!("smartcard DeviceServiceEntry intercepted — setting up custom device");

    let entry_points = unsafe { &*p_entry_points };
    let rdp_context = entry_points.rdpcontext;

    let rdp = match rdp_context.owner() {
        Some(rdp) => rdp,
        None => {
            log::error!("smartcard: failed to recover Rdp owner from rdpContext");
            return CHANNEL_RC_OK;
        }
    };

    let integration = match &rdp.config.integrations.smartcard {
        Some(integration) => integration.clone(),
        None => {
            log::warn!(
                "smartcard: no SmartcardIntegration configured, skipping device registration"
            );
            return CHANNEL_RC_OK;
        }
    };

    if !integration.is_available() {
        log::warn!("smartcard: integration reports not available, skipping");
        return CHANNEL_RC_OK;
    }

    let (irp_tx, irp_rx) = flume::unbounded::<IrpWork>();
    let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
    let outstanding = Arc::new(OutstandingIrpTracker::new());

    let integration_clone = integration.clone();
    let contexts_clone = contexts.clone();
    let outstanding_clone = outstanding.clone();

    let device_thread = std::thread::Builder::new()
        .name("smartcard-device".to_string())
        .spawn(move || {
            device_thread_main(irp_rx, integration_clone, contexts_clone, outstanding_clone);
        })
        .expect("failed to spawn smartcard device thread");

    let mut device_box = Box::new(SmartcardDevice {
        device: unsafe { std::mem::zeroed() },
        integration,
        contexts,
        irp_tx,
        device_thread: Some(device_thread),
        outstanding,
    });

    device_box.device.type_ = RDPDR_DTYP_SMARTCARD;
    device_box.device.IRPRequest = Some(irp_request_handler);
    device_box.device.Free = Some(free_handler);
    device_box.device.Init = Some(init_handler);

    let name_str = "SCARD";
    let name_len = name_str.len();

    let name_c = std::ffi::CString::new(name_str).unwrap();
    let name_ptr = unsafe { freerdp_sys::_strdup(name_c.as_ptr()) };
    device_box.device.name = name_ptr;

    let data_stream = unsafe { freerdp_sys::Stream_New(std::ptr::null_mut(), name_len + 1) };
    if data_stream.is_null() {
        log::error!("smartcard: Stream_New failed for device.data");
        unsafe {
            freerdp_sys::free(name_ptr as *mut std::ffi::c_void);
        }
        return freerdp_sys::CHANNEL_RC_NO_MEMORY;
    }

    unsafe {
        let bytes = name_c.as_bytes_with_nul();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*data_stream).pointer, bytes.len());
        (*data_stream).pointer = (*data_stream).pointer.add(bytes.len());
        device_box.device.data = data_stream;
    }

    let register_fn = match entry_points.RegisterDevice {
        Some(f) => f,
        None => {
            log::error!("smartcard: RegisterDevice callback is null");
            unsafe {
                freerdp_sys::Stream_Free(data_stream, 1);
                freerdp_sys::free(name_ptr as *mut std::ffi::c_void);
            }
            return CHANNEL_RC_OK;
        }
    };

    let device_ptr = &mut device_box.device as *mut DEVICE;

    log::debug!(
        "smartcard: registering device {:p} with devman {:p}",
        device_ptr,
        entry_points.devman
    );

    let result = unsafe { register_fn(entry_points.devman, device_ptr) };

    if result == CHANNEL_RC_OK {
        let _ = Box::into_raw(device_box);
        log::info!("smartcard: device registered successfully");
    } else {
        log::error!("smartcard: RegisterDevice failed with code {}", result);
        unsafe {
            freerdp_sys::Stream_Free(data_stream, 1);
            freerdp_sys::free(name_ptr as *mut std::ffi::c_void);
        }
    }

    result
}
