// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Printer integration trait and auxiliary types.
//!
//! Provides the abstract `PrinterIntegration` trait implemented by clients
//! (such as `uds-client` or `html5-rdp`) to handle redirected print jobs
//! independently of the operating system or environment.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// Information describing a local printer announced to the remote RDP session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterDeviceInfo {
    pub id: u32,
    pub name: String,
    pub driver_name: String,
    pub is_default: bool,
    pub port_name: Option<String>,
}

/// Abstract, platform-agnostic trait for printer redirection.
pub trait PrinterIntegration: Send + Sync + std::fmt::Debug {
    /// Returns true if the printer subsystem is active and ready to handle jobs.
    fn is_available(&self) -> bool;

    /// Enumerates printers to announce over RDPDR.
    fn list_printers(&self) -> Vec<PrinterDeviceInfo>;

    /// Initiates a new print job. Returns a unique job ID or a Win32/RDPDR error code.
    fn create_job(&self, printer_name: &str) -> Result<u32, u32>;

    /// Streams print data for an active job. Returns the number of bytes written.
    fn write_data(&self, job_id: u32, data: &[u8]) -> Result<usize, u32>;

    /// Finalizes the print job and submits it to the local spooler / output device.
    fn close_job(&self, job_id: u32) -> Result<(), u32>;

    /// Cancels or aborts an active job in case of error or abrupt session disconnect.
    fn abort_job(&self, job_id: u32);
}

// ---------------------------------------------------------------------------
// Dummy Implementation (Testing & Fallback)
// ---------------------------------------------------------------------------

/// In-memory dummy printer handle useful for testing and stubbing.
#[derive(Debug)]
pub struct DummyPrinterHandle {
    next_id: AtomicU32,
    jobs: Mutex<HashMap<u32, Vec<u8>>>,
    printers: Vec<PrinterDeviceInfo>,
}

impl Default for DummyPrinterHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl DummyPrinterHandle {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            jobs: Mutex::new(HashMap::new()),
            printers: vec![PrinterDeviceInfo {
                id: 1,
                name: "UDS Virtual Printer".to_string(),
                driver_name: "MS Publisher Imagesetter".to_string(),
                is_default: true,
                port_name: Some("PRN1".to_string()),
            }],
        }
    }

    pub fn with_printers(printers: Vec<PrinterDeviceInfo>) -> Self {
        Self {
            next_id: AtomicU32::new(1),
            jobs: Mutex::new(HashMap::new()),
            printers,
        }
    }

    pub fn get_job_data(&self, job_id: u32) -> Option<Vec<u8>> {
        self.jobs.lock().unwrap().get(&job_id).cloned()
    }
}

impl PrinterIntegration for DummyPrinterHandle {
    fn is_available(&self) -> bool {
        true
    }

    fn list_printers(&self) -> Vec<PrinterDeviceInfo> {
        self.printers.clone()
    }

    fn create_job(&self, _printer_name: &str) -> Result<u32, u32> {
        let job_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.jobs.lock().unwrap().insert(job_id, Vec::new());
        Ok(job_id)
    }

    fn write_data(&self, job_id: u32, data: &[u8]) -> Result<usize, u32> {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(buf) = jobs.get_mut(&job_id) {
            buf.extend_from_slice(data);
            Ok(data.len())
        } else {
            Err(0xC0000008) // STATUS_INVALID_HANDLE
        }
    }

    fn close_job(&self, job_id: u32) -> Result<(), u32> {
        let jobs = self.jobs.lock().unwrap();
        if jobs.contains_key(&job_id) {
            Ok(())
        } else {
            Err(0xC0000008)
        }
    }

    fn abort_job(&self, job_id: u32) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.remove(&job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dummy_printer_lifecycle() {
        let dummy = DummyPrinterHandle::new();
        assert!(dummy.is_available());

        let printers = dummy.list_printers();
        assert_eq!(printers.len(), 1);
        assert_eq!(printers[0].name, "UDS Virtual Printer");

        let job_id = dummy.create_job(&printers[0].name).unwrap();
        assert!(job_id > 0);

        let data = b"Hello, Printer!";
        let written = dummy.write_data(job_id, data).unwrap();
        assert_eq!(written, data.len());

        let stored = dummy.get_job_data(job_id).unwrap();
        assert_eq!(stored, data);

        assert!(dummy.close_job(job_id).is_ok());
    }
}
