// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Device struct, threading, and IRP processing infrastructure.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use freerdp_sys::{CHANNEL_RC_OK, DEVICE, IRP, UINT};

use crate::integrations::SmartcardIntegration;
use crate::integrations::smartcard::{
    IRP_MJ_DEVICE_CONTROL, SCARD_S_SUCCESS, STATUS_BUFFER_TOO_SMALL, STATUS_NOT_SUPPORTED,
    STATUS_UNSUCCESSFUL, ScardContext,
};
use crate::utils::log;

use super::consts::ioctl_name;
use super::handlers::dispatch_ioctl;

// ---------------------------------------------------------------------------
// Threading Structures
// ---------------------------------------------------------------------------

/// Work items sent from the IRP handler to the device thread
pub(crate) enum IrpWork {
    Process(*mut IRP),
    Shutdown,
}

unsafe impl Send for IrpWork {}

/// Tracks outstanding IRPs for cancellation and cleanup
pub(crate) struct OutstandingIrpTracker {
    irps: Mutex<HashMap<u32, OutstandingIrp>>,
}

struct OutstandingIrp {
    #[allow(dead_code)]
    completion_id: u32,
    #[allow(dead_code)]
    started_at: Instant,
    cancelled: Arc<AtomicBool>,
}

impl OutstandingIrpTracker {
    pub(crate) fn new() -> Self {
        OutstandingIrpTracker {
            irps: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn register(&self, completion_id: u32) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut map = self.irps.lock().unwrap();
        map.insert(
            completion_id,
            OutstandingIrp {
                completion_id,
                started_at: Instant::now(),
                cancelled: cancelled.clone(),
            },
        );
        cancelled
    }

    pub(crate) fn complete(&self, completion_id: u32) {
        let mut map = self.irps.lock().unwrap();
        map.remove(&completion_id);
    }

    pub(crate) fn cancel_all(&self) {
        let map = self.irps.lock().unwrap();
        for irp in map.values() {
            irp.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

// ---------------------------------------------------------------------------
// Device Structure
// ---------------------------------------------------------------------------

#[repr(C)]
pub(crate) struct SmartcardDevice {
    /// Must be the first field — FreeRDP casts `DEVICE*` to access this.
    pub(crate) device: DEVICE,

    /// The consumer-provided smartcard implementation.
    pub(crate) integration: Arc<dyn SmartcardIntegration>,

    /// Registry of active contexts (ScardContext raw id → ContextEntry).
    pub(crate) contexts: Arc<Mutex<HashMap<u64, ContextEntry>>>,

    /// Channel to send IRPs to the device thread
    pub(crate) irp_tx: flume::Sender<IrpWork>,

    /// Handle to the device thread (for join on teardown)
    pub(crate) device_thread: Option<std::thread::JoinHandle<()>>,

    /// Tracker for outstanding IRPs
    pub(crate) outstanding: Arc<OutstandingIrpTracker>,
}

// SAFETY: SmartcardDevice is only accessed from FreeRDP callback threads
// and uses Mutex for internal state.
unsafe impl Send for SmartcardDevice {}
unsafe impl Sync for SmartcardDevice {}

pub(crate) struct ContextEntry {
    pub(crate) context: ScardContext,
}

// ---------------------------------------------------------------------------
// Raw pointer helpers
// ---------------------------------------------------------------------------

pub(crate) unsafe fn stream_write_u32(stream: *mut freerdp_sys::wStream, val: u32) {
    unsafe {
        let s = &mut *stream;
        let ptr = s.pointer;
        std::ptr::copy_nonoverlapping(val.to_le_bytes().as_ptr(), ptr, 4);
        s.pointer = s.pointer.add(4);
    }
}

pub(crate) unsafe fn u16_ptr_to_string(ptr: *const u16) -> String {
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

// ---------------------------------------------------------------------------
// Multi-string builders
// ---------------------------------------------------------------------------

pub(crate) fn build_multi_string_ascii(strings: &[String]) -> Vec<u8> {
    let mut result = Vec::new();
    for s in strings {
        result.extend_from_slice(s.as_bytes());
        result.push(0);
    }
    result.push(0);
    result
}

pub(crate) fn build_multi_string_utf16(strings: &[String]) -> Vec<u8> {
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

// ---------------------------------------------------------------------------
// IRP field extractors
// ---------------------------------------------------------------------------

pub(crate) fn get_card_handle(
    h_card: &freerdp_sys::REDIR_SCARDHANDLE,
) -> crate::integrations::smartcard::ScardHandle {
    let mut handle_bytes = [0u8; 8];
    handle_bytes.copy_from_slice(&h_card.pbHandle[..8]);
    let handle_id = u64::from_le_bytes(handle_bytes);
    crate::integrations::smartcard::ScardHandle::from_raw(handle_id, 0)
}

pub(crate) fn get_io_request(
    pci: freerdp_sys::LPSCARD_IO_REQUEST,
) -> crate::integrations::smartcard::ScardIORequest {
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

pub(crate) fn get_reader_states_in_a(
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

pub(crate) fn get_reader_states_in_w(
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

pub(crate) fn pack_reader_states_out(
    states_out: &[crate::integrations::smartcard::ReaderStateOut],
) -> Vec<freerdp_sys::ReaderState_Return> {
    states_out
        .iter()
        .map(|rs| {
            let mut rgb_atr = [0u8; 36];
            let atr_len = rs.atr.len().min(36);
            rgb_atr[..atr_len].copy_from_slice(&rs.atr[..atr_len]);
            freerdp_sys::ReaderState_Return {
                dwCurrentState: 0,
                dwEventState: rs.event_state,
                cbAtr: atr_len as u32,
                rgbAtr: rgb_atr,
            }
        })
        .collect()
}

pub(crate) fn get_atr_masks(
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

pub(crate) unsafe fn begin_response(out: *mut freerdp_sys::wStream) -> usize {
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

pub(crate) unsafe fn end_response(
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
            let ptr = (&mut *out).pointer;
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
            *p_io_status = STATUS_BUFFER_TOO_SMALL as i32;
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
// Device Thread
// ---------------------------------------------------------------------------

pub(crate) fn device_thread_main(
    irp_rx: flume::Receiver<IrpWork>,
    integration: Arc<dyn SmartcardIntegration>,
    contexts: Arc<Mutex<HashMap<u64, ContextEntry>>>,
    outstanding: Arc<OutstandingIrpTracker>,
) {
    log::info!("smartcard: device thread started");

    while let Ok(work) = irp_rx.recv() {
        match work {
            IrpWork::Shutdown => {
                log::info!("smartcard: device thread received shutdown signal");
                break;
            }
            IrpWork::Process(irp) => {
                unsafe { process_irp(irp, &integration, &contexts, &outstanding) };
            }
        }
    }

    log::info!("smartcard: device thread exiting");
}

unsafe fn process_irp(
    irp: *mut IRP,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    outstanding: &Arc<OutstandingIrpTracker>,
) {
    let mut operation: freerdp_sys::SMARTCARD_OPERATION = unsafe { std::mem::zeroed() };
    let decode_status = unsafe {
        freerdp_sys::smartcard_irp_device_control_decode(
            (*irp).input,
            (*irp).CompletionId,
            (*irp).FileId,
            &mut operation,
        )
    };

    if decode_status != SCARD_S_SUCCESS as i32 {
        log::error!("smartcard: failed to decode IRP: 0x{:08X}", decode_status);
        unsafe {
            let irp_ref = &mut *irp;
            irp_ref.IoStatus = STATUS_UNSUCCESSFUL as i32;
            if !irp_ref.output.is_null() {
                freerdp_sys::Stream_SetPosition(irp_ref.output, 16);
                stream_write_u32(irp_ref.output, 0);
                freerdp_sys::smartcard_pack_common_type_header(irp_ref.output);
                freerdp_sys::smartcard_pack_private_type_header(irp_ref.output, 0);
                stream_write_u32(irp_ref.output, decode_status as u32);
                freerdp_sys::Stream_SealLength(irp_ref.output);
            }
            if let Some(complete_fn) = irp_ref.Complete {
                complete_fn(irp);
            }
        }
        return;
    }

    let ioctl = operation.ioControlCode;
    let completion_id = operation.completionID;

    log::debug!(
        "smartcard: IRP ioctl=0x{:08X} ({}) completion_id={}",
        ioctl,
        ioctl_name(ioctl),
        completion_id
    );

    let _cancel_token = outstanding.register(completion_id);

    let mut io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
    let body_pos = unsafe { begin_response((*irp).output) };

    let return_code = dispatch_ioctl(ioctl, &operation, integration, contexts, unsafe {
        (*irp).output
    });

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
        freerdp_sys::smartcard_operation_free(&mut operation, 0);
    }

    outstanding.complete(completion_id);
}

// ---------------------------------------------------------------------------
// Init Handler
// ---------------------------------------------------------------------------

pub(crate) unsafe extern "C" fn init_handler(device: *mut DEVICE) -> UINT {
    if device.is_null() {
        return freerdp_sys::ERROR_INVALID_PARAMETER;
    }
    log::info!("smartcard: init_handler called");
    let scard = unsafe { &mut *(device as *mut SmartcardDevice) };
    let mut ctx_map = scard.contexts.lock().unwrap();
    for (_, entry) in ctx_map.drain() {
        let _ = scard.integration.release_context(&entry.context);
    }
    freerdp_sys::CHANNEL_RC_OK
}

// ---------------------------------------------------------------------------
// Free Handler
// ---------------------------------------------------------------------------

pub(crate) unsafe extern "C" fn free_handler(device: *mut DEVICE) -> UINT {
    if device.is_null() {
        return CHANNEL_RC_OK;
    }

    log::info!("smartcard: free_handler — tearing down device");

    let mut scard = unsafe { Box::from_raw(device as *mut SmartcardDevice) };

    scard.outstanding.cancel_all();

    let _ = scard.irp_tx.send(IrpWork::Shutdown);

    if let Some(handle) = scard.device_thread.take() {
        log::debug!("smartcard: waiting for device thread to finish");
        if handle.join().is_err() {
            log::error!("smartcard: device thread panicked during shutdown");
        }
    }

    {
        let mut contexts = scard.contexts.lock().unwrap();
        for (ctx_id, entry) in contexts.drain() {
            log::debug!(
                "smartcard: releasing context 0x{:X} during teardown",
                ctx_id
            );
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

pub(crate) unsafe extern "C" fn irp_request_handler(device: *mut DEVICE, irp: *mut IRP) -> UINT {
    if device.is_null() || irp.is_null() {
        log::error!("smartcard: irp_request_handler called with null pointer(s)");
        return CHANNEL_RC_OK;
    }

    if unsafe { (*irp).MajorFunction } != IRP_MJ_DEVICE_CONTROL {
        log::warn!(
            "smartcard: unexpected MajorFunction 0x{:08X}, completing with STATUS_NOT_SUPPORTED",
            unsafe { (*irp).MajorFunction }
        );
        unsafe {
            let irp_ref = &mut *irp;
            irp_ref.IoStatus = STATUS_NOT_SUPPORTED as i32;
            if let Some(complete_fn) = irp_ref.Complete {
                complete_fn(irp);
            }
        }
        return CHANNEL_RC_OK;
    }

    let scard = unsafe { &*(device as *const SmartcardDevice) };

    if let Err(e) = scard.irp_tx.send(IrpWork::Process(irp)) {
        log::error!("smartcard: failed to enqueue IRP: {}", e);
        unsafe {
            let irp_ref = &mut *irp;
            irp_ref.IoStatus = STATUS_UNSUCCESSFUL as i32;
            if let Some(complete_fn) = irp_ref.Complete {
                complete_fn(irp);
            }
        }
    }

    CHANNEL_RC_OK
}

// (re-exports handled via pub(crate) on the functions above)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::smartcard::tests::dummy::DummySmartcardHandle;

    #[test]
    fn outstanding_tracker_register_and_complete() {
        let tracker = OutstandingIrpTracker::new();

        let token1 = tracker.register(1);
        let token2 = tracker.register(2);

        assert!(!token1.load(Ordering::SeqCst));
        assert!(!token2.load(Ordering::SeqCst));

        tracker.complete(1);
        tracker.complete(2);
    }

    #[test]
    fn outstanding_tracker_cancel_all() {
        let tracker = OutstandingIrpTracker::new();

        let token1 = tracker.register(10);
        let token2 = tracker.register(20);
        let token3 = tracker.register(30);

        tracker.cancel_all();

        assert!(token1.load(Ordering::SeqCst));
        assert!(token2.load(Ordering::SeqCst));
        assert!(token3.load(Ordering::SeqCst));
    }

    #[test]
    fn outstanding_tracker_complete_removes_entry() {
        let tracker = OutstandingIrpTracker::new();

        let _token = tracker.register(42);
        tracker.complete(42);

        let map = tracker.irps.lock().unwrap();
        assert!(!map.contains_key(&42));
    }

    #[test]
    fn outstanding_tracker_cancel_empty_is_noop() {
        let tracker = OutstandingIrpTracker::new();
        tracker.cancel_all();
    }

    #[test]
    fn device_thread_processes_work() {
        let (tx, rx) = flume::unbounded::<IrpWork>();
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
        let outstanding = Arc::new(OutstandingIrpTracker::new());

        let integration_clone = integration.clone();
        let contexts_clone = contexts.clone();
        let outstanding_clone = outstanding.clone();

        let handle = std::thread::spawn(move || {
            device_thread_main(rx, integration_clone, contexts_clone, outstanding_clone);
        });

        tx.send(IrpWork::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn device_thread_shutdown_on_empty_queue() {
        let (tx, rx) = flume::unbounded::<IrpWork>();
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
        let outstanding = Arc::new(OutstandingIrpTracker::new());

        let integration_clone = integration.clone();
        let contexts_clone = contexts.clone();
        let outstanding_clone = outstanding.clone();

        let handle = std::thread::spawn(move || {
            device_thread_main(rx, integration_clone, contexts_clone, outstanding_clone);
        });

        std::thread::sleep(std::time::Duration::from_millis(10));
        tx.send(IrpWork::Shutdown).unwrap();

        let result = handle.join();
        assert!(result.is_ok());
    }
}
