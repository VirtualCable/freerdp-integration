// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com
//          Janier Rodríguez, jrodriguez at virtualcable dot es

//! Tests for smartcard types, helpers, and the dummy integration handle.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, RwLock};
use std::time::Duration;

use super::SmartcardIntegration;
use super::consts::{
    SCARD_E_CANCELLED, SCARD_E_INVALID_HANDLE, SCARD_E_TIMEOUT, SCARD_E_UNKNOWN_READER,
    SCARD_E_UNSUPPORTED_FEATURE, SCARD_PROTOCOL_T0, SCARD_S_SUCCESS, SCARD_STATE_CHANGED,
    SCARD_STATE_EMPTY, SCARD_STATE_PRESENT,
};
use super::types::*;

// ---------------------------------------------------------------------------
// DummySmartcardHandle — for integration testing
// ---------------------------------------------------------------------------

pub mod dummy {
    use super::*;

    #[derive(Debug)]
    struct DummyCard {
        reader: String,
        atr: Vec<u8>,
        handle: Option<ScardHandle>,
    }

    #[derive(Debug)]
    pub struct DummySmartcardHandle {
        cards: RwLock<Vec<DummyCard>>,
        contexts: RwLock<HashMap<u64, ScardContext>>,
        transaction_holder: Mutex<Option<u64>>,
        transaction_released: Condvar,
        transmit_response: Mutex<Vec<u8>>,
        status_change_timeouts: AtomicU32,
        status_change_calls: AtomicU32,
        cancel_requested: AtomicBool,
    }

    impl Default for DummySmartcardHandle {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DummySmartcardHandle {
        pub fn new() -> Self {
            DummySmartcardHandle {
                cards: RwLock::new(Vec::new()),
                contexts: RwLock::new(HashMap::new()),
                transaction_holder: Mutex::new(None),
                transaction_released: Condvar::new(),
                transmit_response: Mutex::new(vec![0x90, 0x00]),
                status_change_timeouts: AtomicU32::new(0),
                status_change_calls: AtomicU32::new(0),
                cancel_requested: AtomicBool::new(false),
            }
        }

        pub fn add_card(&self, reader: &str, atr: Vec<u8>) {
            let mut cards = self.cards.write().unwrap();
            cards.push(DummyCard {
                reader: reader.to_string(),
                atr,
                handle: None,
            });
        }

        pub fn set_transmit_response(&self, response: Vec<u8>) {
            *self.transmit_response.lock().unwrap() = response;
        }

        /// `get_status_change` answers `SCARD_E_TIMEOUT` this many times before
        /// reporting the change.
        pub fn set_status_change_timeouts(&self, count: u32) {
            self.status_change_timeouts.store(count, Ordering::SeqCst);
        }

        pub fn status_change_calls(&self) -> u32 {
            self.status_change_calls.load(Ordering::SeqCst)
        }

        fn has_handle(&self, handle: &ScardHandle) -> bool {
            let cards = self.cards.read().unwrap();
            cards
                .iter()
                .any(|card| card.handle.is_some_and(|h| h.raw() == handle.raw()))
        }

        /// Blocks while another handle holds the transaction, like pcsc-lite.
        fn wait_for_transaction(
            &self,
            handle: &ScardHandle,
        ) -> std::sync::MutexGuard<'_, Option<u64>> {
            let mut holder = self.transaction_holder.lock().unwrap();
            while holder.is_some_and(|h| h != handle.raw()) {
                holder = self.transaction_released.wait(holder).unwrap();
            }
            holder
        }
    }

    impl SmartcardIntegration for DummySmartcardHandle {
        fn establish_context(&self, _scope: u32) -> Result<ScardContext, u32> {
            let ctx = ScardContext::new();
            let mut contexts = self.contexts.write().unwrap();
            contexts.insert(ctx.raw(), ctx);
            Ok(ctx)
        }

        fn release_context(&self, ctx: &ScardContext) -> Result<(), u32> {
            let mut contexts = self.contexts.write().unwrap();
            contexts.remove(&ctx.raw()).ok_or(SCARD_E_INVALID_HANDLE)?;
            Ok(())
        }

        fn is_valid_context(&self, ctx: &ScardContext) -> bool {
            let contexts = self.contexts.read().unwrap();
            contexts.contains_key(&ctx.raw())
        }

        fn list_readers(
            &self,
            _ctx: &ScardContext,
            _groups: Option<&[String]>,
        ) -> Result<Vec<String>, u32> {
            let cards = self.cards.read().unwrap();
            Ok(cards.iter().map(|c| c.reader.clone()).collect())
        }

        fn connect(
            &self,
            _ctx: &ScardContext,
            reader: &str,
            _share_mode: u32,
            _preferred_protocols: u32,
        ) -> Result<ConnectResult, u32> {
            let mut cards = self.cards.write().unwrap();
            for card in cards.iter_mut() {
                if card.reader == reader {
                    let handle = ScardHandle::new(SCARD_PROTOCOL_T0);
                    card.handle = Some(handle);
                    return Ok(ConnectResult {
                        handle,
                        active_protocol: SCARD_PROTOCOL_T0,
                    });
                }
            }
            Err(SCARD_E_UNKNOWN_READER)
        }

        fn disconnect(&self, handle: &ScardHandle, _disposition: u32) -> Result<(), u32> {
            let mut cards = self.cards.write().unwrap();
            for card in cards.iter_mut() {
                if let Some(h) = &card.handle
                    && h.raw() == handle.raw()
                {
                    card.handle = None;
                    return Ok(());
                }
            }
            Err(SCARD_E_INVALID_HANDLE)
        }

        fn reconnect(
            &self,
            handle: &ScardHandle,
            _share_mode: u32,
            _preferred_protocols: u32,
            _initialization: u32,
        ) -> Result<u32, u32> {
            let cards = self.cards.read().unwrap();
            for card in cards.iter() {
                if let Some(h) = &card.handle
                    && h.raw() == handle.raw()
                {
                    return Ok(SCARD_PROTOCOL_T0);
                }
            }

            Err(SCARD_E_INVALID_HANDLE)
        }

        fn transmit(
            &self,
            handle: &ScardHandle,
            _send_pci: &ScardIORequest,
            _data: &[u8],
        ) -> Result<TransmitResult, u32> {
            if !self.has_handle(handle) {
                return Err(SCARD_E_INVALID_HANDLE);
            }
            let _holder = self.wait_for_transaction(handle);
            Ok(TransmitResult {
                recv_pci: None,
                recv_buffer: self.transmit_response.lock().unwrap().clone(),
            })
        }

        fn control(
            &self,
            _handle: &ScardHandle,
            _control_code: u32,
            _in_data: &[u8],
        ) -> Result<Vec<u8>, u32> {
            Ok(vec![])
        }

        fn status(&self, handle: &ScardHandle) -> Result<ScardStatus, u32> {
            let cards = self.cards.read().unwrap();
            for card in cards.iter() {
                if let Some(h) = &card.handle
                    && h.raw() == handle.raw()
                {
                    return Ok(ScardStatus {
                        reader_names: vec![card.reader.clone()],
                        state: SCARD_STATE_PRESENT,
                        protocol: SCARD_PROTOCOL_T0,
                        atr: card.atr.clone(),
                    });
                }
            }
            Err(SCARD_E_INVALID_HANDLE)
        }

        fn get_status_change(
            &self,
            _ctx: &ScardContext,
            timeout: Duration,
            reader_states: &[ReaderStateIn],
        ) -> Result<(Vec<ReaderStateOut>, u32), u32> {
            self.status_change_calls.fetch_add(1, Ordering::SeqCst);
            let return_code = if self.cancel_requested.swap(false, Ordering::SeqCst) {
                SCARD_E_CANCELLED
            } else if self
                .status_change_timeouts
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                std::thread::sleep(timeout.min(Duration::from_millis(100)));
                SCARD_E_TIMEOUT
            } else {
                SCARD_S_SUCCESS
            };
            let cards = self.cards.read().unwrap();
            let states = reader_states
                .iter()
                .map(|rs| {
                    let present = cards.iter().any(|c| c.reader == rs.reader_name);
                    ReaderStateOut {
                        reader_name: rs.reader_name.clone(),
                        current_state: rs.current_state,
                        event_state: if present {
                            SCARD_STATE_PRESENT | SCARD_STATE_CHANGED
                        } else {
                            SCARD_STATE_EMPTY | SCARD_STATE_CHANGED
                        },
                        atr: cards
                            .iter()
                            .find(|c| c.reader == rs.reader_name)
                            .map(|c| c.atr.clone())
                            .unwrap_or_default(),
                    }
                })
                .collect();
            Ok((states, return_code))
        }

        fn begin_transaction(&self, handle: &ScardHandle) -> Result<(), u32> {
            if !self.has_handle(handle) {
                return Err(SCARD_E_INVALID_HANDLE);
            }
            let mut holder = self.wait_for_transaction(handle);
            *holder = Some(handle.raw());
            Ok(())
        }

        fn end_transaction(&self, handle: &ScardHandle, _disposition: u32) -> Result<(), u32> {
            let mut holder = self.transaction_holder.lock().unwrap();
            if *holder != Some(handle.raw()) {
                return Err(SCARD_E_INVALID_HANDLE);
            }
            *holder = None;
            self.transaction_released.notify_all();
            Ok(())
        }

        fn get_attrib(&self, handle: &ScardHandle, _attr_id: u32) -> Result<Vec<u8>, u32> {
            if !self.has_handle(handle) {
                return Err(SCARD_E_INVALID_HANDLE);
            }
            Ok(vec![0x00])
        }

        fn set_attrib(
            &self,
            _handle: &ScardHandle,
            _attr_id: u32,
            _data: &[u8],
        ) -> Result<(), u32> {
            Ok(())
        }

        fn get_container_info(
            &self,
            _ctx: &ScardContext,
            _container_index: u8,
        ) -> Result<Vec<u8>, u32> {
            Err(SCARD_E_UNSUPPORTED_FEATURE)
        }

        fn get_certificate(&self, _ctx: &ScardContext) -> Result<Vec<u8>, u32> {
            Err(SCARD_E_UNSUPPORTED_FEATURE)
        }

        fn cancel(&self, _ctx: &ScardContext) -> Result<(), u32> {
            self.cancel_requested.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::super::consts::SCARD_STATE_PRESENT;
        use super::*;

        #[test]
        fn dummy_establish_context() {
            let dummy = DummySmartcardHandle::new();
            let ctx = dummy.establish_context(2).unwrap();
            assert!(dummy.is_valid_context(&ctx));
        }

        #[test]
        fn dummy_release_context() {
            let dummy = DummySmartcardHandle::new();
            let ctx = dummy.establish_context(2).unwrap();
            dummy.release_context(&ctx).unwrap();
            assert!(!dummy.is_valid_context(&ctx));
        }

        #[test]
        fn dummy_list_readers() {
            let dummy = DummySmartcardHandle::new();
            dummy.add_card("Reader 1", vec![0x3B]);
            dummy.add_card("Reader 2", vec![0x3B]);
            let ctx = dummy.establish_context(2).unwrap();
            let readers = dummy.list_readers(&ctx, None).unwrap();
            assert_eq!(readers.len(), 2);
            assert!(readers.contains(&"Reader 1".to_string()));
        }

        #[test]
        fn dummy_connect_and_transmit() {
            let dummy = DummySmartcardHandle::new();
            dummy.add_card("Test Reader", vec![0x3B, 0x90]);
            let ctx = dummy.establish_context(2).unwrap();
            let result = dummy.connect(&ctx, "Test Reader", 1, 1).unwrap();
            let tx = dummy
                .transmit(&result.handle, &ScardIORequest::t0(), &[0x00, 0xA4])
                .unwrap();
            assert_eq!(tx.recv_buffer, vec![0x90, 0x00]);
        }

        #[test]
        fn dummy_disconnect() {
            let dummy = DummySmartcardHandle::new();
            dummy.add_card("Test Reader", vec![0x3B]);
            let ctx = dummy.establish_context(2).unwrap();
            let result = dummy.connect(&ctx, "Test Reader", 1, 1).unwrap();
            dummy.disconnect(&result.handle, 0).unwrap();
        }

        #[test]
        fn dummy_get_status_change() {
            let dummy = DummySmartcardHandle::new();
            dummy.add_card("Present Reader", vec![0x3B]);
            let ctx = dummy.establish_context(2).unwrap();
            let states = vec![
                ReaderStateIn {
                    reader_name: "Present Reader".to_string(),
                    current_state: 0,
                },
                ReaderStateIn {
                    reader_name: "Missing Reader".to_string(),
                    current_state: 0,
                },
            ];
            let (out, return_code) = dummy
                .get_status_change(&ctx, Duration::from_millis(100), &states)
                .unwrap();
            assert_eq!(return_code, SCARD_S_SUCCESS);
            assert_eq!(out.len(), 2);
            assert_ne!(out[0].event_state & SCARD_STATE_PRESENT, 0);
            assert_eq!(out[1].event_state & SCARD_STATE_PRESENT, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests for types and helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit_tests {
    use super::super::consts::{
        SCARD_E_NO_SMARTCARD, SCARD_S_SUCCESS, STATUS_BUFFER_TOO_SMALL, STATUS_CANCELLED,
    };
    use super::*;

    #[test]
    fn scard_context_is_nonzero() {
        let ctx = ScardContext::new();
        assert_ne!(ctx.raw(), 0);
    }

    #[test]
    fn scard_context_unique() {
        let a = ScardContext::new();
        let b = ScardContext::new();
        assert_ne!(a.raw(), b.raw());
    }

    #[test]
    fn scard_handle_is_nonzero() {
        let h = ScardHandle::new(SCARD_PROTOCOL_T0);
        assert_ne!(h.raw(), 0);
        assert_eq!(h.active_protocol(), SCARD_PROTOCOL_T0);
    }

    #[test]
    fn scard_io_request_t0() {
        let r = ScardIORequest::t0();
        assert_eq!(r.protocol, SCARD_PROTOCOL_T0);
        assert!(r.extra_bytes.is_empty());
    }

    #[test]
    fn match_atr_exact() {
        let atr = vec![0x3B, 0x90, 0x95];
        let pattern = vec![0x3B, 0x90, 0x95];
        let mask = vec![0xFF, 0xFF, 0xFF];
        assert!(match_atr(&atr, &pattern, &mask));
    }

    #[test]
    fn match_atr_with_wildcard() {
        let atr = vec![0x3B, 0x99, 0x95];
        let pattern = vec![0x3B, 0x90, 0x95];
        let mask = vec![0xFF, 0xF0, 0xFF];
        assert!(match_atr(&atr, &pattern, &mask));
    }

    #[test]
    fn match_atr_mismatch() {
        let atr = vec![0x3B, 0x00, 0x95];
        let pattern = vec![0x3B, 0x90, 0x95];
        let mask = vec![0xFF, 0xFF, 0xFF];
        assert!(!match_atr(&atr, &pattern, &mask));
    }

    #[test]
    fn match_atr_different_lengths() {
        let atr = vec![0x3B];
        let pattern = vec![0x3B, 0x90];
        let mask = vec![0xFF, 0xFF];
        assert!(!match_atr(&atr, &pattern, &mask));
    }

    #[test]
    fn filter_reader_no_filter() {
        assert!(filter_reader(None, "Some Reader"));
        assert!(filter_reader(Some(""), "Some Reader"));
    }

    #[test]
    fn filter_reader_substring() {
        assert!(filter_reader(Some("reader"), "Some Reader v2"));
        assert!(!filter_reader(Some("usb"), "Some Reader v2"));
    }

    #[test]
    fn filter_reader_case_insensitive() {
        assert!(filter_reader(Some("READER"), "some reader v2"));
    }

    #[test]
    fn error_classification_ntstatus() {
        assert_eq!(
            classify_error(STATUS_BUFFER_TOO_SMALL),
            ErrorPlacement::IoStatus
        );
        assert_eq!(classify_error(STATUS_CANCELLED), ErrorPlacement::IoStatus);
    }

    #[test]
    fn error_classification_scard() {
        assert_eq!(
            classify_error(SCARD_E_NO_SMARTCARD),
            ErrorPlacement::ReturnCode
        );
        assert_eq!(classify_error(SCARD_S_SUCCESS), ErrorPlacement::ReturnCode);
    }

    #[test]
    fn is_ntstatus_works() {
        assert!(is_ntstatus(0xC0000023));
        assert!(is_ntstatus(0xC0000002));
        assert!(!is_ntstatus(0x8010_000C)); // SCARD code
        assert!(!is_ntstatus(0x0000_0000)); // STATUS_SUCCESS
    }
}
