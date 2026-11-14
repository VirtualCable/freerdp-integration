// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Printer device struct, worker thread, and FreeRDP callbacks.

use std::sync::Arc;

use freerdp_sys::{CHANNEL_RC_OK, DEVICE, IRP, UINT, UINT16, wStream};

use crate::integrations::PrinterIntegration;
use crate::utils::log;

use super::consts::*;
use super::handlers::process_irp;

pub(crate) enum IrpWork {
    Process(*mut IRP),
    Shutdown,
}

unsafe impl Send for IrpWork {}

/// Per-printer IRP queue depth bound. The host can stream WRITE IRPs at wire
/// rate against a slow print backend; an unbounded queue lets a malicious
/// pooled host grow per-session memory without limit (the IRP's retained
/// whole-PDU input stream is only released at Complete). A bounded queue
/// turns that into real backpressure: when full, the IRP is completed
/// synchronously with STATUS_PRINT_QUEUE_FULL instead of being queued.
pub(crate) const PRINTER_IRP_QUEUE_DEPTH: usize = 256;

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
            IrpWork::Process(irp) => unsafe {
                process_irp(irp, &integration, &printer_name);
            },
            IrpWork::Shutdown => {
                log::info!(
                    "printer[{}]: worker thread shutdown requested",
                    printer_name
                );
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
    match pdev.irp_tx.try_send(IrpWork::Process(irp)) {
        Ok(()) => {}
        Err(flume::TrySendError::Full(_)) => {
            // Queue full: apply real backpressure. Complete the IRP
            // synchronously with STATUS_PRINT_QUEUE_FULL (the caller is the
            // rdpdr plugin thread that would run completion anyway), so the
            // host sees a legitimate "print queue full" response and upstream
            // releases the retained input stream. Returning non-OK alone
            // would leak the IRP on 3.26.
            log::warn!(
                "printer[{}]: IRP queue full, rejecting with STATUS_PRINT_QUEUE_FULL",
                pdev.printer_name
            );
            unsafe {
                let irp_ref = &mut *irp;
                if !irp_ref.output.is_null() {
                    super::handlers::stream_write_u32(irp_ref.output, 0);
                }
                irp_ref.IoStatus = STATUS_PRINT_QUEUE_FULL;
                if let Some(complete) = irp_ref.Complete {
                    complete(irp);
                }
            }
        }
        Err(flume::TrySendError::Disconnected(_)) => {
            log::error!("printer: failed to queue IRP (channel disconnected)");
            unsafe {
                let irp_ref = &mut *irp;
                irp_ref.IoStatus = STATUS_UNSUCCESSFUL;
                if let Some(complete) = irp_ref.Complete {
                    complete(irp);
                }
            }
        }
    }

    CHANNEL_RC_OK
}

pub(crate) unsafe extern "C" fn init_handler(_device: *mut DEVICE) -> UINT {
    log::debug!("printer: init_handler called");
    CHANNEL_RC_OK
}

/// Printer cache and XPS notices from the server (MS-RDPEPC). Nothing is cached,
/// but they must be accepted: rdpdr treats an error here as a broken channel
/// and drops the whole session.
pub(crate) unsafe extern "C" fn custom_component_handler(
    _device: *mut DEVICE,
    component: UINT16,
    packet_id: UINT16,
    _stream: *mut wStream,
) -> UINT {
    log::debug!(
        "printer: ignoring component message component=0x{:04X} packet=0x{:04X}",
        component,
        packet_id
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_cache_messages_are_accepted() {
        let cache_data = freerdp_sys::RDPDR_PAKID_PAKID_PRN_CACHE_DATA as UINT16;
        let using_xps = freerdp_sys::RDPDR_PAKID_PAKID_PRN_USING_XPS as UINT16;
        let component = freerdp_sys::RDPDR_CTYP_RDPDR_CTYP_PRN as UINT16;
        for packet_id in [cache_data, using_xps] {
            let result = unsafe {
                custom_component_handler(
                    std::ptr::null_mut(),
                    component,
                    packet_id,
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(result, CHANNEL_RC_OK);
        }
    }
}

#[cfg(test)]
mod backpressure_tests {
    //! Lifetime / DoS guarantee (Fix F): the per-printer IRP queue is bounded;
    //! when full, irp_request_handler completes the IRP synchronously with
    //! STATUS_PRINT_QUEUE_FULL (real backpressure) instead of growing an
    //! unbounded queue. The worker is parked inside create_job so the queue
    //! stays full while we probe.
    use super::*;
    use crate::integrations::printer::{DummyPrinterHandle, PrinterDeviceInfo};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    /// Integration whose create_job blocks until released, so the single
    /// worker thread is parked and the bounded queue fills deterministically.
    #[derive(Debug)]
    struct BlockingPrinter {
        dummy: DummyPrinterHandle,
        entered: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
        creates: Arc<AtomicU32>,
    }
    impl PrinterIntegration for BlockingPrinter {
        fn is_available(&self) -> bool {
            true
        }
        fn list_printers(&self) -> Vec<PrinterDeviceInfo> {
            self.dummy.list_printers()
        }
        fn create_job(&self, name: &str) -> Result<u32, u32> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            let (lock, cvar) = &*self.entered;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            let (rlock, rcvar) = &*self.release;
            let mut released = rlock.lock().unwrap();
            while !*released {
                released = rcvar.wait(released).unwrap();
            }
            self.dummy.create_job(name)
        }
        fn write_data(&self, job_id: u32, data: &[u8]) -> Result<usize, u32> {
            self.dummy.write_data(job_id, data)
        }
        fn close_job(&self, job_id: u32) -> Result<(), u32> {
            self.dummy.close_job(job_id)
        }
        fn abort_job(&self, job_id: u32) {
            self.dummy.abort_job(job_id)
        }
    }

    struct IrpSink {
        completed: Mutex<Vec<(u32, i32)>>, // (CompletionId, IoStatus)
    }
    unsafe extern "C" fn rec_complete(irp: *mut IRP) -> UINT {
        let sink = unsafe { &*((*irp).device as *const IrpSink) };
        sink.completed
            .lock()
            .unwrap()
            .push(unsafe { ((*irp).CompletionId, (*irp).IoStatus) });
        CHANNEL_RC_OK
    }

    struct FakeIrp {
        irp: *mut IRP,
        sink: *mut IrpSink,
        output: *mut wStream,
    }
    impl FakeIrp {
        fn new(id: u32, major: u32) -> Self {
            let sink = Box::into_raw(Box::new(IrpSink {
                completed: Mutex::new(Vec::new()),
            }));
            let output = unsafe { freerdp_sys::Stream_New(std::ptr::null_mut(), 256) };
            let irp = Box::into_raw(Box::new(unsafe { std::mem::zeroed::<IRP>() }));
            unsafe {
                (*irp).CompletionId = id;
                (*irp).MajorFunction = major;
                (*irp).device = sink as *mut DEVICE;
                (*irp).output = output;
                (*irp).Complete = Some(rec_complete);
            }
            Self { irp, sink, output }
        }
        fn completed(&self) -> Vec<(u32, i32)> {
            unsafe { (*self.sink).completed.lock().unwrap().clone() }
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

    /// Build a PrinterDevice with a bounded queue and the given integration.
    fn make_device(integration: Arc<dyn PrinterIntegration>) -> *mut PrinterDevice {
        let (irp_tx, irp_rx) = flume::bounded(PRINTER_IRP_QUEUE_DEPTH);
        let integration_clone = integration.clone();
        let worker = std::thread::spawn(move || {
            printer_thread_main(irp_rx, integration_clone, "TestPrinter".to_string());
        });
        Box::into_raw(Box::new(PrinterDevice {
            device: unsafe { std::mem::zeroed() },
            printer_name: "TestPrinter".to_string(),
            integration,
            irp_tx,
            device_thread: Some(worker),
        }))
    }

    #[test]
    fn full_queue_completes_irp_with_queue_full() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let creates = Arc::new(AtomicU32::new(0));
        let integration = Arc::new(BlockingPrinter {
            dummy: DummyPrinterHandle::new(),
            entered: entered.clone(),
            release: release.clone(),
            creates: creates.clone(),
        });
        let dev = make_device(integration.clone());
        let dev_ptr = dev as *mut DEVICE;

        // First CREATE: worker picks it up and parks inside create_job.
        let first = FakeIrp::new(1, IRP_MJ_CREATE);
        assert_eq!(
            unsafe { irp_request_handler(dev_ptr, first.irp) },
            CHANNEL_RC_OK
        );
        let (lock, cvar) = &*entered;
        let mut g = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !*g {
            let now = std::time::Instant::now();
            assert!(now < deadline, "worker never entered create_job");
            let (gg, _) = cvar.wait_timeout(g, deadline - now).unwrap();
            g = gg;
        }
        drop(g);

        // Fill the bounded queue (worker is parked, so nothing drains).
        let mut queued = Vec::new();
        for i in 0..PRINTER_IRP_QUEUE_DEPTH {
            let f = FakeIrp::new(100 + i as u32, IRP_MJ_CREATE);
            assert_eq!(
                unsafe { irp_request_handler(dev_ptr, f.irp) },
                CHANNEL_RC_OK
            );
            queued.push(f);
        }

        // One more: queue is full -> must be completed synchronously with
        // STATUS_PRINT_QUEUE_FULL, NOT queued.
        let overflow = FakeIrp::new(999, IRP_MJ_CREATE);
        assert_eq!(
            unsafe { irp_request_handler(dev_ptr, overflow.irp) },
            CHANNEL_RC_OK
        );
        assert_eq!(
            overflow.completed(),
            vec![(999, STATUS_PRINT_QUEUE_FULL)],
            "overflow IRP must be completed synchronously with QUEUE_FULL"
        );

        // Release the worker and shut down cleanly.
        let (rlock, rcvar) = &*release;
        *rlock.lock().unwrap() = true;
        rcvar.notify_all();
        unsafe {
            let _ = free_handler(dev_ptr);
        }
    }

    #[test]
    fn below_capacity_irps_are_queued_not_completed_inline() {
        let integration: Arc<dyn PrinterIntegration> = Arc::new(DummyPrinterHandle::new());
        let dev = make_device(integration);
        let dev_ptr = dev as *mut DEVICE;
        let f = FakeIrp::new(7, IRP_MJ_CREATE);
        assert_eq!(
            unsafe { irp_request_handler(dev_ptr, f.irp) },
            CHANNEL_RC_OK
        );
        // Worker completes it asynchronously; wait for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while f.completed().is_empty() {
            assert!(std::time::Instant::now() < deadline, "IRP never completed");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(f.completed()[0].0, 7);
        assert_eq!(f.completed()[0].1, STATUS_SUCCESS);
        unsafe {
            let _ = free_handler(dev_ptr);
        }
    }
}
