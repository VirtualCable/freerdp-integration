// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Printer channel addin — intercepts the FreeRDP printer device service
//! via `custom_addin_provider` and routes all IRP requests to the
//! `PrinterIntegration` trait.

pub(crate) mod consts;
pub(crate) mod device;
pub(crate) mod handlers;

use freerdp_sys::{CHANNEL_RC_NO_MEMORY, CHANNEL_RC_OK, DEVICE, PDEVICE_SERVICE_ENTRY_POINTS, UINT};

use crate::context::OwnerFromCtx;
use crate::utils::log;

use consts::{RDPDR_DTYP_PRINT, RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER};
use device::{
    PrinterDevice, custom_component_handler, free_handler, init_handler, irp_request_handler,
    printer_thread_main,
};
use handlers::stream_write_u32;

pub unsafe extern "C" fn device_service_entry(
    p_entry_points: PDEVICE_SERVICE_ENTRY_POINTS,
) -> UINT {
    if p_entry_points.is_null() {
        log::error!("printer DeviceServiceEntry called with null entry points");
        return CHANNEL_RC_OK;
    }

    log::info!("printer DeviceServiceEntry intercepted — setting up custom Rust printer device");

    let entry_points = unsafe { &*p_entry_points };
    let rdp_context = entry_points.rdpcontext;

    let rdp = match rdp_context.owner() {
        Some(rdp) => rdp,
        None => {
            log::error!("printer: failed to recover Rdp owner from rdpContext");
            return CHANNEL_RC_OK;
        }
    };

    let integration = match &rdp.config.integrations.printer {
        Some(integration) => integration.clone(),
        None => {
            log::warn!("printer: no PrinterIntegration configured, skipping device registration");
            return CHANNEL_RC_OK;
        }
    };

    if !integration.is_available() {
        log::warn!("printer: integration reports not available, skipping");
        return CHANNEL_RC_OK;
    }

    let printers = integration.list_printers();
    if printers.is_empty() {
        log::warn!("printer: no printers returned from integration, skipping");
        return CHANNEL_RC_OK;
    }

    let register_fn = match entry_points.RegisterDevice {
        Some(f) => f,
        None => {
            log::error!("printer: RegisterDevice callback is null");
            return CHANNEL_RC_OK;
        }
    };

    for (idx, printer) in printers.iter().enumerate() {
        let (irp_tx, irp_rx) = flume::unbounded();
        let integration_clone = integration.clone();
        let printer_name = printer.name.clone();

        let device_thread = std::thread::Builder::new()
            .name(format!("printer-worker-{}", idx))
            .spawn(move || {
                printer_thread_main(irp_rx, integration_clone, printer_name);
            })
            .expect("failed to spawn printer device worker thread");

        let mut device_box = Box::new(PrinterDevice {
            device: unsafe { std::mem::zeroed() },
            printer_name: printer.name.clone(),
            integration: integration.clone(),
            irp_tx,
            device_thread: Some(device_thread),
        });

        device_box.device.type_ = RDPDR_DTYP_PRINT;
        device_box.device.IRPRequest = Some(irp_request_handler);
        device_box.device.Free = Some(free_handler);
        device_box.device.Init = Some(init_handler);
        device_box.device.CustomComponentRequest = Some(custom_component_handler);

        let port_str = printer
            .port_name
            .clone()
            .unwrap_or_else(|| format!("PRN{}", idx + 1));
        let port_c = std::ffi::CString::new(port_str).unwrap();
        let name_ptr = unsafe { freerdp_sys::_strdup(port_c.as_ptr()) };
        device_box.device.name = name_ptr;

        // Build UTF-16LE byte sequences for driver name and printer name
        let mut driver_bytes = Vec::new();
        for c in printer.driver_name.encode_utf16() {
            driver_bytes.extend_from_slice(&c.to_le_bytes());
        }
        driver_bytes.extend_from_slice(&0u16.to_le_bytes());

        let mut printer_bytes = Vec::new();
        for c in printer.name.encode_utf16() {
            printer_bytes.extend_from_slice(&c.to_le_bytes());
        }
        printer_bytes.extend_from_slice(&0u16.to_le_bytes());

        let total_stream_len = 24 + driver_bytes.len() + printer_bytes.len();
        let data_stream = unsafe { freerdp_sys::Stream_New(std::ptr::null_mut(), total_stream_len) };
        if data_stream.is_null() {
            log::error!("printer: Stream_New failed for device.data");
            unsafe {
                freerdp_sys::free(name_ptr as *mut std::ffi::c_void);
            }
            return CHANNEL_RC_NO_MEMORY;
        }

        let mut flags = 0u32;
        if printer.is_default {
            flags |= RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER;
        }

        unsafe {
            stream_write_u32(data_stream, flags);
            stream_write_u32(data_stream, 0); // CodePage
            stream_write_u32(data_stream, 0); // PnPNameLen
            stream_write_u32(data_stream, driver_bytes.len() as u32);
            stream_write_u32(data_stream, printer_bytes.len() as u32);
            stream_write_u32(data_stream, 0); // CachedFieldsLen

            let s = &mut *data_stream;
            std::ptr::copy_nonoverlapping(driver_bytes.as_ptr(), s.pointer, driver_bytes.len());
            s.pointer = s.pointer.add(driver_bytes.len());

            std::ptr::copy_nonoverlapping(printer_bytes.as_ptr(), s.pointer, printer_bytes.len());
            s.pointer = s.pointer.add(printer_bytes.len());

            device_box.device.data = data_stream;
        }

        let device_ptr = &mut device_box.device as *mut DEVICE;
        log::debug!(
            "printer: registering device {:p} for '{}' with devman {:p}",
            device_ptr,
            printer.name,
            entry_points.devman
        );

        let result = unsafe { register_fn(entry_points.devman, device_ptr) };
        if result == CHANNEL_RC_OK {
            let _ = Box::into_raw(device_box);
            log::info!("printer: device for '{}' registered successfully", printer.name);
        } else {
            log::error!(
                "printer: failed to register device for '{}': error {}",
                printer.name,
                result
            );
        }
    }

    CHANNEL_RC_OK
}
