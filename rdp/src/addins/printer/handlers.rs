// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! IRP request handlers for printer redirection.

use std::sync::Arc;

use freerdp_sys::{IRP, wStream};

use crate::integrations::PrinterIntegration;
use crate::utils::log;

use super::consts::*;

unsafe fn stream_remaining(stream: *mut wStream) -> usize {
    if stream.is_null() {
        return 0;
    }
    let s = unsafe { &*stream };
    let used = s.pointer as usize - s.buffer as usize;
    s.length.saturating_sub(used)
}

pub(crate) unsafe fn stream_write_u8(stream: *mut wStream, val: u8) {
    unsafe {
        let s = &mut *stream;
        *s.pointer = val;
        s.pointer = s.pointer.add(1);
    }
}

pub(crate) unsafe fn stream_write_u32(stream: *mut wStream, val: u32) {
    unsafe {
        let s = &mut *stream;
        std::ptr::copy_nonoverlapping(val.to_le_bytes().as_ptr(), s.pointer, 4);
        s.pointer = s.pointer.add(4);
    }
}

pub(crate) unsafe fn stream_write_zeros(stream: *mut wStream, count: usize) {
    unsafe {
        let s = &mut *stream;
        std::ptr::write_bytes(s.pointer, 0, count);
        s.pointer = s.pointer.add(count);
    }
}

pub(crate) unsafe fn process_irp(
    irp: *mut IRP,
    integration: &Arc<dyn PrinterIntegration>,
    printer_name: &str,
) {
    if irp.is_null() {
        return;
    }

    let irp_ref = unsafe { &mut *irp };
    let major = irp_ref.MajorFunction;

    match major {
        IRP_MJ_CREATE => {
            log::debug!("printer[{}]: processing IRP_MJ_CREATE", printer_name);
            match integration.create_job(printer_name) {
                Ok(job_id) => {
                    log::info!("printer[{}]: created print job id={}", printer_name, job_id);
                    if !irp_ref.output.is_null() {
                        unsafe {
                            stream_write_u32(irp_ref.output, job_id);
                        }
                    }
                    irp_ref.IoStatus = STATUS_SUCCESS;
                }
                Err(err) => {
                    log::error!("printer[{}]: create_job failed with error 0x{:08X}", printer_name, err);
                    if !irp_ref.output.is_null() {
                        unsafe {
                            stream_write_u32(irp_ref.output, 0);
                        }
                    }
                    irp_ref.IoStatus = STATUS_PRINT_QUEUE_FULL;
                }
            }
        }
        IRP_MJ_WRITE => {
            let file_id = irp_ref.FileId;
            let input = irp_ref.input;

            let remaining = unsafe { stream_remaining(input) };
            if remaining < 32 {
                log::error!("printer[{}]: IRP_MJ_WRITE input stream too short ({} < 32)", printer_name, remaining);
                irp_ref.IoStatus = STATUS_INVALID_DATA;
            } else {
                let (_len, data_slice) = unsafe {
                    let s = &mut *input;
                    let len_bytes = [
                        *s.pointer,
                        *s.pointer.add(1),
                        *s.pointer.add(2),
                        *s.pointer.add(3),
                    ];
                    let length = u32::from_le_bytes(len_bytes) as usize;
                    s.pointer = s.pointer.add(32); // skip Length (4), Offset (8), Padding (20)

                    let available = stream_remaining(input);
                    if available < length {
                        log::error!("printer[{}]: IRP_MJ_WRITE payload too short ({} < {})", printer_name, available, length);
                        (0, None)
                    } else {
                        let slice = std::slice::from_raw_parts(s.pointer, length);
                        s.pointer = s.pointer.add(length);
                        (length, Some(slice))
                    }
                };

                if let Some(data) = data_slice {
                    match integration.write_data(file_id, data) {
                        Ok(written) => {
                            if !irp_ref.output.is_null() {
                                unsafe {
                                    stream_write_u32(irp_ref.output, written as u32);
                                    stream_write_u8(irp_ref.output, 0); // Padding
                                }
                            }
                            irp_ref.IoStatus = STATUS_SUCCESS;
                        }
                        Err(e) => {
                            log::error!("printer[{}]: write_data job_id={} failed with error 0x{:08X}", printer_name, file_id, e);
                            if !irp_ref.output.is_null() {
                                unsafe {
                                    stream_write_u32(irp_ref.output, 0);
                                    stream_write_u8(irp_ref.output, 0);
                                }
                            }
                            irp_ref.IoStatus = STATUS_UNSUCCESSFUL;
                        }
                    }
                } else {
                    irp_ref.IoStatus = STATUS_INVALID_DATA;
                }
            }
        }
        IRP_MJ_CLOSE => {
            let file_id = irp_ref.FileId;
            log::info!("printer[{}]: closing print job id={}", printer_name, file_id);
            let _ = integration.close_job(file_id);
            if !irp_ref.output.is_null() {
                unsafe {
                    stream_write_zeros(irp_ref.output, 4);
                }
            }
            irp_ref.IoStatus = STATUS_SUCCESS;
        }
        IRP_MJ_DEVICE_CONTROL => {
            log::debug!("printer[{}]: IRP_MJ_DEVICE_CONTROL received", printer_name);
            if !irp_ref.output.is_null() {
                unsafe {
                    stream_write_u32(irp_ref.output, 0); // OutputBufferLength = 0
                }
            }
            irp_ref.IoStatus = STATUS_SUCCESS;
        }
        _ => {
            log::warn!("printer[{}]: unsupported IRP MajorFunction: 0x{:08X}", printer_name, major);
            irp_ref.IoStatus = STATUS_NOT_SUPPORTED;
        }
    }

    if let Some(complete) = irp_ref.Complete {
        unsafe {
            complete(irp);
        }
    }
}
