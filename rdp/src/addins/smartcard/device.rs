// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com
//          Janier Rodríguez, jrodriguez at virtualcable dot es

//! Device struct, threading, and IRP processing infrastructure.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use freerdp_sys::{CHANNEL_RC_OK, DEVICE, IRP, UINT};

use crate::integrations::SmartcardIntegration;
use crate::integrations::smartcard::{
    IRP_MJ_DEVICE_CONTROL, SCARD_E_INVALID_HANDLE, SCARD_S_SUCCESS, STATUS_BUFFER_TOO_SMALL,
    STATUS_NOT_SUPPORTED, STATUS_UNSUCCESSFUL, ScardContext,
};
use crate::utils::log;

use super::consts::ioctl_name;
use super::handlers::{dispatch_ioctl, pack_empty_return};

// ---------------------------------------------------------------------------
// Threading Structures
// ---------------------------------------------------------------------------

/// Work items sent from the IRP handler to the device thread
pub(crate) enum IrpWork {
    Process(*mut IRP),
    Shutdown,
}

unsafe impl Send for IrpWork {}

/// A decoded operation handed to a context worker thread.
pub(crate) struct ContextWork {
    irp: *mut IRP,
    operation: freerdp_sys::SMARTCARD_OPERATION,
}

unsafe impl Send for ContextWork {}

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
}

// SAFETY: SmartcardDevice is only accessed from FreeRDP callback threads
// and uses Mutex for internal state.
unsafe impl Send for SmartcardDevice {}
unsafe impl Sync for SmartcardDevice {}

/// One worker thread per context, as in FreeRDP's `smartcard_context_thread`:
/// a blocking BEGIN_TRANSACTION must not stall operations of other contexts.
pub(crate) struct ContextEntry {
    pub(crate) context: ScardContext,
    pub(crate) work_tx: flume::Sender<ContextWork>,
    pub(crate) worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) cancelled: Arc<AtomicBool>,
}

impl ContextEntry {
    pub(crate) fn spawn(
        context: ScardContext,
        integration: Arc<dyn SmartcardIntegration>,
        contexts: Arc<Mutex<HashMap<u64, ContextEntry>>>,
    ) -> Self {
        let (work_tx, work_rx) = flume::unbounded::<ContextWork>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = std::thread::Builder::new()
            .name(format!("smartcard-ctx-{:x}", context.raw()))
            .spawn({
                let cancelled = cancelled.clone();
                move || context_thread_main(work_rx, integration, contexts, cancelled)
            })
            .ok();
        ContextEntry {
            context,
            work_tx,
            worker,
            cancelled,
        }
    }

    /// Stops the worker: pending IRPs are discarded, the in-flight one is
    /// cancelled through the integration and awaited.
    pub(crate) fn shutdown(mut self, integration: &Arc<dyn SmartcardIntegration>) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Err(code) = integration.cancel(&self.context) {
            log::warn!(
                "smartcard: failed to cancel context 0x{:X}: 0x{:08X}",
                self.context.raw(),
                code
            );
        }
        drop(self.work_tx);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!(
                "smartcard: worker of context 0x{:X} panicked",
                self.context.raw()
            );
        }
    }
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
                dwCurrentState: rs.current_state,
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

pub(crate) unsafe fn stream_position(stream: *mut freerdp_sys::wStream) -> usize {
    unsafe { (*stream).pointer.offset_from((*stream).buffer) as usize }
}

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
) {
    log::info!("smartcard: device thread started");

    while let Ok(work) = irp_rx.recv() {
        match work {
            IrpWork::Shutdown => {
                log::info!("smartcard: device thread received shutdown signal");
                break;
            }
            IrpWork::Process(irp) => {
                unsafe { process_irp(irp, &integration, &contexts) };
            }
        }
    }

    log::info!("smartcard: device thread exiting");
}

fn context_thread_main(
    work_rx: flume::Receiver<ContextWork>,
    integration: Arc<dyn SmartcardIntegration>,
    contexts: Arc<Mutex<HashMap<u64, ContextEntry>>>,
    cancelled: Arc<AtomicBool>,
) {
    // recv keeps delivering what was queued before the sender dropped, so the
    // flag is what tells a released context's leftovers apart from live work.
    while let Ok(work) = work_rx.recv() {
        if cancelled.load(Ordering::SeqCst) {
            unsafe { discard_operation(work) };
        } else {
            unsafe { run_operation(work, &integration, &contexts) };
        }
    }
}

fn runs_on_device_thread(ioctl: u32) -> bool {
    use crate::addins::smartcard::consts::*;
    matches!(
        ioctl,
        SCARD_IOCTL_ESTABLISHCONTEXT
            | SCARD_IOCTL_RELEASECONTEXT
            | SCARD_IOCTL_ISVALIDCONTEXT
            | SCARD_IOCTL_CANCEL
            | SCARD_IOCTL_ACCESSSTARTEDEVENT
            | SCARD_IOCTL_RELEASETARTEDEVENT
    )
}

unsafe fn process_irp(
    irp: *mut IRP,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
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

    // Reduce log noise: GET_STATUS_CHANGE floods the log every 50ms
    let is_status_change = ioctl == crate::addins::smartcard::consts::SCARD_IOCTL_GETSTATUSCHANGEA
        || ioctl == crate::addins::smartcard::consts::SCARD_IOCTL_GETSTATUSCHANGEW;
    if is_status_change {
        log::trace!(
            "smartcard: IRP ioctl=0x{:08X} ({}) completion_id={}",
            ioctl,
            ioctl_name(ioctl),
            completion_id
        );
    } else {
        log::trace!(
            "smartcard: IRP ioctl=0x{:08X} ({}) completion_id={}",
            ioctl,
            ioctl_name(ioctl),
            completion_id
        );
    }

    let work = ContextWork { irp, operation };

    if runs_on_device_thread(ioctl) {
        unsafe { run_operation(work, integration, contexts) };
        return;
    }

    let posted = {
        let ctx_map = contexts.lock().unwrap();
        match ctx_map.get(&(work.operation.hContext as u64)) {
            Some(entry) => entry.work_tx.send(work).map_err(|e| e.into_inner()),
            None => Err(work),
        }
    };
    if let Err(work) = posted {
        // Unknown context: nothing can serve it, answer inline (FreeRDP does the same).
        unsafe { run_operation(work, integration, contexts) };
    }
}

unsafe fn run_operation(
    work: ContextWork,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
) {
    let ContextWork { irp, mut operation } = work;
    let ioctl = operation.ioControlCode;

    let mut io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
    let body_pos = unsafe { begin_response((*irp).output) };

    let return_code = dispatch_ioctl(ioctl, &operation, integration, contexts, unsafe {
        (*irp).output
    });

    unsafe {
        if return_code != SCARD_S_SUCCESS && stream_position((*irp).output) == body_pos {
            pack_empty_return(ioctl, return_code, &operation, (*irp).output);
        }
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
}

unsafe fn discard_operation(work: ContextWork) {
    let ContextWork { irp, mut operation } = work;
    log::debug!(
        "smartcard: discarding queued {} for released context 0x{:X}",
        ioctl_name(operation.ioControlCode),
        operation.hContext
    );
    unsafe {
        match (*irp).Discard {
            Some(discard_fn) => {
                discard_fn(irp);
            }
            None => {
                let mut io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
                let body_pos = begin_response((*irp).output);
                end_response(
                    (*irp).output,
                    body_pos,
                    SCARD_E_INVALID_HANDLE as i32,
                    operation.outputBufferLength,
                    &mut io_status,
                );
                (*irp).IoStatus = io_status;
                if let Some(complete_fn) = (*irp).Complete {
                    complete_fn(irp);
                }
            }
        }
        freerdp_sys::smartcard_operation_free(&mut operation, 0);
    }
}

// ---------------------------------------------------------------------------
// Init Handler
// ---------------------------------------------------------------------------

pub(crate) unsafe extern "C" fn init_handler(device: *mut DEVICE) -> UINT {
    if device.is_null() {
        return freerdp_sys::ERROR_INVALID_PARAMETER;
    }
    log::debug!("smartcard: init_handler called");
    let scard = unsafe { &mut *(device as *mut SmartcardDevice) };
    shutdown_all_contexts(&scard.integration, &scard.contexts);
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

    // Un in-flight GET_STATUS_CHANGE may be blocked inside the integration. Take
    // a snapshot first so backend cancellation does not run while holding the
    // contexts mutex, then wait for the device thread to finish safely.
    shutdown_all_contexts(&scard.integration, &scard.contexts);

    let _ = scard.irp_tx.send(IrpWork::Shutdown);

    if let Some(handle) = scard.device_thread.take() {
        log::trace!("smartcard: waiting for device thread to finish");
        if handle.join().is_err() {
            log::error!("smartcard: device thread panicked during shutdown");
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

pub(crate) fn shutdown_all_contexts(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
) {
    let entries = {
        let mut ctx_map = contexts.lock().unwrap();
        ctx_map.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
    };
    for entry in entries {
        log::trace!(
            "smartcard: releasing context 0x{:X} during teardown",
            entry.context.raw()
        );
        let context = entry.context;
        entry.shutdown(integration);
        let _ = integration.release_context(&context);
    }
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
    fn device_thread_processes_work() {
        let (tx, rx) = flume::unbounded::<IrpWork>();
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));

        let integration_clone = integration.clone();
        let contexts_clone = contexts.clone();

        let handle = std::thread::spawn(move || {
            device_thread_main(rx, integration_clone, contexts_clone);
        });

        tx.send(IrpWork::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn device_thread_shutdown_on_empty_queue() {
        let (tx, rx) = flume::unbounded::<IrpWork>();
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));

        let integration_clone = integration.clone();
        let contexts_clone = contexts.clone();

        let handle = std::thread::spawn(move || {
            device_thread_main(rx, integration_clone, contexts_clone);
        });

        std::thread::sleep(std::time::Duration::from_millis(10));
        tx.send(IrpWork::Shutdown).unwrap();

        let result = handle.join();
        assert!(result.is_ok());
    }

    use std::cell::UnsafeCell;
    use std::ffi::CString;
    use std::time::Duration;

    use freerdp_sys::{
        Connect_Return, GetAttrib_Return, LocateCards_Return, REDIR_SCARDHANDLE,
        SCARD_READERSTATEA, SMARTCARD_OPERATION, Status_Return, Transmit_Return,
    };

    use crate::addins::smartcard::consts::*;
    use crate::integrations::smartcard::{
        SCARD_E_CANCELLED, SCARD_E_INSUFFICIENT_BUFFER, SCARD_E_TIMEOUT, SCARD_E_UNKNOWN_READER,
        SCARD_PROTOCOL_T0, SCARD_SHARE_SHARED, SCARD_STATE_CHANGED, SCARD_TRANSMIT_MAX,
    };

    const RDPDR_HEADER_LEN: usize = 16;
    const RESPONSE_HEADER_LEN: usize = 24;
    const RETURN_CODE_POS: usize = RDPDR_HEADER_LEN + RESPONSE_HEADER_LEN - 4;
    const BODY_POS: usize = RDPDR_HEADER_LEN + RESPONSE_HEADER_LEN;
    const OUTPUT_LEN: usize = 4096;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT: Duration = Duration::from_secs(5);
    const SETTLE: Duration = Duration::from_millis(250);

    #[repr(C)]
    struct TestIrp {
        irp: UnsafeCell<IRP>,
        label: &'static str,
        completed: flume::Sender<&'static str>,
    }

    // The IRP is handed to one worker at a time and read back only once it has completed.
    unsafe impl Send for TestIrp {}
    unsafe impl Sync for TestIrp {}

    unsafe extern "C" fn complete_test_irp(irp: *mut IRP) -> UINT {
        let test_irp = unsafe { &*(irp as *const TestIrp) };
        let _ = test_irp.completed.send(test_irp.label);
        CHANNEL_RC_OK
    }

    impl TestIrp {
        fn new(
            label: &'static str,
            completed: flume::Sender<&'static str>,
            capacity: usize,
        ) -> Self {
            let output = unsafe { freerdp_sys::Stream_New(std::ptr::null_mut(), capacity) };
            unsafe { freerdp_sys::Stream_SetPosition(output, RDPDR_HEADER_LEN) };
            let mut irp: IRP = unsafe { std::mem::zeroed() };
            irp.output = output;
            irp.Complete = Some(complete_test_irp);
            TestIrp {
                irp: UnsafeCell::new(irp),
                label,
                completed,
            }
        }

        fn raw(&self) -> *mut IRP {
            self.irp.get()
        }

        fn response(&self) -> &[u8] {
            unsafe {
                let out = &*(*self.raw()).output;
                std::slice::from_raw_parts(out.buffer, out.length)
            }
        }

        fn return_code(&self) -> u32 {
            read_u32(self.response(), RETURN_CODE_POS)
        }

        fn body(&self) -> &[u8] {
            &self.response()[BODY_POS..]
        }
    }

    impl Drop for TestIrp {
        fn drop(&mut self) {
            unsafe { freerdp_sys::Stream_Free((*self.raw()).output, 1) };
        }
    }

    fn read_u32(bytes: &[u8], pos: usize) -> u32 {
        u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap())
    }

    fn c_string(text: &str) -> *mut std::ffi::c_char {
        let owned = CString::new(text).unwrap();
        unsafe { freerdp_sys::_strdup(owned.as_ptr()) }
    }

    fn c_bytes(data: &[u8]) -> *mut u8 {
        unsafe {
            let ptr = freerdp_sys::malloc(data.len() as _) as *mut u8;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            ptr
        }
    }

    fn redir_handle(raw: u64) -> REDIR_SCARDHANDLE {
        REDIR_SCARDHANDLE {
            cbHandle: 8,
            pbHandle: raw.to_le_bytes(),
        }
    }

    fn operation(ioctl: u32, ctx: u64) -> SMARTCARD_OPERATION {
        let mut operation: SMARTCARD_OPERATION = unsafe { std::mem::zeroed() };
        operation.ioControlCode = ioctl;
        operation.hContext = ctx as freerdp_sys::SCARDCONTEXT;
        operation.outputBufferLength = OUTPUT_LEN as u32;
        operation
    }

    fn card_operation(ioctl: u32, ctx: u64, card: u64) -> SMARTCARD_OPERATION {
        let mut operation = operation(ioctl, ctx);
        operation.call.hCardAndDisposition.handles.hCard = redir_handle(card);
        operation
    }

    fn transmit_operation(
        ctx: u64,
        card: u64,
        recv_len: u32,
        recv_is_null: bool,
    ) -> SMARTCARD_OPERATION {
        let apdu = [0x00u8, 0xA4, 0x04, 0x00];
        let mut operation = card_operation(SCARD_IOCTL_TRANSMIT, ctx, card);
        let call = unsafe { &mut operation.call.transmit };
        call.pbSendBuffer = c_bytes(&apdu);
        call.cbSendLength = apdu.len() as u32;
        call.cbRecvLength = recv_len;
        call.fpbRecvBufferIsNULL = recv_is_null as i32;
        operation
    }

    fn connect_operation(ctx: u64, reader: &str) -> SMARTCARD_OPERATION {
        let mut operation = operation(SCARD_IOCTL_CONNECTA, ctx);
        let call = unsafe { &mut operation.call.connectA };
        call.szReader = c_string(reader);
        call.Common.dwShareMode = SCARD_SHARE_SHARED;
        call.Common.dwPreferredProtocols = SCARD_PROTOCOL_T0;
        operation
    }

    fn status_change_operation(ctx: u64, reader: &str, timeout_ms: u32) -> SMARTCARD_OPERATION {
        let mut operation = operation(SCARD_IOCTL_GETSTATUSCHANGEA, ctx);
        let states = unsafe {
            freerdp_sys::calloc(1, std::mem::size_of::<SCARD_READERSTATEA>() as _)
                as *mut SCARD_READERSTATEA
        };
        unsafe { (*states).szReader = c_string(reader) };
        let call = unsafe { &mut operation.call.getStatusChangeA };
        call.dwTimeOut = timeout_ms;
        call.cReaders = 1;
        call.rgReaderStates = states;
        operation
    }

    fn get_attrib_operation(ctx: u64, card: u64) -> SMARTCARD_OPERATION {
        let mut operation = card_operation(SCARD_IOCTL_GETATTRIB, ctx, card);
        let call = unsafe { &mut operation.call.getAttrib };
        call.dwAttrId = 0x0001_0100;
        call.cbAttrLen = 32;
        operation
    }

    /// Packs `ret` with `pack` into a scratch stream and returns the bytes written.
    fn packed_reference<T>(ret: T, pack: impl FnOnce(*mut freerdp_sys::wStream, &T)) -> Vec<u8> {
        unsafe {
            let stream = freerdp_sys::Stream_New(std::ptr::null_mut(), OUTPUT_LEN);
            pack(stream, &ret);
            freerdp_sys::Stream_SealLength(stream);
            let bytes = std::slice::from_raw_parts((*stream).buffer, (*stream).length).to_vec();
            freerdp_sys::Stream_Free(stream, 1);
            bytes
        }
    }

    struct Fixture {
        fake: Arc<DummySmartcardHandle>,
        integration: Arc<dyn SmartcardIntegration>,
        contexts: Arc<Mutex<HashMap<u64, ContextEntry>>>,
        completed_tx: flume::Sender<&'static str>,
        completed: flume::Receiver<&'static str>,
        /// Keeps every IRP alive until the workers are joined, even when an assertion fails.
        irps: Mutex<Vec<Arc<TestIrp>>>,
    }

    impl Fixture {
        fn new() -> Self {
            let fake = Arc::new(DummySmartcardHandle::new());
            fake.add_card("Reader A", vec![0x3B, 0x7F]);
            fake.add_card("Reader B", vec![0x3B, 0x7F]);
            let (completed_tx, completed) = flume::unbounded();
            Fixture {
                integration: fake.clone(),
                fake,
                contexts: Arc::new(Mutex::new(HashMap::new())),
                completed_tx,
                completed,
                irps: Mutex::new(Vec::new()),
            }
        }

        fn irp(&self, label: &'static str) -> Arc<TestIrp> {
            self.irp_with_capacity(label, OUTPUT_LEN)
        }

        fn irp_with_capacity(&self, label: &'static str, capacity: usize) -> Arc<TestIrp> {
            let irp = Arc::new(TestIrp::new(label, self.completed_tx.clone(), capacity));
            self.irps.lock().unwrap().push(irp.clone());
            irp
        }

        fn establish(&self) -> u64 {
            let ctx = self.integration.establish_context(0).unwrap();
            let entry = ContextEntry::spawn(ctx, self.integration.clone(), self.contexts.clone());
            self.contexts.lock().unwrap().insert(ctx.raw(), entry);
            ctx.raw()
        }

        fn connect(&self, ctx: u64, reader: &str) -> u64 {
            self.integration
                .connect(
                    &ScardContext::from_raw(ctx),
                    reader,
                    SCARD_SHARE_SHARED,
                    SCARD_PROTOCOL_T0,
                )
                .unwrap()
                .handle
                .raw()
        }

        /// Runs inline, as the device thread does for context-level IOCTLs.
        fn run(&self, irp: &TestIrp, operation: SMARTCARD_OPERATION) {
            let work = ContextWork {
                irp: irp.raw(),
                operation,
            };
            unsafe { run_operation(work, &self.integration, &self.contexts) };
        }

        /// Queues on the context worker, as the device thread does for card IOCTLs.
        fn post(&self, ctx: u64, irp: &TestIrp, operation: SMARTCARD_OPERATION) {
            let work = ContextWork {
                irp: irp.raw(),
                operation,
            };
            let ctx_map = self.contexts.lock().unwrap();
            ctx_map.get(&ctx).unwrap().work_tx.send(work).unwrap();
        }

        fn wait_completed(&self) -> &'static str {
            self.completed
                .recv_timeout(WAIT)
                .expect("IRP never completed")
        }

        fn assert_not_completed(&self, what: &str) {
            assert!(
                self.completed.recv_timeout(SETTLE).is_err(),
                "{what} completed although it should still be blocked"
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            shutdown_all_contexts(&self.integration, &self.contexts);
        }
    }

    #[test]
    fn transaction_on_one_context_does_not_stall_the_other_context() {
        let fx = Fixture::new();
        let ctx_a = fx.establish();
        let ctx_b = fx.establish();
        let card_a = fx.connect(ctx_a, "Reader A");
        let card_b = fx.connect(ctx_b, "Reader B");

        let begin_a = fx.irp("begin-a");
        fx.post(
            ctx_a,
            &begin_a,
            card_operation(SCARD_IOCTL_BEGINTRANSACTION, ctx_a, card_a),
        );
        assert_eq!(fx.wait_completed(), "begin-a");
        assert_eq!(begin_a.return_code(), SCARD_S_SUCCESS);

        let transmit_b = fx.irp("transmit-b");
        fx.post(
            ctx_b,
            &transmit_b,
            transmit_operation(ctx_b, card_b, 258, false),
        );
        fx.assert_not_completed("transmit on context B");

        let end_a = fx.irp("end-a");
        fx.post(
            ctx_a,
            &end_a,
            card_operation(SCARD_IOCTL_ENDTRANSACTION, ctx_a, card_a),
        );
        let mut order = vec![fx.wait_completed(), fx.wait_completed()];
        order.sort_unstable();
        assert_eq!(order, ["end-a", "transmit-b"]);
        assert_eq!(end_a.return_code(), SCARD_S_SUCCESS);
        assert_eq!(transmit_b.return_code(), SCARD_S_SUCCESS);
        assert!(transmit_b.body().ends_with(&[0x90, 0x00, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn cancel_interrupts_infinite_status_change_without_waiting_behind_queue() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        let card = fx.connect(ctx, "Reader A");
        fx.fake.set_status_change_timeouts(u32::MAX);

        let status_change = fx.irp("status-change");
        fx.post(
            ctx,
            &status_change,
            status_change_operation(ctx, "Reader A", INFINITE),
        );
        let transmit = fx.irp("transmit");
        fx.post(ctx, &transmit, transmit_operation(ctx, card, 258, false));
        fx.assert_not_completed("infinite status change");

        let cancel = fx.irp("cancel");
        fx.run(&cancel, operation(SCARD_IOCTL_CANCEL, ctx));
        assert_eq!(fx.wait_completed(), "cancel");
        assert_eq!(cancel.return_code(), SCARD_S_SUCCESS);

        assert_eq!(fx.wait_completed(), "status-change");
        assert_eq!(status_change.return_code(), SCARD_E_CANCELLED);
        assert_eq!(fx.wait_completed(), "transmit");
        assert_eq!(transmit.return_code(), SCARD_S_SUCCESS);
        assert!(
            fx.fake.status_change_calls() > 1,
            "expected polling in steps"
        );
    }

    #[test]
    fn release_context_cancels_in_flight_request_and_discards_queued_ones() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        let card = fx.connect(ctx, "Reader A");
        fx.fake.set_status_change_timeouts(u32::MAX);

        let status_change = fx.irp("status-change");
        fx.post(
            ctx,
            &status_change,
            status_change_operation(ctx, "Reader A", INFINITE),
        );
        let transmit = fx.irp("transmit");
        fx.post(ctx, &transmit, transmit_operation(ctx, card, 258, false));
        fx.assert_not_completed("infinite status change");

        let release = fx.irp("release");
        fx.run(&release, operation(SCARD_IOCTL_RELEASECONTEXT, ctx));
        assert_eq!(release.return_code(), SCARD_S_SUCCESS);

        let order = [
            fx.wait_completed(),
            fx.wait_completed(),
            fx.wait_completed(),
        ];
        assert_eq!(order, ["status-change", "transmit", "release"]);
        assert_eq!(status_change.return_code(), SCARD_E_CANCELLED);
        assert_eq!(transmit.return_code(), SCARD_E_INVALID_HANDLE);
        assert!(fx.contexts.lock().unwrap().is_empty());
        assert!(
            !fx.integration
                .is_valid_context(&ScardContext::from_raw(ctx))
        );
    }

    fn assert_failed_with_packed_return(irp: &TestIrp, code: u32, reference: &[u8]) {
        assert_eq!(irp.return_code(), code, "ReturnCode");
        assert!(!reference.is_empty(), "reference return structure is empty");
        assert_eq!(
            &irp.body()[..reference.len()],
            reference,
            "packed return structure"
        );
        assert_eq!(irp.body().len() % 8, 0, "body alignment");
    }

    #[test]
    fn failed_ioctls_still_pack_their_return_structure() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        let unknown_card = 0xDEAD_BEEF;
        let unknown_ctx = ctx + 1;

        let connect = fx.irp("connect");
        fx.run(&connect, connect_operation(ctx, "No such reader"));
        let reference = packed_reference(
            Connect_Return {
                ReturnCode: SCARD_E_UNKNOWN_READER as i32,
                ..unsafe { std::mem::zeroed() }
            },
            |s, ret| unsafe {
                freerdp_sys::smartcard_pack_connect_return(s, ret);
            },
        );
        assert_failed_with_packed_return(&connect, SCARD_E_UNKNOWN_READER, &reference);

        let transmit = fx.irp("transmit");
        fx.run(&transmit, transmit_operation(ctx, unknown_card, 258, false));
        let reference = packed_reference(
            Transmit_Return {
                ReturnCode: SCARD_E_INVALID_HANDLE as i32,
                ..unsafe { std::mem::zeroed() }
            },
            |s, ret| unsafe {
                freerdp_sys::smartcard_pack_transmit_return(s, ret);
            },
        );
        assert_failed_with_packed_return(&transmit, SCARD_E_INVALID_HANDLE, &reference);

        let get_attrib = fx.irp("get-attrib");
        let get_attrib_op = get_attrib_operation(ctx, unknown_card);
        let (attr_id, attr_len) = unsafe {
            (
                get_attrib_op.call.getAttrib.dwAttrId,
                get_attrib_op.call.getAttrib.cbAttrLen,
            )
        };
        fx.run(&get_attrib, get_attrib_op);
        let reference = packed_reference(
            GetAttrib_Return {
                ReturnCode: SCARD_E_INVALID_HANDLE as i32,
                ..unsafe { std::mem::zeroed() }
            },
            |s, ret| unsafe {
                freerdp_sys::smartcard_pack_get_attrib_return(s, ret, attr_id, attr_len);
            },
        );
        assert_failed_with_packed_return(&get_attrib, SCARD_E_INVALID_HANDLE, &reference);

        let status = fx.irp("status");
        fx.run(
            &status,
            card_operation(SCARD_IOCTL_STATUSA, ctx, unknown_card),
        );
        let reference = packed_reference(
            Status_Return {
                ReturnCode: SCARD_E_INVALID_HANDLE as i32,
                ..unsafe { std::mem::zeroed() }
            },
            |s, ret| unsafe {
                freerdp_sys::smartcard_pack_status_return(s, ret, 0);
            },
        );
        assert_failed_with_packed_return(&status, SCARD_E_INVALID_HANDLE, &reference);

        let status_change = fx.irp("status-change");
        fx.run(
            &status_change,
            status_change_operation(unknown_ctx, "Reader A", 0),
        );
        let reference = packed_reference(
            LocateCards_Return {
                ReturnCode: SCARD_E_INVALID_HANDLE as i32,
                ..unsafe { std::mem::zeroed() }
            },
            |s, ret| unsafe {
                freerdp_sys::smartcard_pack_get_status_change_return(s, ret, 0);
            },
        );
        assert_failed_with_packed_return(&status_change, SCARD_E_INVALID_HANDLE, &reference);
    }

    #[test]
    fn infinite_status_change_polls_until_a_change_is_reported() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        fx.fake.set_status_change_timeouts(3);

        let status_change = fx.irp("status-change");
        fx.post(
            ctx,
            &status_change,
            status_change_operation(ctx, "Reader A", INFINITE),
        );
        assert_eq!(fx.wait_completed(), "status-change");

        assert_eq!(status_change.return_code(), SCARD_S_SUCCESS);
        assert_eq!(fx.fake.status_change_calls(), 4);
        let body = status_change.body();
        assert_eq!(read_u32(body, 0), 1, "cReaders");
        assert_eq!(read_u32(body, 8), 1, "NDR array length");
        assert_ne!(read_u32(body, 16) & SCARD_STATE_CHANGED, 0, "dwEventState");
    }

    #[test]
    fn finite_status_change_times_out_after_the_requested_period() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        fx.fake.set_status_change_timeouts(u32::MAX);

        let status_change = fx.irp("status-change");
        fx.post(
            ctx,
            &status_change,
            status_change_operation(ctx, "Reader A", 150),
        );
        assert_eq!(fx.wait_completed(), "status-change");

        assert_eq!(status_change.return_code(), SCARD_E_TIMEOUT);
        assert_eq!(fx.fake.status_change_calls(), 2);
    }

    #[test]
    fn transmit_without_receive_buffer_returns_no_data_and_no_error() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        let card = fx.connect(ctx, "Reader A");
        fx.fake.set_transmit_response(vec![0xAB; 300]);

        let transmit = fx.irp("transmit");
        fx.run(&transmit, transmit_operation(ctx, card, 0, true));
        assert_eq!(transmit.return_code(), SCARD_S_SUCCESS);
        assert_eq!(read_u32(transmit.body(), 4), 0, "cbRecvLength");

        let too_small = fx.irp("too-small");
        fx.run(&too_small, transmit_operation(ctx, card, 100, false));
        assert_eq!(too_small.return_code(), SCARD_E_INSUFFICIENT_BUFFER);
    }

    #[test]
    fn transmit_receive_length_is_capped_at_the_protocol_maximum() {
        let fx = Fixture::new();
        let ctx = fx.establish();
        let card = fx.connect(ctx, "Reader A");
        let capacity = SCARD_TRANSMIT_MAX * 2;

        fx.fake
            .set_transmit_response(vec![0xAB; SCARD_TRANSMIT_MAX]);
        let at_limit = fx.irp_with_capacity("at-limit", capacity);
        let mut op = transmit_operation(ctx, card, u32::MAX, false);
        op.outputBufferLength = capacity as u32;
        fx.run(&at_limit, op);
        assert_eq!(at_limit.return_code(), SCARD_S_SUCCESS);
        assert_eq!(read_u32(at_limit.body(), 4), SCARD_TRANSMIT_MAX as u32);

        fx.fake
            .set_transmit_response(vec![0xAB; SCARD_TRANSMIT_MAX + 1]);
        let over_limit = fx.irp_with_capacity("over-limit", capacity);
        let mut op = transmit_operation(ctx, card, u32::MAX, false);
        op.outputBufferLength = capacity as u32;
        fx.run(&over_limit, op);
        assert_eq!(over_limit.return_code(), SCARD_E_INSUFFICIENT_BUFFER);
    }
}
