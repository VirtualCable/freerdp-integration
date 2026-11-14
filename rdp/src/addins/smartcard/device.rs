// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com
// Author: Andres Schumann, aschumann at virtualcable dot es

//! Device struct, threading, and IRP processing infrastructure.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use freerdp_sys::{CHANNEL_RC_OK, DEVICE, IRP, UINT};

use crate::integrations::SmartcardIntegration;
use crate::integrations::smartcard::{
    IRP_MJ_DEVICE_CONTROL, SCARD_E_CANCELLED, SCARD_E_NOT_READY, SCARD_S_SUCCESS,
    STATUS_BUFFER_TOO_SMALL, STATUS_NOT_SUPPORTED, STATUS_UNSUCCESSFUL, ScardContext,
};
use crate::utils::log;

use super::consts::{
    SCARD_IOCTL_GETSTATUSCHANGEA, SCARD_IOCTL_GETSTATUSCHANGEW, SCARD_IOCTL_RELEASECONTEXT,
    ioctl_name,
};
use super::handlers::{CardCache, dispatch_ioctl};
use super::transactions::{TRANSACTION_IDLE_TIMEOUT, TransactionGate};

// ---------------------------------------------------------------------------
// Threading Structures
// ---------------------------------------------------------------------------

/// Work items sent from the IRP handler to the device thread
pub(crate) enum IrpWork {
    Process(*mut IRP),
    Shutdown,
}

unsafe impl Send for IrpWork {}

/// Per-device IRP queue depth bound (printer-class sibling). The host can
/// stream IRPs at wire rate against a slow pcsc backend; an unbounded queue
/// lets a malicious pooled host grow per-session IRP/PDU retention without
/// limit (the IRP's retained input stream is only released at Complete). A
/// bounded queue turns that into real backpressure: when full, the IRP is
/// completed synchronously with SCARD_E_NOT_READY instead of being queued.
pub(crate) const SCARD_IRP_QUEUE_DEPTH: usize = 256;

/// Tracks outstanding IRPs for cancellation and cleanup
pub(crate) struct OutstandingIrpTracker {
    irps: Mutex<HashMap<u32, OutstandingIrp>>,
}

/// Device-liveness gate shared by every IRP-executing thread (device thread
/// and detached status workers). `free_handler` sets `alive = false` before
/// tearing down; `execute_request` checks it before touching the IRP's
/// output stream / Complete, so no thread Completes an IRP after teardown
/// (upstream frees the IRP/stream/devman right after Free).
pub(crate) struct DeviceLiveness {
    alive: AtomicBool,
}

impl DeviceLiveness {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            alive: AtomicBool::new(true),
        })
    }
    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub(crate) fn mark_dead(&self) {
        self.alive.store(false, Ordering::Release);
    }
}

/// Registry of detached status-thread JoinHandles so `free_handler` can
/// join them (bounded) instead of leaving them detached.
pub(crate) struct StatusThreads {
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl StatusThreads {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            handles: Mutex::new(Vec::new()),
        })
    }
    fn push(&self, handle: std::thread::JoinHandle<()>) {
        let mut handles = self.handles.lock().unwrap();
        // Reap finished handles so a long-lived device does not accumulate
        // one entry per GET_STATUS_CHANGE IRP.
        handles.retain(|h| !h.is_finished());
        handles.push(handle);
    }
    /// Join all registered threads, bounded by `deadline`. Returns the
    /// number of threads still running when the bound expired.
    fn join_all(&self, deadline: Instant) -> usize {
        let handles: Vec<_> = self.handles.lock().unwrap().drain(..).collect();
        let mut still_running = 0;
        for handle in handles {
            while !handle.is_finished() {
                if Instant::now() >= deadline {
                    still_running += 1;
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        still_running
    }
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

    /// Waits until no IRP is outstanding; returns false if `timeout` expires first.
    pub(crate) fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.irps.lock().unwrap().is_empty() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
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

    /// Liveness gate: cleared by free_handler before teardown so no
    /// IRP-executing thread Completes an IRP afterwards.
    pub(crate) liveness: Arc<DeviceLiveness>,

    /// JoinHandles of detached status threads (joined on teardown).
    pub(crate) status_threads: Arc<StatusThreads>,

    /// Per-device READ_CACHE/WRITE_CACHE store (session-scoped, drained on
    /// teardown). Replaces the former process-global cache that leaked
    /// public-key material across sessions.
    pub(crate) card_cache: CardCache,
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
    liveness: Arc<DeviceLiveness>,
    status_threads: Arc<StatusThreads>,
    card_cache: CardCache,
) {
    log::info!("smartcard: device thread started");

    let mut gate = TransactionGate::new();

    loop {
        let work = match gate.time_to_expiry(Instant::now()) {
            Some(wait) => match irp_rx.recv_timeout(wait) {
                Ok(work) => Some(work),
                Err(flume::RecvTimeoutError::Timeout) => None,
                Err(flume::RecvTimeoutError::Disconnected) => break,
            },
            None => match irp_rx.recv() {
                Ok(work) => Some(work),
                Err(_) => break,
            },
        };

        // Expire first, so a request that arrives right at the deadline does not
        // revive a transaction that has already timed out.
        if let Some(card) = gate.expire_idle(Instant::now()) {
            log::debug!(
                "smartcard: transaction of card=0x{:X} idle for more than {:?}, released",
                card,
                TRANSACTION_IDLE_TIMEOUT
            );
            unsafe {
                run_parked(
                    &mut gate,
                    &integration,
                    &contexts,
                    &outstanding,
                    &liveness,
                    &status_threads,
                    &card_cache,
                )
            };
        }

        match work {
            Some(IrpWork::Shutdown) => {
                log::info!("smartcard: device thread received shutdown signal");
                break;
            }
            Some(IrpWork::Process(irp)) => {
                if let Some(request) = unsafe { decode_irp(irp) } {
                    unsafe {
                        handle_request(
                            request,
                            &mut gate,
                            &integration,
                            &contexts,
                            &outstanding,
                            &liveness,
                            &status_threads,
                            &card_cache,
                        )
                    };
                }
            }
            None => {}
        }
    }

    // The channel is going away: like FreeRDP's own smartcard channel, drop the
    // parked IRPs instead of answering them.
    for request in gate.take_all() {
        unsafe { discard_request(request) };
    }

    log::info!("smartcard: device thread exiting");
}

/// A decoded IRP, ready to run now or to be parked until the card is free.
struct Request {
    irp: *mut IRP,
    operation: freerdp_sys::SMARTCARD_OPERATION,
}

// SAFETY: like `IrpWork`, the raw pointers belong to FreeRDP, which allows an IRP to be
// completed from any thread.
unsafe impl Send for Request {}

impl Request {
    fn ioctl(&self) -> u32 {
        self.operation.ioControlCode
    }

    fn card(&self) -> u64 {
        get_card_handle(unsafe { &self.operation.call.handles.hCard }).raw()
    }

    fn context(&self) -> u64 {
        self.operation.hContext as u64
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn handle_request(
    request: Request,
    gate: &mut TransactionGate<Request>,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    outstanding: &Arc<OutstandingIrpTracker>,
    liveness: &Arc<DeviceLiveness>,
    status_threads: &Arc<StatusThreads>,
    card_cache: &CardCache,
) {
    let (ioctl, card, context) = (request.ioctl(), request.card(), request.context());

    if is_status_change(ioctl) {
        unsafe {
            run_detached(
                request,
                integration,
                contexts,
                outstanding,
                liveness,
                status_threads,
                card_cache,
            )
        };
        return;
    }

    if gate.must_wait(ioctl, card) {
        log::trace!(
            "smartcard: {} card=0x{:X} parked, card held by 0x{:X}",
            ioctl_name(ioctl),
            card,
            gate.owner_card().unwrap_or_default()
        );
        gate.park(ioctl, card, context, request);
        return;
    }

    let result = unsafe {
        execute_request(
            request,
            integration,
            contexts,
            outstanding,
            liveness,
            card_cache,
        )
    };
    gate.on_completed(ioctl, card, context, result, Instant::now());

    // SCardCancel only interrupts blocking status waits, so a CANCEL leaves parked
    // requests alone; once the context is gone they can never run.
    if ioctl == SCARD_IOCTL_RELEASECONTEXT && result == SCARD_S_SUCCESS {
        for parked in gate.take_context(context) {
            unsafe { complete_without_running(parked, SCARD_E_CANCELLED, liveness) };
        }
    }

    unsafe {
        run_parked(
            gate,
            integration,
            contexts,
            outstanding,
            liveness,
            status_threads,
            card_cache,
        )
    };
}

fn is_status_change(ioctl: u32) -> bool {
    ioctl == SCARD_IOCTL_GETSTATUSCHANGEA || ioctl == SCARD_IOCTL_GETSTATUSCHANGEW
}

/// GET_STATUS_CHANGE waits inside the backend, so it gets its own thread: the device
/// thread keeps serving the other IRPs instead of queueing them behind the wait.
unsafe fn run_detached(
    request: Request,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    outstanding: &Arc<OutstandingIrpTracker>,
    liveness: &Arc<DeviceLiveness>,
    status_threads: &Arc<StatusThreads>,
    card_cache: &CardCache,
) {
    outstanding.register(request.operation.completionID);

    let slot = Arc::new(Mutex::new(Some(request)));
    let worker_slot = slot.clone();
    let (
        worker_integration,
        worker_contexts,
        worker_outstanding,
        worker_liveness,
        worker_card_cache,
    ) = (
        integration.clone(),
        contexts.clone(),
        outstanding.clone(),
        liveness.clone(),
        card_cache.clone(),
    );

    let spawned = std::thread::Builder::new()
        .name("scard-status".into())
        .spawn(move || {
            if let Some(request) = worker_slot.lock().unwrap().take() {
                unsafe {
                    execute_request(
                        request,
                        &worker_integration,
                        &worker_contexts,
                        &worker_outstanding,
                        &worker_liveness,
                        &worker_card_cache,
                    )
                };
            }
        });

    match spawned {
        Ok(handle) => status_threads.push(handle),
        Err(error) => {
            log::error!("smartcard: cannot start the status thread ({error}), running inline");
            if let Some(request) = slot.lock().unwrap().take() {
                unsafe {
                    execute_request(
                        request,
                        integration,
                        contexts,
                        outstanding,
                        liveness,
                        card_cache,
                    )
                };
            }
        }
    }
}

/// Runs the parked requests that no longer have to wait, in arrival order.
unsafe fn run_parked(
    gate: &mut TransactionGate<Request>,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    outstanding: &Arc<OutstandingIrpTracker>,
    liveness: &Arc<DeviceLiveness>,
    status_threads: &Arc<StatusThreads>,
    card_cache: &CardCache,
) {
    let _ = status_threads; // parked requests run inline on the device thread
    while let Some((ioctl, card, context, parked)) = gate.next_runnable() {
        log::trace!("smartcard: {} card=0x{:X} resumed", ioctl_name(ioctl), card);
        let result = unsafe {
            execute_request(
                parked,
                integration,
                contexts,
                outstanding,
                liveness,
                card_cache,
            )
        };
        gate.on_completed(ioctl, card, context, result, Instant::now());
    }
}

unsafe fn decode_irp(irp: *mut IRP) -> Option<Request> {
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
        return None;
    }

    Some(Request { irp, operation })
}

/// Completes a parked request with `return_code` without calling the integration.
unsafe fn complete_without_running(
    request: Request,
    return_code: u32,
    liveness: &Arc<DeviceLiveness>,
) {
    let Request { irp, mut operation } = request;
    if !liveness.is_alive() {
        log::warn!(
            "smartcard: dropping parked IRP completion_id={}: device torn down",
            operation.completionID
        );
        unsafe { freerdp_sys::smartcard_operation_free(&mut operation, 0) };
        return;
    }
    log::trace!(
        "smartcard: {} card=0x{:X} completed without running: 0x{:08X}",
        ioctl_name(operation.ioControlCode),
        get_card_handle(unsafe { &operation.call.handles.hCard }).raw(),
        return_code
    );
    unsafe {
        let mut io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
        let body_pos = begin_response((*irp).output);
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
        }
        freerdp_sys::smartcard_operation_free(&mut operation, 0);
    }
}

unsafe fn discard_request(request: Request) {
    let Request { irp, mut operation } = request;
    unsafe {
        freerdp_sys::smartcard_operation_free(&mut operation, 0);
        if let Some(discard_fn) = (*irp).Discard {
            discard_fn(irp);
        }
    }
}

unsafe fn execute_request(
    request: Request,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    outstanding: &Arc<OutstandingIrpTracker>,
    liveness: &Arc<DeviceLiveness>,
    card_cache: &CardCache,
) -> u32 {
    let Request { irp, mut operation } = request;
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

    let _cancel_token = outstanding.register(completion_id);

    // Teardown gate: if the device is already being freed, do NOT touch the
    // IRP's output stream or Complete — upstream may have freed them. Drop
    // the request (free only our decoded operation) and log.
    if !liveness.is_alive() {
        log::warn!(
            "smartcard: dropping IRP completion_id={} ({}): device torn down",
            completion_id,
            ioctl_name(ioctl)
        );
        unsafe { freerdp_sys::smartcard_operation_free(&mut operation, 0) };
        outstanding.complete(completion_id);
        return SCARD_E_CANCELLED;
    }

    let mut io_status = crate::integrations::smartcard::STATUS_SUCCESS as i32;
    let body_pos = unsafe { begin_response((*irp).output) };

    let return_code = dispatch_ioctl(
        ioctl,
        &operation,
        integration,
        contexts,
        card_cache,
        unsafe { (*irp).output },
    );

    // Re-check after the (possibly blocking) backend call: teardown may have
    // begun while we were inside the integration. If so, do not Complete.
    if !liveness.is_alive() {
        log::warn!(
            "smartcard: suppressing IRP completion_id={} ({}): device torn down during backend call",
            completion_id,
            ioctl_name(ioctl)
        );
        unsafe { freerdp_sys::smartcard_operation_free(&mut operation, 0) };
        outstanding.complete(completion_id);
        return SCARD_E_CANCELLED;
    }

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
    return_code
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

    // Mark the device dead FIRST: every IRP-executing thread (device thread
    // and detached status workers) checks this before touching the IRP's
    // output stream / Complete, so none can Complete after teardown begins.
    scard.liveness.mark_dead();

    // Un in-flight GET_STATUS_CHANGE may be blocked inside the integration. Take
    // a snapshot first so backend cancellation does not run while holding the
    // contexts mutex, then wait for the device thread to finish safely.
    let contexts_to_cancel = {
        let contexts = scard.contexts.lock().unwrap();
        contexts
            .values()
            .map(|entry| entry.context)
            .collect::<Vec<_>>()
    };
    for context in contexts_to_cancel {
        if let Err(code) = scard.integration.cancel(&context) {
            log::warn!(
                "smartcard: failed to cancel context 0x{:X} during teardown: 0x{:08X}",
                context.raw(),
                code
            );
        }
    }

    scard.outstanding.cancel_all();

    let _ = scard.irp_tx.send(IrpWork::Shutdown);

    if let Some(handle) = scard.device_thread.take() {
        log::trace!("smartcard: waiting for device thread to finish");
        if handle.join().is_err() {
            log::error!("smartcard: device thread panicked during shutdown");
        }
    }

    // Join the detached status threads (bounded). Their backend waits were
    // cancelled above (integration.cancel + cancel_all); a well-behaved
    // backend returns promptly. The liveness gate already prevents any
    // Complete after teardown, so even a thread that overruns the bound
    // cannot touch freed upstream state — it drops its request and exits.
    let status_deadline = Instant::now() + Duration::from_secs(5);
    let still_running = scard.status_threads.join_all(status_deadline);
    if still_running > 0 {
        log::warn!(
            "smartcard: {still_running} status thread(s) still running at teardown bound; \
             liveness gate prevents post-teardown IRP completion"
        );
    }

    if !scard.outstanding.wait_idle(Duration::from_secs(2)) {
        log::warn!("smartcard: status threads still running at teardown");
    }

    {
        let mut contexts = scard.contexts.lock().unwrap();
        for (ctx_id, entry) in contexts.drain() {
            log::trace!(
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

    // Bounded queue with real backpressure (printer-class sibling): an
    // unbounded send can never be Full, so a malicious pooled host could
    // stream IRPs at wire rate against a slow pcsc backend and grow
    // per-session IRP/PDU retention without limit. On overflow, complete the
    // IRP synchronously with a packed scard error (the caller is the rdpdr
    // plugin thread that would run completion anyway); returning non-OK alone
    // would leak the IRP on 3.26.
    match scard.irp_tx.try_send(IrpWork::Process(irp)) {
        Ok(()) => {}
        Err(flume::TrySendError::Full(_)) => {
            log::warn!("smartcard: IRP queue full, rejecting with SCARD_E_NOT_READY");
            unsafe { complete_irp_with_error(irp, SCARD_E_NOT_READY) };
        }
        Err(flume::TrySendError::Disconnected(_)) => {
            log::error!("smartcard: failed to enqueue IRP (channel disconnected)");
            unsafe {
                let irp_ref = &mut *irp;
                irp_ref.IoStatus = STATUS_UNSUCCESSFUL as i32;
                if let Some(complete_fn) = irp_ref.Complete {
                    complete_fn(irp);
                }
            }
        }
    }

    CHANNEL_RC_OK
}

/// Complete an IRP synchronously with a packed scard error body, mirroring the
/// decode-failure path. Used for queue-overflow backpressure so the host sees
/// a legitimate failure and upstream releases the retained input stream.
unsafe fn complete_irp_with_error(irp: *mut IRP, scard_error: u32) {
    unsafe {
        let irp_ref = &mut *irp;
        irp_ref.IoStatus = STATUS_UNSUCCESSFUL as i32;
        if !irp_ref.output.is_null() {
            freerdp_sys::Stream_SetPosition(irp_ref.output, 16);
            stream_write_u32(irp_ref.output, 0);
            freerdp_sys::smartcard_pack_common_type_header(irp_ref.output);
            freerdp_sys::smartcard_pack_private_type_header(irp_ref.output, 0);
            stream_write_u32(irp_ref.output, scard_error);
            freerdp_sys::Stream_SealLength(irp_ref.output);
        }
        if let Some(complete_fn) = irp_ref.Complete {
            complete_fn(irp);
        }
    }
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
    fn wait_idle_returns_once_everything_completed() {
        let tracker = Arc::new(OutstandingIrpTracker::new());
        tracker.register(7);

        let completer = tracker.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            completer.complete(7);
        });

        assert!(tracker.wait_idle(Duration::from_secs(2)));
        worker.join().unwrap();
    }

    #[test]
    fn wait_idle_gives_up_while_an_irp_is_outstanding() {
        let tracker = OutstandingIrpTracker::new();
        tracker.register(7);

        assert!(!tracker.wait_idle(Duration::from_millis(50)));
    }

    #[test]
    fn only_get_status_change_runs_detached() {
        use super::super::consts::{SCARD_IOCTL_ESTABLISHCONTEXT, SCARD_IOCTL_TRANSMIT};

        assert!(is_status_change(SCARD_IOCTL_GETSTATUSCHANGEA));
        assert!(is_status_change(SCARD_IOCTL_GETSTATUSCHANGEW));
        assert!(!is_status_change(SCARD_IOCTL_TRANSMIT));
        assert!(!is_status_change(SCARD_IOCTL_ESTABLISHCONTEXT));
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
            device_thread_main(
                rx,
                integration_clone,
                contexts_clone,
                outstanding_clone,
                DeviceLiveness::new(),
                StatusThreads::new(),
                new_card_cache(),
            );
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
            device_thread_main(
                rx,
                integration_clone,
                contexts_clone,
                outstanding_clone,
                DeviceLiveness::new(),
                StatusThreads::new(),
                new_card_cache(),
            );
        });

        std::thread::sleep(std::time::Duration::from_millis(10));
        tx.send(IrpWork::Shutdown).unwrap();

        let result = handle.join();
        assert!(result.is_ok());
    }

    // ── Characterization: detached status-thread IRP contract ────────────
    //
    // These pin the CURRENT observable behavior of the GET_STATUS_CHANGE
    // detached path (run_detached -> execute_request) as the host/consumer
    // sees it: the IRP is Completed exactly once with the backend's return
    // code, the output stream is a valid packed response, and the tracker
    // entry is removed. Test-only: the lifetime fix (Fix C) must preserve
    // all of this while adding quiesce-on-teardown.

    use super::super::handlers::new_card_cache;
    use crate::integrations::smartcard::{ReaderStateIn, ReaderStateOut, ScardContext};
    use std::sync::Condvar;

    /// Per-IRP completion sink, recovered from the IRP's `device` pointer
    /// (a leaked Box<IrpSink>) so Complete/Discard calls from any thread
    /// land in the right test. No globals.
    struct IrpSink {
        completed: Mutex<Vec<u32>>, // completion ids
        discarded: Mutex<Vec<u32>>,
    }

    unsafe extern "C" fn fake_complete(irp: *mut IRP) -> UINT {
        let sink = unsafe { &*((*irp).device as *const IrpSink) };
        sink.completed
            .lock()
            .unwrap()
            .push(unsafe { (*irp).CompletionId });
        CHANNEL_RC_OK
    }

    unsafe extern "C" fn fake_discard(irp: *mut IRP) -> UINT {
        let sink = unsafe { &*((*irp).device as *const IrpSink) };
        sink.discarded
            .lock()
            .unwrap()
            .push(unsafe { (*irp).CompletionId });
        CHANNEL_RC_OK
    }

    /// A fake IRP with a real output stream (begin/end_response write into
    /// it) and Complete/Discard that record into the sink. The operation is
    /// built by the caller; `input` is unused by execute_request.
    struct FakeIrp {
        irp: *mut IRP,
        sink: *mut IrpSink,
        output: *mut freerdp_sys::wStream,
    }

    impl FakeIrp {
        fn new(completion_id: u32) -> Self {
            let sink = Box::into_raw(Box::new(IrpSink {
                completed: Mutex::new(Vec::new()),
                discarded: Mutex::new(Vec::new()),
            }));
            let output = unsafe { freerdp_sys::Stream_New(std::ptr::null_mut(), 4096) };
            assert!(!output.is_null());
            let irp = Box::into_raw(Box::new(unsafe { std::mem::zeroed::<IRP>() }));
            unsafe {
                (*irp).CompletionId = completion_id;
                (*irp).device = sink as *mut DEVICE;
                (*irp).output = output;
                (*irp).Complete = Some(fake_complete);
                (*irp).Discard = Some(fake_discard);
            }
            Self { irp, sink, output }
        }
        fn completed(&self) -> Vec<u32> {
            unsafe { (*self.sink).completed.lock().unwrap().clone() }
        }
        fn discarded(&self) -> Vec<u32> {
            unsafe { (*self.sink).discarded.lock().unwrap().clone() }
        }
    }

    impl Drop for FakeIrp {
        fn drop(&mut self) {
            unsafe {
                freerdp_sys::Stream_Free(self.output, 1);
                let _ = Box::from_raw(self.irp);
                let _ = Box::from_raw(self.sink);
            }
        }
    }

    /// Build a GET_STATUS_CHANGE Request with a valid context registered
    /// and one reader state (the host always sends >= 1; a zero-reader
    /// response is too small for end_response's NDR header arithmetic).
    /// The reader-state allocations (strdup'd name + calloc'd array) are
    /// reclaimed by smartcard_operation_free after execute_request packs
    /// the response, mirroring the upstream decoder's ownership.
    fn status_request(
        fake: &FakeIrp,
        contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
        completion_id: u32,
        timeout_ms: u32,
    ) -> Request {
        let ctx = ScardContext::new();
        contexts
            .lock()
            .unwrap()
            .insert(ctx.raw(), ContextEntry { context: ctx });
        let mut operation: freerdp_sys::SMARTCARD_OPERATION = unsafe { std::mem::zeroed() };
        operation.ioControlCode = SCARD_IOCTL_GETSTATUSCHANGEA;
        operation.completionID = completion_id;
        operation.hContext = ctx.raw() as usize;
        operation.outputBufferLength = 4096;
        // One reader state, allocated like the upstream decoder does
        // (strdup'd name + calloc'd array): smartcard_operation_free will
        // free both after the response is packed.
        unsafe {
            let name = freerdp_sys::_strdup(c"TestReader".as_ptr());
            let states = freerdp_sys::calloc(
                1,
                std::mem::size_of::<freerdp_sys::SCARD_READERSTATEA>() as u64,
            ) as *mut freerdp_sys::SCARD_READERSTATEA;
            (*states).szReader = name;
            (*states).dwCurrentState = 0;
            operation.call.getStatusChangeA.dwTimeOut = timeout_ms;
            operation.call.getStatusChangeA.cReaders = 1;
            operation.call.getStatusChangeA.rgReaderStates = states;
        }
        Request {
            irp: fake.irp,
            operation,
        }
    }

    #[test]
    fn execute_request_completes_status_irp_once_with_backend_code() {
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
        let outstanding = Arc::new(OutstandingIrpTracker::new());
        let fake = FakeIrp::new(100);
        let request = status_request(&fake, &contexts, 100, 0);

        let rc = unsafe {
            execute_request(
                request,
                &integration,
                &contexts,
                &outstanding,
                &DeviceLiveness::new(),
                &new_card_cache(),
            )
        };
        assert_eq!(rc, SCARD_S_SUCCESS);
        assert_eq!(fake.completed(), vec![100]);
        assert!(fake.discarded().is_empty());
        // tracker entry removed
        assert!(outstanding.irps.lock().unwrap().is_empty());
        // output stream sealed with a non-trivial packed response
        let len = unsafe { (*fake.output).length };
        assert!(len > 24, "packed response must exceed the 24-byte header");
    }

    #[test]
    fn run_detached_completes_status_irp_on_its_own_thread() {
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
        let outstanding = Arc::new(OutstandingIrpTracker::new());
        let fake = FakeIrp::new(200);
        let request = status_request(&fake, &contexts, 200, 0);

        unsafe {
            run_detached(
                request,
                &integration,
                &contexts,
                &outstanding,
                &DeviceLiveness::new(),
                &StatusThreads::new(),
                &new_card_cache(),
            )
        };
        // The detached thread completes the IRP; wait for it (bounded).
        let deadline = Instant::now() + Duration::from_secs(5);
        while fake.completed().is_empty() {
            assert!(Instant::now() < deadline, "detached thread never completed");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fake.completed(), vec![200]);
        assert!(outstanding.wait_idle(Duration::from_secs(2)));
    }

    // NOTE: error paths of handle_get_status_change (invalid context or
    // backend Err) leave the response body empty, which underflows
    // end_response's NDR header arithmetic in debug builds. That is a
    // separate pre-existing defect, out of scope for Fix C; no
    // characterization test pins it here.

    // ── Lifetime guarantee (Fix C: status threads quiesced on teardown) ──
    //
    // NEW safety contract: free_handler must not return while a detached
    // status thread can still Complete(irp) — upstream frees the IRP/stream
    // /devman right after Free. Deterministic and memory-safe: the backend
    // get_status_change parks on a condvar; the fake IRP's Complete records
    // into a sink that outlives the device.
    //
    // Against UNFIXED code this FAILS (free_handler returns after the 2 s
    // wait_idle while the status thread is still parked, then the released
    // thread Completes a torn-down IRP); against the fix it PASSES.

    /// Integration whose get_status_change blocks until released, counting
    /// how many waits are in flight. All other methods delegate to a dummy.
    #[derive(Debug)]
    struct BlockingStatusIntegration {
        dummy: DummySmartcardHandle,
        entered: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
        inflight: Arc<std::sync::atomic::AtomicU32>,
    }

    impl SmartcardIntegration for BlockingStatusIntegration {
        fn establish_context(&self, scope: u32) -> Result<ScardContext, u32> {
            self.dummy.establish_context(scope)
        }
        fn release_context(&self, ctx: &ScardContext) -> Result<(), u32> {
            self.dummy.release_context(ctx)
        }
        fn is_valid_context(&self, ctx: &ScardContext) -> bool {
            self.dummy.is_valid_context(ctx)
        }
        fn list_readers(
            &self,
            ctx: &ScardContext,
            groups: Option<&[String]>,
        ) -> Result<Vec<String>, u32> {
            self.dummy.list_readers(ctx, groups)
        }
        fn connect(
            &self,
            ctx: &ScardContext,
            reader: &str,
            share_mode: u32,
            preferred_protocols: u32,
        ) -> Result<crate::integrations::smartcard::ConnectResult, u32> {
            self.dummy
                .connect(ctx, reader, share_mode, preferred_protocols)
        }
        fn disconnect(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            disposition: u32,
        ) -> Result<(), u32> {
            self.dummy.disconnect(handle, disposition)
        }
        fn reconnect(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            share_mode: u32,
            preferred_protocols: u32,
            initialization: u32,
        ) -> Result<u32, u32> {
            self.dummy
                .reconnect(handle, share_mode, preferred_protocols, initialization)
        }
        fn transmit(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            send_pci: &crate::integrations::smartcard::ScardIORequest,
            data: &[u8],
        ) -> Result<crate::integrations::smartcard::TransmitResult, u32> {
            self.dummy.transmit(handle, send_pci, data)
        }
        fn control(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            control_code: u32,
            in_data: &[u8],
        ) -> Result<Vec<u8>, u32> {
            self.dummy.control(handle, control_code, in_data)
        }
        fn status(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
        ) -> Result<crate::integrations::smartcard::ScardStatus, u32> {
            self.dummy.status(handle)
        }
        fn get_status_change(
            &self,
            _ctx: &ScardContext,
            _timeout: Duration,
            _reader_states: &[ReaderStateIn],
        ) -> Result<(Vec<ReaderStateOut>, u32), u32> {
            self.inflight.fetch_add(1, Ordering::SeqCst);
            let (lock, cvar) = &*self.entered;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            let (rlock, rcvar) = &*self.release;
            let mut released = rlock.lock().unwrap();
            while !*released {
                released = rcvar.wait(released).unwrap();
            }
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            // Return one reader state so the packed response body is large
            // enough for end_response's NDR header arithmetic (a zero-reader
            // body underflows it — separate pre-existing defect).
            Ok((
                vec![ReaderStateOut {
                    reader_name: "TestReader".to_string(),
                    current_state: 0,
                    event_state: 0,
                    atr: Vec::new(),
                }],
                SCARD_S_SUCCESS,
            ))
        }
        fn begin_transaction(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
        ) -> Result<(), u32> {
            self.dummy.begin_transaction(handle)
        }
        fn end_transaction(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            disposition: u32,
        ) -> Result<(), u32> {
            self.dummy.end_transaction(handle, disposition)
        }
        fn get_attrib(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            attr_id: u32,
        ) -> Result<Vec<u8>, u32> {
            self.dummy.get_attrib(handle, attr_id)
        }
        fn set_attrib(
            &self,
            handle: &crate::integrations::smartcard::ScardHandle,
            attr_id: u32,
            data: &[u8],
        ) -> Result<(), u32> {
            self.dummy.set_attrib(handle, attr_id, data)
        }
        fn get_container_info(
            &self,
            ctx: &ScardContext,
            container_index: u8,
        ) -> Result<Vec<u8>, u32> {
            self.dummy.get_container_info(ctx, container_index)
        }
        fn get_certificate(&self, ctx: &ScardContext) -> Result<Vec<u8>, u32> {
            self.dummy.get_certificate(ctx)
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    /// Build a SmartcardDevice exactly like device_service_entry does, with
    /// the given integration. Returns the raw DEVICE pointer (owned by the
    /// caller; free_handler consumes it).
    fn make_device(integration: Arc<dyn SmartcardIntegration>) -> *mut DEVICE {
        make_device_bounded(integration, SCARD_IRP_QUEUE_DEPTH)
    }

    fn make_device_bounded(
        integration: Arc<dyn SmartcardIntegration>,
        depth: usize,
    ) -> *mut DEVICE {
        let (irp_tx, irp_rx) = flume::bounded::<IrpWork>(depth);
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
        let outstanding = Arc::new(OutstandingIrpTracker::new());
        let liveness = DeviceLiveness::new();
        let status_threads = StatusThreads::new();
        let integration_clone = integration.clone();
        let contexts_clone = contexts.clone();
        let outstanding_clone = outstanding.clone();
        let liveness_clone = liveness.clone();
        let status_threads_clone = status_threads.clone();
        let device_thread = std::thread::Builder::new()
            .name("smartcard-device-test".into())
            .spawn(move || {
                device_thread_main(
                    irp_rx,
                    integration_clone,
                    contexts_clone,
                    outstanding_clone,
                    liveness_clone,
                    status_threads_clone,
                    new_card_cache(),
                );
            })
            .unwrap();
        let mut device_box = Box::new(SmartcardDevice {
            device: unsafe { std::mem::zeroed() },
            integration,
            contexts,
            irp_tx,
            device_thread: Some(device_thread),
            outstanding,
            liveness,
            status_threads,
            card_cache: new_card_cache(),
        });
        device_box.device.type_ = crate::addins::smartcard::consts::RDPDR_DTYP_SMARTCARD;
        device_box.device.IRPRequest = Some(irp_request_handler);
        device_box.device.Free = Some(free_handler);
        device_box.device.Init = Some(init_handler);
        Box::into_raw(device_box) as *mut DEVICE
    }

    #[test]
    fn free_handler_quiesces_detached_status_thread_before_returning() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let inflight = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let integration = Arc::new(BlockingStatusIntegration {
            dummy: DummySmartcardHandle::new(),
            entered: entered.clone(),
            release: release.clone(),
            inflight: inflight.clone(),
        });
        let device = make_device(integration.clone());

        // Register a context and send a GET_STATUS_CHANGE IRP through the
        // real device thread (irp_request_handler -> decode -> run_detached).
        // We build the IRP with a real encoded input PDU is complex; instead
        // drive run_detached directly via the device's own structures by
        // sending a Process work item with a fake IRP whose input decodes.
        // Simpler: call run_detached through the device thread by injecting
        // a Request — but IrpWork only carries raw IRPs. So we exercise the
        // detached path directly with the device's outstanding tracker.
        let scard = unsafe { &*(device as *mut SmartcardDevice) };
        let fake = FakeIrp::new(500);
        let request = status_request(&fake, &scard.contexts, 500, 0xFFFF_FFFF);
        unsafe {
            run_detached(
                request,
                &scard.integration,
                &scard.contexts,
                &scard.outstanding,
                &scard.liveness,
                &scard.status_threads,
                &scard.card_cache,
            )
        };

        // Wait until the status thread is parked inside get_status_change.
        let (lock, cvar) = &*entered;
        let mut entered_guard = lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !*entered_guard {
            let now = Instant::now();
            assert!(
                now < deadline,
                "status thread never entered get_status_change"
            );
            let (g, _) = cvar.wait_timeout(entered_guard, deadline - now).unwrap();
            entered_guard = g;
        }
        drop(entered_guard);
        assert_eq!(inflight.load(Ordering::SeqCst), 1);

        // free_handler on another thread: it must NOT return while the
        // status thread can still Complete the IRP.
        let dev_addr = device as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rc = unsafe { free_handler(dev_addr as *mut DEVICE) };
            let _ = done_tx.send(rc);
        });

        // Pre-fix: free_handler returns after ~2 s (wait_idle timeout) with
        // the status thread still parked. Give it up to 3 s to (incorrectly)
        // return early. Post-fix it blocks until the thread is quiesced.
        let early_return = done_rx.recv_timeout(Duration::from_secs(3)).is_ok();

        // Release the backend wait; the status thread now tries to Complete.
        let (rlock, rcvar) = &*release;
        *rlock.lock().unwrap() = true;
        rcvar.notify_all();

        let rc = if early_return {
            CHANNEL_RC_OK
        } else {
            done_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("free_handler never returned after release")
        };

        // Let the released status thread finish before asserting.
        let settle = Instant::now() + Duration::from_secs(2);
        while inflight.load(Ordering::SeqCst) > 0 && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(
            !early_return,
            "free_handler returned while a detached status thread could still \
             Complete the IRP — use-after-free on torn-down upstream state"
        );
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_eq!(inflight.load(Ordering::SeqCst), 0);
        // Post-fix: the IRP must NOT be Completed after teardown. Pre-fix
        // the released thread Completes it (recorded in the sink).
        assert!(
            fake.completed().is_empty(),
            "IRP Completed after free_handler returned = use-after-free"
        );
    }
    // ── IRP-queue backpressure (Fix O) ───────────────────────────────────

    // IRP-queue backpressure guarantee (Fix O, printer-class sibling): the
    // per-device queue is bounded; when full, irp_request_handler completes
    // the IRP synchronously with a packed scard error (real backpressure)
    // instead of growing an unbounded queue.
    /// Build a device whose worker thread is NOT draining (we never send it
    /// work and it blocks on recv), so the bounded channel can be filled
    /// deterministically.
    fn make_idle_device(depth: usize) -> (*mut DEVICE, flume::Receiver<IrpWork>) {
        // No worker thread: the bounded channel is never drained, so filler
        // IRPs (which have no decodable input) are never processed. The
        // returned Receiver MUST be kept alive by the caller: dropping it
        // disconnects the channel, which would make send() fail with
        // Disconnected (a different error path) instead of Full.
        let (irp_tx, irp_rx) = flume::bounded::<IrpWork>(depth);
        let contexts = Arc::new(Mutex::new(HashMap::<u64, ContextEntry>::new()));
        let outstanding = Arc::new(OutstandingIrpTracker::new());
        let liveness = DeviceLiveness::new();
        let status_threads = StatusThreads::new();
        let integration: Arc<dyn SmartcardIntegration> = Arc::new(DummySmartcardHandle::new());
        let mut device_box = Box::new(SmartcardDevice {
            device: unsafe { std::mem::zeroed() },
            integration,
            contexts,
            irp_tx,
            device_thread: None,
            outstanding,
            liveness,
            status_threads,
            card_cache: new_card_cache(),
        });
        device_box.device.type_ = crate::addins::smartcard::consts::RDPDR_DTYP_SMARTCARD;
        device_box.device.IRPRequest = Some(irp_request_handler);
        device_box.device.Free = Some(free_handler);
        device_box.device.Init = Some(init_handler);
        (Box::into_raw(device_box) as *mut DEVICE, irp_rx)
    }

    #[test]
    fn complete_irp_with_error_packs_and_completes() {
        let f = FakeIrp::new(7);
        unsafe { complete_irp_with_error(f.irp, SCARD_E_NOT_READY) };
        assert_eq!(f.completed(), vec![7]);
        unsafe {
            assert_eq!((*f.irp).IoStatus, STATUS_UNSUCCESSFUL as i32);
            // The packed error body must be non-trivial (header + scard error).
            assert!((*f.output).length > 16);
        }
    }

    #[test]
    fn full_queue_completes_overflow_irp_inline() {
        let depth = 4;
        let (dev, _irp_rx) = make_idle_device(depth);
        let scard = unsafe { &*(dev as *const SmartcardDevice) };

        // Fill the bounded channel directly (worker is idle, blocked on recv,
        // but we fill the channel buffer up to capacity without giving the
        // worker a chance to drain by holding the send side). Use try_send to
        // occupy all slots with dummy IRP pointers (never decoded because we
        // complete the overflow before the worker runs them).
        let mut filler = Vec::new();
        for i in 0..depth {
            let f = FakeIrp::new(200 + i as u32);
            // Occupy a slot; the worker may pick it up, so we keep filling
            // until try_send reports Full.
            filler.push(f);
        }
        // Force the channel full by sending raw works until Full.
        let mut sent = 0;
        loop {
            let f = FakeIrp::new(300 + sent);
            match scard.irp_tx.try_send(IrpWork::Process(f.irp)) {
                Ok(()) => {
                    sent += 1;
                    filler.push(f);
                    if sent > (depth * 4) as u32 {
                        break; // worker is draining; cannot force full this way
                    }
                }
                Err(flume::TrySendError::Full(_)) => {
                    filler.push(f);
                    break;
                }
                Err(_) => break,
            }
        }

        // Now the handler must complete the overflow IRP inline.
        let overflow = FakeIrp::new(999);
        unsafe {
            (*overflow.irp).MajorFunction = IRP_MJ_DEVICE_CONTROL;
        }
        let rc = unsafe { irp_request_handler(dev, overflow.irp) };
        assert_eq!(rc, CHANNEL_RC_OK);
        // The channel is full (no worker drains it), so the overflow IRP MUST
        // be completed synchronously with a scard error. With an unbounded
        // queue this would instead be enqueued and never completed (RED).
        assert_eq!(
            overflow.completed(),
            vec![999],
            "overflow IRP must be completed synchronously when the queue is full"
        );
        unsafe {
            assert_eq!((*overflow.irp).IoStatus, STATUS_UNSUCCESSFUL as i32);
        }

        // Drain the queue so free_handler's Shutdown send does not block on a
        // full channel (there is no worker in this idle device).
        while _irp_rx.try_recv().is_ok() {}
        unsafe {
            let _ = free_handler(dev);
        }
    }
}
