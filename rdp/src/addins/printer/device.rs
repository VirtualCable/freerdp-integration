// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Printer device struct, worker thread, and FreeRDP callbacks.

use std::sync::Arc;

use freerdp_sys::{CHANNEL_RC_OK, DEVICE, IRP, UINT};

use crate::integrations::PrinterIntegration;
use crate::utils::log;

use super::consts::*;
use super::handlers::process_irp;

pub(crate) enum IrpWork {
    Process(*mut IRP),
    Shutdown,
}

unsafe impl Send for IrpWork {}

#[repr(C)]
pub(crate) struct PrinterDevice {
    pub(crate) device: DEVICE,
    pub(crate) printer_name: String,
    pub(crate) integration: Arc<dyn PrinterIntegration>,
    pub(crate) irp_tx: flume::Sender<IrpWork>,
    pub(crate) device_thread: Option<std::thread::JoinHandle<()>>,
}

pub(crate) fn printer_thread_main(
    irp_rx: flume::Receiver<IrpWork>,
    integration: Arc<dyn PrinterIntegration>,
    printer_name: String,
) {
    log::info!("printer[{}]: worker thread started", printer_name);

    while let Ok(work) = irp_rx.recv() {
        match work {
            IrpWork::Process(irp) => {
                unsafe {
                    process_irp(irp, &integration, &printer_name);
                }
            }
            IrpWork::Shutdown => {
                log::info!("printer[{}]: worker thread shutdown requested", printer_name);
                break;
            }
        }
    }

    log::info!("printer[{}]: worker thread terminated", printer_name);
}

pub(crate) unsafe extern "C" fn irp_request_handler(device: *mut DEVICE, irp: *mut IRP) -> UINT {
    if device.is_null() || irp.is_null() {
        log::error!("printer: irp_request_handler received null pointer");
        return CHANNEL_RC_OK;
    }

    let pdev = unsafe { &*(device as *const PrinterDevice) };
    if let Err(e) = pdev.irp_tx.send(IrpWork::Process(irp)) {
        log::error!("printer: failed to queue IRP: {}", e);
        unsafe {
            let irp_ref = &mut *irp;
            irp_ref.IoStatus = STATUS_UNSUCCESSFUL;
            if let Some(complete) = irp_ref.Complete {
                complete(irp);
            }
        }
    }

    CHANNEL_RC_OK
}

pub(crate) unsafe extern "C" fn init_handler(_device: *mut DEVICE) -> UINT {
    log::debug!("printer: init_handler called");
    CHANNEL_RC_OK
}

pub(crate) unsafe extern "C" fn free_handler(device: *mut DEVICE) -> UINT {
    if device.is_null() {
        return CHANNEL_RC_OK;
    }

    log::info!("printer: freeing device at {:p}", device);
    let mut pdev = unsafe { Box::from_raw(device as *mut PrinterDevice) };

    let _ = pdev.irp_tx.send(IrpWork::Shutdown);
    if let Some(handle) = pdev.device_thread.take() {
        let _ = handle.join();
    }

    if !pdev.device.name.is_null() {
        unsafe {
            freerdp_sys::free(pdev.device.name as *mut std::ffi::c_void);
        }
        pdev.device.name = std::ptr::null();
    }

    if !pdev.device.data.is_null() {
        unsafe {
            freerdp_sys::Stream_Free(pdev.device.data, 1);
        }
        pdev.device.data = std::ptr::null_mut();
    }

    log::info!("printer: device teardown complete");
    CHANNEL_RC_OK
}
