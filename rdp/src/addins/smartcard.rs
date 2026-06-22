// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Smartcard channel addin — intercepts the FreeRDP smartcard device service
//! via `custom_addin_provider` and routes all IRP requests to the
//! `SmartcardIntegration` trait.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use freerdp_sys::{
    CHANNEL_RC_OK, DEVICE, IRP, PDEVICE_SERVICE_ENTRY_POINTS, UINT,
};

use crate::context::OwnerFromCtx;
use crate::integrations::SmartcardIntegration;
use crate::utils::log;

use crate::integrations::smartcard::{
    ScardContext,
    // Constants
    SCARD_E_INVALID_HANDLE,
    SCARD_E_NO_READERS_AVAILABLE,
    SCARD_E_UNSUPPORTED_FEATURE,
    SCARD_S_SUCCESS,
};

// ---------------------------------------------------------------------------
// IOCTL Constants (MS-RDPESC)
// ---------------------------------------------------------------------------

const SCARD_IOCTL_ESTABLISHCONTEXT: u32 = 0x0009_0014;
const SCARD_IOCTL_RELEASECONTEXT: u32 = 0x0009_0018;
const SCARD_IOCTL_ISVALIDCONTEXT: u32 = 0x0009_001C;
const SCARD_IOCTL_LISTREADERGROUPSA: u32 = 0x0009_0020;
const SCARD_IOCTL_LISTREADERGROUPSW: u32 = 0x0009_0024;
const SCARD_IOCTL_LISTREADERSA: u32 = 0x0009_0028;
const SCARD_IOCTL_LISTREADERSW: u32 = 0x0009_002C;
const SCARD_IOCTL_GETSTATUSCHANGEA: u32 = 0x0009_00A0;
const SCARD_IOCTL_GETSTATUSCHANGEW: u32 = 0x0009_00A4;
const SCARD_IOCTL_CANCEL: u32 = 0x0009_00A8;
const SCARD_IOCTL_CONNECTA: u32 = 0x0009_00AC;
const SCARD_IOCTL_CONNECTW: u32 = 0x0009_00B0;
const SCARD_IOCTL_RECONNECT: u32 = 0x0009_00B4;
const SCARD_IOCTL_DISCONNECT: u32 = 0x0009_00B8;
const SCARD_IOCTL_BEGINTRANSACTION: u32 = 0x0009_00BC;
const SCARD_IOCTL_ENDTRANSACTION: u32 = 0x0009_00C0;
const SCARD_IOCTL_STATUSA: u32 = 0x0009_00C8;
const SCARD_IOCTL_STATUSW: u32 = 0x0009_00CC;
const SCARD_IOCTL_TRANSMIT: u32 = 0x0009_00D0;
const SCARD_IOCTL_CONTROL: u32 = 0x0009_00D4;
const SCARD_IOCTL_GETATTRIB: u32 = 0x0009_00D8;
const SCARD_IOCTL_SETATTRIB: u32 = 0x0009_00DC;
const SCARD_IOCTL_ACCESSSTARTEDEVENT: u32 = 0x0009_00E0;
const SCARD_IOCTL_RELEASETARTEDEVENT: u32 = 0x0009_00E4;
const SCARD_IOCTL_LOCATECARDSBYATRA: u32 = 0x0009_00E8;
const SCARD_IOCTL_LOCATECARDSBYATRW: u32 = 0x0009_00EC;
const SCARD_IOCTL_LOCATECARDSA: u32 = 0x0009_0098;
const SCARD_IOCTL_LOCATECARDSW: u32 = 0x0009_009C;
const SCARD_IOCTL_STATE: u32 = 0x0009_00C4;

/// RDPDR device type for smartcard.
const RDPDR_DTYP_SMARTCARD: u32 = 0x0020;

// ---------------------------------------------------------------------------
// Device Structure
// ---------------------------------------------------------------------------

#[repr(C)]
struct SmartcardDevice {
    /// Must be the first field — FreeRDP casts `DEVICE*` to access this.
    device: DEVICE,

    /// The consumer-provided smartcard implementation.
    integration: Arc<dyn SmartcardIntegration>,

    /// Registry of active contexts (ScardContext raw id → ContextEntry).
    contexts: Mutex<HashMap<u64, ContextEntry>>,
}

// SAFETY: SmartcardDevice is only accessed from FreeRDP callback threads
// and uses Mutex for internal state.
unsafe impl Send for SmartcardDevice {}
unsafe impl Sync for SmartcardDevice {}

struct ContextEntry {
    context: ScardContext,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

unsafe fn stream_write_u32(stream: *mut freerdp_sys::wStream, val: u32) {
    unsafe {
        let s = &mut *stream;
        let ptr = s.pointer as *mut u8;
        std::ptr::copy_nonoverlapping(val.to_le_bytes().as_ptr(), ptr, 4);
        s.pointer = s.pointer.add(4);
    }
}

unsafe fn u16_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        String::from_utf16_lossy(slice)
    }
}

fn build_multi_string_ascii(strings: &[String]) -> Vec<u8> {
    let mut result = Vec::new();
    for s in strings {
        result.extend_from_slice(s.as_bytes());
        result.push(0);
    }
    result.push(0);
    result
}

fn build_multi_string_utf16(strings: &[String]) -> Vec<u8> {
    let mut result = Vec::new();
    for s in strings {
        for c in s.encode_utf16() {
            result.extend_from_slice(&c.to_le_bytes());
        }
        result.extend_from_slice(&0u16.to_le_bytes());
    }
    result.extend_from_slice(&0u16.to_le_bytes());
    result
}

fn get_card_handle(h_card: &freerdp_sys::REDIR_SCARDHANDLE) -> crate::integrations::smartcard::ScardHandle {
    let mut handle_bytes = [0u8; 8];
    handle_bytes.copy_from_slice(&h_card.pbHandle[..8]);
    let handle_id = u64::from_le_bytes(handle_bytes);
    crate::integrations::smartcard::ScardHandle::from_raw(handle_id, 0)
}

fn get_io_request(pci: freerdp_sys::LPSCARD_IO_REQUEST) -> crate::integrations::smartcard::ScardIORequest {
    if pci.is_null() {
        return crate::integrations::smartcard::ScardIORequest::t0();
    }
    unsafe {
        let pci_ref = &*pci;
        let mut extra_bytes = Vec::new();
        if pci_ref.cbPciLength > 8 {
            let extra_len = (pci_ref.cbPciLength - 8) as usize;
            let ptr = (pci as *const u8).add(8);
            extra_bytes.extend_from_slice(std::slice::from_raw_parts(ptr, extra_len));
        }
        crate::integrations::smartcard::ScardIORequest {
            protocol: pci_ref.dwProtocol,
            extra_bytes,
        }
    }
}

fn get_reader_states_in_a(
    states: freerdp_sys::LPSCARD_READERSTATEA,
    count: u32,
) -> Vec<crate::integrations::smartcard::ReaderStateIn> {
    let mut result = Vec::new();
    if states.is_null() || count == 0 {
        return result;
    }
    unsafe {
        for i in 0..(count as usize) {
            let state = &*states.add(i);
            let name = if state.szReader.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(state.szReader)
                    .to_string_lossy()
                    .into_owned()
            };
            result.push(crate::integrations::smartcard::ReaderStateIn {
                reader_name: name,
                current_state: state.dwCurrentState,
            });
        }
    }
    result
}

fn get_reader_states_in_w(
    states: freerdp_sys::LPSCARD_READERSTATEW,
    count: u32,
) -> Vec<crate::integrations::smartcard::ReaderStateIn> {
    let mut result = Vec::new();
    if states.is_null() || count == 0 {
        return result;
    }
    unsafe {
        for i in 0..(count as usize) {
            let state = &*states.add(i);
            let name = u16_ptr_to_string(state.szReader);
            result.push(crate::integrations::smartcard::ReaderStateIn {
                reader_name: name,
                current_state: state.dwCurrentState,
            });
        }
    }
    result
}

fn pack_reader_states_out(
    states_out: &[crate::integrations::smartcard::ReaderStateOut],
) -> Vec<freerdp_sys::ReaderState_Return> {
    states_out.iter().map(|rs| {
        let mut rgb_atr = [0u8; 36];
        let atr_len = rs.atr.len().min(36);
        rgb_atr[..atr_len].copy_from_slice(&rs.atr[..atr_len]);
        freerdp_sys::ReaderState_Return {
            dwCurrentState: 0,
            dwEventState: rs.event_state,
            cbAtr: atr_len as u32,
            rgbAtr: rgb_atr,
        }
    }).collect()
}

fn get_atr_masks(
    masks: *mut freerdp_sys::LocateCards_ATRMask,
    count: u32,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut result = Vec::new();
    if masks.is_null() || count == 0 {
        return result;
    }
    unsafe {
        for i in 0..(count as usize) {
            let m = &*masks.add(i);
            let len = m.cbAtr.min(36) as usize;
            result.push((m.rgbAtr[..len].to_vec(), m.rgbMask[..len].to_vec()));
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Header Packing helpers
// ---------------------------------------------------------------------------

unsafe fn begin_response(out: *mut freerdp_sys::wStream) -> usize {
    unsafe {
        // Write 24 bytes of dummy header
        stream_write_u32(out, 0); // OutputBufferLength
        stream_write_u32(out, 0); // CommonTypeHeader pt 1
        stream_write_u32(out, 0); // CommonTypeHeader pt 2
        stream_write_u32(out, 0); // PrivateTypeHeader pt 1
        stream_write_u32(out, 0); // PrivateTypeHeader pt 2
        stream_write_u32(out, 0); // Result
        
        let s = &*out;
        (s.pointer as usize).saturating_sub(s.buffer as usize)
    }
}

unsafe fn end_response(
    out: *mut freerdp_sys::wStream,
    body_pos: usize,
    result: i32,
    output_buffer_length_limit: u32,
    p_io_status: &mut i32,
) {
    unsafe {
        // Align body to 8-byte boundary
        let s = &*out;
        let current_pos = (s.pointer as usize).saturating_sub(s.buffer as usize);
        let body_len = current_pos - body_pos;
        let padding = (8 - (body_len % 8)) % 8;
        if padding > 0 {
            let ptr = (&mut *out).pointer as *mut u8;
            std::ptr::write_bytes(ptr, 0, padding);
            (&mut *out).pointer = (&mut *out).pointer.add(padding);
        }
        freerdp_sys::Stream_SealLength(out);
        
        let total_len = (*out).length;
        
        let mut output_buf_len = total_len - 20;
        let mut obj_buf_len = output_buf_len - 16;
        
        let mut final_result = result;
        
        if output_buf_len > output_buffer_length_limit as usize {
            log::warn!(
                "smartcard IRP: expected outputBufferLength {}, but current length {}, returning STATUS_BUFFER_TOO_SMALL",
                output_buffer_length_limit,
                output_buf_len
            );
            *p_io_status = crate::integrations::smartcard::STATUS_BUFFER_TOO_SMALL as i32;
            final_result = *p_io_status;
            output_buf_len = 0;
            obj_buf_len = 0;
        } else {
            *p_io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
        }
        
        freerdp_sys::Stream_SetPosition(out, 16);
        
        stream_write_u32(out, output_buf_len as u32);
        freerdp_sys::smartcard_pack_common_type_header(out);
        freerdp_sys::smartcard_pack_private_type_header(out, obj_buf_len as u32);
        stream_write_u32(out, final_result as u32);
        
        freerdp_sys::Stream_SetPosition(out, total_len);
    }
}

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

    let mut device_box = Box::new(SmartcardDevice {
        device: unsafe { std::mem::zeroed() },
        integration,
        contexts: Mutex::new(HashMap::new()),
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
        unsafe { freerdp_sys::free(name_ptr as *mut std::ffi::c_void); }
        return freerdp_sys::CHANNEL_RC_NO_MEMORY;
    }

    unsafe {
        let bytes = name_c.as_bytes_with_nul();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*data_stream).pointer as *mut u8, bytes.len());
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

// ---------------------------------------------------------------------------
// Init Handler
// ---------------------------------------------------------------------------

unsafe extern "C" fn init_handler(device: *mut DEVICE) -> UINT {
    if device.is_null() {
        return freerdp_sys::ERROR_INVALID_PARAMETER;
    }
    log::info!("smartcard: init_handler called");
    let scard = unsafe { &mut *(device as *mut SmartcardDevice) };
    let mut contexts = scard.contexts.lock().unwrap();
    for (_, entry) in contexts.drain() {
        let _ = scard.integration.release_context(&entry.context);
    }
    freerdp_sys::CHANNEL_RC_OK
}

// ---------------------------------------------------------------------------
// Free Handler
// ---------------------------------------------------------------------------

unsafe extern "C" fn free_handler(device: *mut DEVICE) -> UINT {
    if device.is_null() {
        return CHANNEL_RC_OK;
    }

    log::info!("smartcard: free_handler — tearing down device");

    let mut scard = unsafe { Box::from_raw(device as *mut SmartcardDevice) };

    {
        let mut contexts = scard.contexts.lock().unwrap();
        for (ctx_id, entry) in contexts.drain() {
            log::debug!("smartcard: releasing context 0x{:X} during teardown", ctx_id);
            let _ = scard.integration.release_context(&entry.context);
        }
    }

    if !scard.device.name.is_null() {
        unsafe {
            freerdp_sys::free(scard.device.name as *mut std::ffi::c_void);
        }
        scard.device.name = std::ptr::null();
    }

    if !scard.device.data.is_null() {
        unsafe {
            freerdp_sys::Stream_Free(scard.device.data, 1);
        }
        scard.device.data = std::ptr::null_mut();
    }

    log::info!("smartcard: device teardown complete");
    CHANNEL_RC_OK
}

// ---------------------------------------------------------------------------
// IRP Request Handler
// ---------------------------------------------------------------------------

unsafe extern "C" fn irp_request_handler(device: *mut DEVICE, irp: *mut IRP) -> UINT {
    if device.is_null() || irp.is_null() {
        log::error!("smartcard: irp_request_handler called with null pointer(s)");
        return CHANNEL_RC_OK;
    }

    let scard = unsafe { &*(device as *const SmartcardDevice) };

    let mut operation: freerdp_sys::SMARTCARD_OPERATION = unsafe { std::mem::zeroed() };
    let decode_status = unsafe {
        freerdp_sys::smartcard_irp_device_control_decode(
            (*irp).input,
            (*irp).CompletionId,
            (*irp).FileId,
            &mut operation,
        )
    };

    if decode_status != crate::integrations::smartcard::SCARD_S_SUCCESS as i32 {
        log::error!("smartcard: failed to decode IRP: 0x{:08X}", decode_status);
        unsafe {
            let irp_ref = &mut *irp;
            irp_ref.IoStatus = crate::integrations::smartcard::STATUS_SUCCESS as i32;
            if !irp_ref.output.is_null() {
                freerdp_sys::Stream_SetPosition(irp_ref.output, 16);
                stream_write_u32(irp_ref.output, 0); // OutputBufferLength
                freerdp_sys::smartcard_pack_common_type_header(irp_ref.output);
                freerdp_sys::smartcard_pack_private_type_header(irp_ref.output, 0);
                stream_write_u32(irp_ref.output, decode_status as u32); // Result
                freerdp_sys::Stream_SealLength(irp_ref.output);
            }
            if let Some(complete_fn) = irp_ref.Complete {
                complete_fn(irp);
            }
        }
        return freerdp_sys::CHANNEL_RC_OK;
    }

    let ioctl = operation.ioControlCode;

    log::debug!(
        "smartcard: IRP ioctl=0x{:08X} ({}) completion_id={}",
        ioctl,
        ioctl_name(ioctl),
        operation.completionID
    );

    let mut io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
    let body_pos = unsafe { begin_response((*irp).output) };

    let return_code = match ioctl {
        SCARD_IOCTL_ESTABLISHCONTEXT => handle_establish_context(scard, &operation, unsafe { (*irp).output }),
        SCARD_IOCTL_RELEASECONTEXT => handle_release_context(scard, &operation),
        SCARD_IOCTL_ISVALIDCONTEXT => handle_is_valid_context(scard, &operation),
        SCARD_IOCTL_CANCEL => handle_cancel(scard, &operation),
        SCARD_IOCTL_ACCESSSTARTEDEVENT => handle_access_started_event(scard, &operation),
        SCARD_IOCTL_RELEASETARTEDEVENT => handle_release_started_event(scard, &operation),
        SCARD_IOCTL_LISTREADERGROUPSA | SCARD_IOCTL_LISTREADERSA => {
            handle_list_readers(scard, &operation, unsafe { (*irp).output }, false)
        }
        SCARD_IOCTL_LISTREADERGROUPSW | SCARD_IOCTL_LISTREADERSW => {
            handle_list_readers(scard, &operation, unsafe { (*irp).output }, true)
        }
        SCARD_IOCTL_CONNECTA => handle_connect(scard, &operation, unsafe { (*irp).output }, false),
        SCARD_IOCTL_CONNECTW => handle_connect(scard, &operation, unsafe { (*irp).output }, true),
        SCARD_IOCTL_RECONNECT => handle_reconnect(scard, &operation, unsafe { (*irp).output }),
        SCARD_IOCTL_DISCONNECT => handle_disconnect(scard, &operation),
        SCARD_IOCTL_BEGINTRANSACTION => handle_begin_transaction(scard, &operation),
        SCARD_IOCTL_ENDTRANSACTION => handle_end_transaction(scard, &operation),
        SCARD_IOCTL_TRANSMIT => handle_transmit(scard, &operation, unsafe { (*irp).output }),
        SCARD_IOCTL_CONTROL => handle_control(scard, &operation, unsafe { (*irp).output }),
        SCARD_IOCTL_GETATTRIB => handle_get_attrib(scard, &operation, unsafe { (*irp).output }),
        SCARD_IOCTL_SETATTRIB => handle_set_attrib(scard, &operation),
        SCARD_IOCTL_STATE => handle_state(scard, &operation, unsafe { (*irp).output }),
        SCARD_IOCTL_STATUSA => handle_status(scard, &operation, unsafe { (*irp).output }, false),
        SCARD_IOCTL_STATUSW => handle_status(scard, &operation, unsafe { (*irp).output }, true),
        SCARD_IOCTL_GETSTATUSCHANGEA => handle_get_status_change(scard, &operation, unsafe { (*irp).output }, false),
        SCARD_IOCTL_GETSTATUSCHANGEW => handle_get_status_change(scard, &operation, unsafe { (*irp).output }, true),
        SCARD_IOCTL_LOCATECARDSA => handle_locate_cards(&operation, unsafe { (*irp).output }, false),
        SCARD_IOCTL_LOCATECARDSW => handle_locate_cards(&operation, unsafe { (*irp).output }, true),
        SCARD_IOCTL_LOCATECARDSBYATRA => handle_locate_cards_by_atr(scard, &operation, unsafe { (*irp).output }, false),
        SCARD_IOCTL_LOCATECARDSBYATRW => handle_locate_cards_by_atr(scard, &operation, unsafe { (*irp).output }, true),
        _ => SCARD_E_UNSUPPORTED_FEATURE,
    };

    unsafe {
        end_response(
            (*irp).output,
            body_pos,
            return_code as i32,
            operation.outputBufferLength,
            &mut io_status,
        );
        (*irp).IoStatus = io_status;
        if let Some(complete_fn) = (*irp).Complete {
            complete_fn(irp);
        } else {
            log::error!("smartcard: IRP has no Complete callback");
        }
        freerdp_sys::smartcard_operation_free(&mut operation, 1);
    }

    CHANNEL_RC_OK
}

// ---------------------------------------------------------------------------
// IOCTL Handlers
// ---------------------------------------------------------------------------

fn handle_establish_context(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let scope = unsafe { operation.call.establishContext.dwScope };
    log::debug!("smartcard: ESTABLISH_CONTEXT scope={}", scope);

    match scard.integration.establish_context(scope) {
        Ok(ctx) => {
            let ctx_id = ctx.raw();
            log::info!("smartcard: established context 0x{:X}", ctx_id);

            let mut contexts = scard.contexts.lock().unwrap();
            contexts.insert(ctx_id, ContextEntry { context: ctx });

            unsafe {
                let mut pb_context = [0u8; 8];
                pb_context[..8].copy_from_slice(&ctx_id.to_le_bytes());
                let ret = freerdp_sys::EstablishContext_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    hContext: freerdp_sys::REDIR_SCARDCONTEXT {
                        cbContext: 8,
                        pbContext: pb_context,
                    },
                };
                freerdp_sys::smartcard_pack_establish_context_return(out, &ret);
            }

            SCARD_S_SUCCESS
        }
        Err(code) => {
            log::error!("smartcard: establish_context failed: 0x{:08X}", code);
            code
        }
    }
}

fn handle_release_context(scard: &SmartcardDevice, operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    let ctx_id = operation.hContext as u64;
    log::debug!("smartcard: RELEASE_CONTEXT 0x{:X}", ctx_id);

    let mut contexts = scard.contexts.lock().unwrap();
    if let Some(entry) = contexts.remove(&ctx_id) {
        match scard.integration.release_context(&entry.context) {
            Ok(()) => {
                log::info!("smartcard: released context 0x{:X}", ctx_id);
                SCARD_S_SUCCESS
            }
            Err(code) => {
                log::error!("smartcard: release_context failed for 0x{:X}: 0x{:08X}", ctx_id, code);
                code
            }
        }
    } else {
        log::warn!("smartcard: RELEASE_CONTEXT for unknown context 0x{:X}", ctx_id);
        SCARD_E_INVALID_HANDLE
    }
}

fn handle_is_valid_context(scard: &SmartcardDevice, operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    let ctx_id = operation.hContext as u64;
    let contexts = scard.contexts.lock().unwrap();
    if contexts.contains_key(&ctx_id) {
        SCARD_S_SUCCESS
    } else {
        SCARD_E_INVALID_HANDLE
    }
}

fn handle_cancel(scard: &SmartcardDevice, operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    let ctx_id = operation.hContext as u64;
    log::debug!("smartcard: CANCEL for context 0x{:X}", ctx_id);

    let contexts = scard.contexts.lock().unwrap();
    if let Some(entry) = contexts.get(&ctx_id) {
        match scard.integration.cancel(&entry.context) {
            Ok(()) => SCARD_S_SUCCESS,
            Err(code) => code,
        }
    } else {
        SCARD_E_INVALID_HANDLE
    }
}

fn handle_access_started_event(_scard: &SmartcardDevice, _operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    log::debug!("smartcard: ACCESS_STARTED_EVENT — responding success");
    SCARD_S_SUCCESS
}

fn handle_release_started_event(_scard: &SmartcardDevice, _operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    log::debug!("smartcard: RELEASE_STARTED_EVENT — responding success");
    SCARD_S_SUCCESS
}

fn handle_list_readers(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ioctl = operation.ioControlCode;
    log::debug!("smartcard: LIST_READERS/GROUPS for context 0x{:X}", ctx_id);

    let ctx = {
        let contexts = scard.contexts.lock().unwrap();
        match contexts.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    match scard.integration.list_readers(&ctx, None) {
        Ok(readers) => {
            log::debug!("smartcard: found {} reader(s): {:?}", readers.len(), readers);
            if readers.is_empty() {
                return SCARD_E_NO_READERS_AVAILABLE;
            }

            let mut msz_bytes = if unicode {
                build_multi_string_utf16(&readers)
            } else {
                build_multi_string_ascii(&readers)
            };

            let ret = freerdp_sys::ListReaders_Return {
                ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                cBytes: msz_bytes.len() as u32,
                msz: msz_bytes.as_mut_ptr(),
            };

            unsafe {
                if ioctl == SCARD_IOCTL_LISTREADERGROUPSA || ioctl == SCARD_IOCTL_LISTREADERGROUPSW {
                    freerdp_sys::smartcard_pack_list_reader_groups_return(
                        out,
                        &ret,
                        if unicode { 1 } else { 0 },
                    );
                } else {
                    freerdp_sys::smartcard_pack_list_readers_return(
                        out,
                        &ret,
                        if unicode { 1 } else { 0 },
                    );
                }
            }

            SCARD_S_SUCCESS
        }
        Err(code) => {
            log::debug!("smartcard: list_readers failed: 0x{:08X}", code);
            code
        }
    }
}

fn handle_connect(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    log::debug!("smartcard: CONNECT for context 0x{:X}", ctx_id);

    let ctx = {
        let contexts = scard.contexts.lock().unwrap();
        match contexts.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    let (reader_name, share_mode, preferred_protocols) = unsafe {
        if unicode {
            let call = &operation.call.connectW;
            (
                u16_ptr_to_string(call.szReader),
                call.Common.dwShareMode,
                call.Common.dwPreferredProtocols,
            )
        } else {
            let call = &operation.call.connectA;
            let name = if call.szReader.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(call.szReader)
                    .to_string_lossy()
                    .into_owned()
            };
            (name, call.Common.dwShareMode, call.Common.dwPreferredProtocols)
        }
    };

    match scard.integration.connect(
        &ctx,
        &reader_name,
        share_mode,
        preferred_protocols,
    ) {
        Ok(result) => {
            log::info!(
                "smartcard: connected to '{}', handle=0x{:X}, protocol=0x{:X}",
                reader_name,
                result.handle.raw(),
                result.active_protocol
            );

            unsafe {
                let mut pb_context = [0u8; 8];
                pb_context[..8].copy_from_slice(&ctx_id.to_le_bytes());
                let mut pb_handle = [0u8; 8];
                pb_handle[..8].copy_from_slice(&result.handle.raw().to_le_bytes());

                let ret = freerdp_sys::Connect_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    hContext: freerdp_sys::REDIR_SCARDCONTEXT {
                        cbContext: 8,
                        pbContext: pb_context,
                    },
                    hCard: freerdp_sys::REDIR_SCARDHANDLE {
                        cbHandle: 8,
                        pbHandle: pb_handle,
                    },
                    dwActiveProtocol: result.active_protocol,
                };

                freerdp_sys::smartcard_pack_connect_return(out, &ret);
            }

            SCARD_S_SUCCESS
        }
        Err(code) => {
            log::debug!("smartcard: connect failed: 0x{:08X}", code);
            code
        }
    }
}

fn handle_reconnect(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.reconnect };
    let card_handle = get_card_handle(&call.handles.hCard);
    let share_mode = call.dwShareMode;
    let preferred_protocols = call.dwPreferredProtocols;
    let initialization = call.dwInitialization;
    log::debug!(
        "smartcard: RECONNECT card=0x{:X} share_mode={} preferred_protocols={} initialization={}",
        card_handle.raw(),
        share_mode,
        preferred_protocols,
        initialization
    );

    match scard.integration.reconnect(&card_handle, share_mode, preferred_protocols, initialization) {
        Ok(active_protocol) => {
            unsafe {
                let ret = freerdp_sys::Reconnect_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    dwActiveProtocol: active_protocol,
                };
                freerdp_sys::smartcard_pack_reconnect_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_disconnect(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.hCardAndDisposition };
    let card_handle = get_card_handle(&call.handles.hCard);
    let disposition = call.dwDisposition;
    log::debug!("smartcard: DISCONNECT card=0x{:X} disposition={}", card_handle.raw(), disposition);

    match scard.integration.disconnect(&card_handle, disposition) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_begin_transaction(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.hCardAndDisposition };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!("smartcard: BEGIN_TRANSACTION card=0x{:X}", card_handle.raw());

    match scard.integration.begin_transaction(&card_handle) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_end_transaction(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.hCardAndDisposition };
    let card_handle = get_card_handle(&call.handles.hCard);
    let disposition = call.dwDisposition;
    log::debug!("smartcard: END_TRANSACTION card=0x{:X} disposition={}", card_handle.raw(), disposition);

    match scard.integration.end_transaction(&card_handle, disposition) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_state(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.state };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!("smartcard: STATE card=0x{:X}", card_handle.raw());

    match scard.integration.status(&card_handle) {
        Ok(status_info) => {
            let mut rg_atr = [0u8; 36];
            let atr_len = status_info.atr.len().min(36);
            rg_atr[..atr_len].copy_from_slice(&status_info.atr[..atr_len]);

            unsafe {
                let ret = freerdp_sys::State_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    dwState: status_info.state,
                    dwProtocol: status_info.protocol,
                    cbAtrLen: atr_len as u32,
                    rgAtr: rg_atr,
                };
                freerdp_sys::smartcard_pack_state_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_status(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let call = unsafe { &operation.call.status };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!("smartcard: STATUS card=0x{:X}", card_handle.raw());

    match scard.integration.status(&card_handle) {
        Ok(status_info) => {
            let mut msz_bytes = if unicode {
                build_multi_string_utf16(&status_info.reader_names)
            } else {
                build_multi_string_ascii(&status_info.reader_names)
            };

            let mut pb_atr = [0u8; 32];
            let atr_len = status_info.atr.len().min(32);
            pb_atr[..atr_len].copy_from_slice(&status_info.atr[..atr_len]);

            unsafe {
                let ret = freerdp_sys::Status_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    cBytes: msz_bytes.len() as u32,
                    mszReaderNames: msz_bytes.as_mut_ptr(),
                    dwState: status_info.state,
                    dwProtocol: status_info.protocol,
                    pbAtr: pb_atr,
                    cbAtrLen: atr_len as u32,
                };
                freerdp_sys::smartcard_pack_status_return(out, &ret, if unicode { 1 } else { 0 });
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_transmit(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.transmit };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!("smartcard: TRANSMIT card=0x{:X} send_len={}", card_handle.raw(), call.cbSendLength);

    let send_pci = get_io_request(call.pioSendPci);
    let send_data = unsafe { std::slice::from_raw_parts(call.pbSendBuffer, call.cbSendLength as usize) };

    match scard.integration.transmit(&card_handle, &send_pci, send_data) {
        Ok(result) => {
            log::debug!("smartcard: transmit success, recv_len={}", result.recv_buffer.len());

            let pio_recv_pci = if let Some(ref recv_pci) = result.recv_pci {
                let mut pci_buf = Vec::new();
                pci_buf.extend_from_slice(&recv_pci.protocol.to_le_bytes());
                let total_pci_len = (8 + recv_pci.extra_bytes.len()) as u32;
                pci_buf.extend_from_slice(&total_pci_len.to_le_bytes());
                pci_buf.extend_from_slice(&recv_pci.extra_bytes);
                Some(pci_buf)
            } else {
                None
            };

            let mut recv_buf = result.recv_buffer;
            let ret = freerdp_sys::Transmit_Return {
                ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                pioRecvPci: pio_recv_pci.as_ref().map(|b| b.as_ptr() as freerdp_sys::LPSCARD_IO_REQUEST).unwrap_or(std::ptr::null_mut()),
                cbRecvLength: recv_buf.len() as u32,
                pbRecvBuffer: recv_buf.as_mut_ptr(),
            };

            unsafe {
                freerdp_sys::smartcard_pack_transmit_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_control(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.control };
    let card_handle = get_card_handle(&call.handles.hCard);
    let control_code = call.dwControlCode;
    log::debug!("smartcard: CONTROL card=0x{:X} code=0x{:X} len={}", card_handle.raw(), control_code, call.cbInBufferSize);

    let in_data = unsafe { std::slice::from_raw_parts(call.pvInBuffer, call.cbInBufferSize as usize) };

    match scard.integration.control(&card_handle, control_code, in_data) {
        Ok(mut out_data) => {
            log::debug!("smartcard: control success, response_len={}", out_data.len());
            unsafe {
                let ret = freerdp_sys::Control_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    cbOutBufferSize: out_data.len() as u32,
                    pvOutBuffer: out_data.as_mut_ptr(),
                };
                freerdp_sys::smartcard_pack_control_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_get_attrib(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.getAttrib };
    let card_handle = get_card_handle(&call.handles.hCard);
    let attr_id = call.dwAttrId;
    log::debug!("smartcard: GET_ATTRIB card=0x{:X} attr_id=0x{:X}", card_handle.raw(), attr_id);

    match scard.integration.get_attrib(&card_handle, attr_id) {
        Ok(mut attr_data) => {
            log::debug!("smartcard: get_attrib success, response_len={}", attr_data.len());
            unsafe {
                let ret = freerdp_sys::GetAttrib_Return {
                    ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                    cbAttrLen: attr_data.len() as u32,
                    pbAttr: attr_data.as_mut_ptr(),
                };
                freerdp_sys::smartcard_pack_get_attrib_return(out, &ret, attr_id, call.cbAttrLen);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_set_attrib(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.setAttrib };
    let card_handle = get_card_handle(&call.handles.hCard);
    let attr_id = call.dwAttrId;
    log::debug!("smartcard: SET_ATTRIB card=0x{:X} attr_id=0x{:X} len={}", card_handle.raw(), attr_id, call.cbAttrLen);

    let data = unsafe { std::slice::from_raw_parts(call.pbAttr, call.cbAttrLen as usize) };

    match scard.integration.set_attrib(&card_handle, attr_id, data) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_get_status_change(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ctx = {
        let contexts = scard.contexts.lock().unwrap();
        match contexts.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    let (timeout_ms, reader_states_in) = unsafe {
        if unicode {
            let call = &operation.call.getStatusChangeW;
            (call.dwTimeOut, get_reader_states_in_w(call.rgReaderStates, call.cReaders))
        } else {
            let call = &operation.call.getStatusChangeA;
            (call.dwTimeOut, get_reader_states_in_a(call.rgReaderStates, call.cReaders))
        }
    };

    let timeout = if timeout_ms == 0xFFFF_FFFF {
        Duration::from_secs(3600 * 24)
    } else {
        Duration::from_millis(timeout_ms as u64)
    };

    log::debug!("smartcard: GET_STATUS_CHANGE context=0x{:X} timeout_ms={} count={}", ctx_id, timeout_ms, reader_states_in.len());

    match scard.integration.get_status_change(&ctx, timeout, &reader_states_in) {
        Ok(result_states) => {
            log::debug!("smartcard: get_status_change success, count={}", result_states.len());
            let mut returned_states = pack_reader_states_out(&result_states);
            let ret = freerdp_sys::LocateCards_Return {
                ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                cReaders: returned_states.len() as u32,
                rgReaderStates: returned_states.as_mut_ptr(),
            };
            unsafe {
                freerdp_sys::smartcard_pack_get_status_change_return(out, &ret, if unicode { 1 } else { 0 });
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_locate_cards(
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let (c_readers, rg_reader_states) = unsafe {
        if unicode {
            let call = &operation.call.locateCardsW;
            (call.cReaders, call.rgReaderStates as *mut std::ffi::c_void)
        } else {
            let call = &operation.call.locateCardsA;
            (call.cReaders, call.rgReaderStates as *mut std::ffi::c_void)
        }
    };

    log::debug!("smartcard: LOCATE_CARDS count={}", c_readers);

    let mut returned_states = Vec::new();
    if c_readers > 0 && !rg_reader_states.is_null() {
        for i in 0..(c_readers as usize) {
            unsafe {
                if unicode {
                    let state = &*(rg_reader_states as *mut freerdp_sys::SCARD_READERSTATEW).add(i);
                    let mut rgb_atr = [0u8; 36];
                    rgb_atr[..36].copy_from_slice(&state.rgbAtr[..36]);
                    returned_states.push(freerdp_sys::ReaderState_Return {
                        dwCurrentState: state.dwCurrentState,
                        dwEventState: state.dwEventState,
                        cbAtr: state.cbAtr,
                        rgbAtr: rgb_atr,
                    });
                } else {
                    let state = &*(rg_reader_states as *mut freerdp_sys::SCARD_READERSTATEA).add(i);
                    let mut rgb_atr = [0u8; 36];
                    rgb_atr[..36].copy_from_slice(&state.rgbAtr[..36]);
                    returned_states.push(freerdp_sys::ReaderState_Return {
                        dwCurrentState: state.dwCurrentState,
                        dwEventState: state.dwEventState,
                        cbAtr: state.cbAtr,
                        rgbAtr: rgb_atr,
                    });
                }
            }
        }
    }

    let ret = freerdp_sys::LocateCards_Return {
        ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
        cReaders: returned_states.len() as u32,
        rgReaderStates: returned_states.as_mut_ptr(),
    };

    unsafe {
        freerdp_sys::smartcard_pack_locate_cards_return(out, &ret);
    }

    SCARD_S_SUCCESS
}

fn handle_locate_cards_by_atr(
    scard: &SmartcardDevice,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ctx = {
        let contexts = scard.contexts.lock().unwrap();
        match contexts.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    let (atrs, reader_states_in) = unsafe {
        if unicode {
            let call = &operation.call.locateCardsByATRW;
            (
                get_atr_masks(call.rgAtrMasks, call.cAtrs),
                get_reader_states_in_w(call.rgReaderStates, call.cReaders),
            )
        } else {
            let call = &operation.call.locateCardsByATRA;
            (
                get_atr_masks(call.rgAtrMasks, call.cAtrs),
                get_reader_states_in_a(call.rgReaderStates, call.cReaders),
            )
        }
    };

    log::debug!(
        "smartcard: LOCATE_CARDS_BY_ATR context=0x{:X} atrs_count={} count={}",
        ctx_id,
        atrs.len(),
        reader_states_in.len()
    );

    match scard.integration.locate_cards_by_atr(&ctx, &atrs, &reader_states_in) {
        Ok(results) => {
            log::debug!("smartcard: locate_cards_by_atr success, count={}", results.len());
            let mut returned_states = results
                .iter()
                .map(|r| {
                    let rgb_atr = [0u8; 36];
                    let atr_len = 0;
                    
                    let mut dw_event_state = r.event_state;
                    if r.atr_match {
                        dw_event_state |= crate::integrations::smartcard::SCARD_STATE_ATRMATCH;
                    }

                    freerdp_sys::ReaderState_Return {
                        dwCurrentState: 0,
                        dwEventState: dw_event_state,
                        cbAtr: atr_len,
                        rgbAtr: rgb_atr,
                    }
                })
                .collect::<Vec<_>>();

            let ret = freerdp_sys::LocateCards_Return {
                ReturnCode: crate::integrations::smartcard::SCARD_S_SUCCESS as i32,
                cReaders: returned_states.len() as u32,
                rgReaderStates: returned_states.as_mut_ptr(),
            };

            unsafe {
                freerdp_sys::smartcard_pack_locate_cards_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn ioctl_name(ioctl: u32) -> &'static str {
    match ioctl {
        SCARD_IOCTL_ESTABLISHCONTEXT => "ESTABLISH_CONTEXT",
        SCARD_IOCTL_RELEASECONTEXT => "RELEASE_CONTEXT",
        SCARD_IOCTL_ISVALIDCONTEXT => "IS_VALID_CONTEXT",
        SCARD_IOCTL_CANCEL => "CANCEL",
        SCARD_IOCTL_ACCESSSTARTEDEVENT => "ACCESS_STARTED_EVENT",
        SCARD_IOCTL_RELEASETARTEDEVENT => "RELEASE_STARTED_EVENT",
        SCARD_IOCTL_LISTREADERGROUPSA | SCARD_IOCTL_LISTREADERSA => "LIST_READERS_A",
        SCARD_IOCTL_LISTREADERGROUPSW | SCARD_IOCTL_LISTREADERSW => "LIST_READERS_W",
        SCARD_IOCTL_CONNECTA => "CONNECT_A",
        SCARD_IOCTL_CONNECTW => "CONNECT_W",
        SCARD_IOCTL_RECONNECT => "RECONNECT",
        SCARD_IOCTL_DISCONNECT => "DISCONNECT",
        SCARD_IOCTL_TRANSMIT => "TRANSMIT",
        SCARD_IOCTL_STATUSA => "STATUS_A",
        SCARD_IOCTL_STATUSW => "STATUS_W",
        SCARD_IOCTL_GETSTATUSCHANGEA => "GET_STATUS_CHANGE_A",
        SCARD_IOCTL_GETSTATUSCHANGEW => "GET_STATUS_CHANGE_W",
        SCARD_IOCTL_BEGINTRANSACTION => "BEGIN_TRANSACTION",
        SCARD_IOCTL_ENDTRANSACTION => "END_TRANSACTION",
        SCARD_IOCTL_GETATTRIB => "GET_ATTRIB",
        SCARD_IOCTL_SETATTRIB => "SET_ATTRIB",
        SCARD_IOCTL_CONTROL => "CONTROL",
        SCARD_IOCTL_LOCATECARDSA => "LOCATE_CARDS_A",
        SCARD_IOCTL_LOCATECARDSW => "LOCATE_CARDS_W",
        SCARD_IOCTL_STATE => "STATE",
        SCARD_IOCTL_LOCATECARDSBYATRA => "LOCATE_CARDS_BY_ATR_A",
        SCARD_IOCTL_LOCATECARDSBYATRW => "LOCATE_CARDS_BY_ATR_W",
        _ => "UNKNOWN",
    }
}
