// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Smartcard integration trait and auxiliary types.
//!
//! This module defines the `SmartcardIntegration` trait that consumers must implement
//! to provide smartcard functionality. The trait is backend-agnostic — implementations
//! can back it with:
//!
//! - **Dummy** responses (for testing / development)
//! - **pcsc-lite** (real physical card reader via the `pcsc` crate)
//! - **WebSocket bridge** (browser-based proxy for interactive smartcard access)
//!
//! The addin layer (`addins/smartcard.rs`) will receive IRPs from the RDPDR channel,
//! decode them, and dispatch them to the appropriate method on the trait.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Opaque Handles
// ---------------------------------------------------------------------------

static CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(1);
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_nonzero(counter: &AtomicU64) -> u64 {
    loop {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
        // 0 is reserved (SCARD_E_INVALID_HANDLE in some implementations)
    }
}

/// Opaque handle representing an established smartcard context.
///
/// Corresponds to `SCARDCONTEXT` in the PC/SC specification. The handle is
/// generated locally and echoed back by the RDP server in subsequent IOCTLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScardContext(u64);

impl Default for ScardContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ScardContext {
    /// Create a new context with a unique, non-zero ID.
    pub fn new() -> Self {
        ScardContext(next_nonzero(&CONTEXT_COUNTER))
    }

    /// Create a context from a raw u64 (e.g. when decoding from an IRP).
    pub fn from_raw(raw: u64) -> Self {
        ScardContext(raw)
    }

    /// Return the raw u64 representation.
    pub fn raw(&self) -> u64 {
        self.0
    }
}

/// Opaque handle representing a connection to a specific smartcard.
///
/// Corresponds to `SCARDHANDLE` in the PC/SC specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScardHandle {
    handle: u64,
    active_protocol: u32,
}

impl ScardHandle {
    /// Create a new handle with a unique ID.
    pub fn new(active_protocol: u32) -> Self {
        ScardHandle {
            handle: next_nonzero(&HANDLE_COUNTER),
            active_protocol,
        }
    }

    /// Create a handle from raw values (e.g. when decoding from an IRP).
    pub fn from_raw(handle: u64, active_protocol: u32) -> Self {
        ScardHandle {
            handle,
            active_protocol,
        }
    }

    /// Return the raw u64 handle.
    pub fn raw(&self) -> u64 {
        self.handle
    }

    /// Return the active protocol negotiated during `connect`.
    pub fn active_protocol(&self) -> u32 {
        self.active_protocol
    }
}

// ---------------------------------------------------------------------------
// Auxiliary Types
// ---------------------------------------------------------------------------

/// I/O request structure used in `transmit`.
///
/// Maps to `SCARD_IO_REQUEST` in the PC/SC spec.
#[derive(Debug, Clone)]
pub struct ScardIORequest {
    pub protocol: u32,
    pub extra_bytes: Vec<u8>,
}

impl ScardIORequest {
    /// Protocol T=0 (byte-oriented)
    pub fn t0() -> Self {
        ScardIORequest {
            protocol: SCARD_PROTOCOL_T0,
            extra_bytes: Vec::new(),
        }
    }

    /// Protocol T=1 (block-oriented)
    pub fn t1() -> Self {
        ScardIORequest {
            protocol: SCARD_PROTOCOL_T1,
            extra_bytes: Vec::new(),
        }
    }
}

/// Result of a `transmit` operation.
#[derive(Debug, Clone)]
pub struct TransmitResult {
    /// The I/O request returned by the card (may differ from the send PCI).
    pub recv_pci: Option<ScardIORequest>,
    /// The response bytes from the card (including SW1 SW2 at the end).
    pub recv_buffer: Vec<u8>,
}

/// Status of a connected card, returned by `status`.
#[derive(Debug, Clone)]
pub struct ScardStatus {
    /// Names of the reader(s) associated with the card (multi-string).
    pub reader_names: Vec<String>,
    /// Current card state (SCARD_STATE_* flags).
    pub state: u32,
    /// Active protocol (SCARD_PROTOCOL_* value).
    pub protocol: u32,
    /// ATR (Answer To Reset) of the card.
    pub atr: Vec<u8>,
}

/// Input state for `get_status_change` — describes the current known state
/// of a reader from the caller's perspective.
#[derive(Debug, Clone)]
pub struct ReaderStateIn {
    /// Name of the reader to watch.
    pub reader_name: String,
    /// Current state as known by the caller (SCARD_STATE_* flags).
    pub current_state: u32,
}

/// Output state returned by `get_status_change` — the new state after the call.
#[derive(Debug, Clone)]
pub struct ReaderStateOut {
    /// Name of the reader.
    pub reader_name: String,
    /// The state that triggered the return (SCARD_STATE_* flags).
    pub event_state: u32,
    /// ATR of the card (if present in the reader).
    pub atr: Vec<u8>,
}

/// Result of `locate_cards_by_atr` — which readers matched the ATR pattern.
#[derive(Debug, Clone)]
pub struct LocateCardResult {
    /// Name of the reader.
    pub reader_name: String,
    /// Whether the card's ATR matched the search pattern.
    pub atr_match: bool,
    /// The event state flags.
    pub event_state: u32,
}

/// Connect result, returned by `connect` and `reconnect`.
#[derive(Debug, Clone)]
pub struct ConnectResult {
    /// Opaque handle to the connected card.
    pub handle: ScardHandle,
    /// Active protocol negotiated with the card.
    pub active_protocol: u32,
}

// ---------------------------------------------------------------------------
// Protocol Constants
// ---------------------------------------------------------------------------

/// Smart card scope: operation applies to the system.
pub const SCARD_SCOPE_SYSTEM: u32 = 2;

/// Protocol T=0 (byte-oriented half-duplex).
pub const SCARD_PROTOCOL_T0: u32 = 0x0000_0001;
/// Protocol T=1 (block-oriented half-duplex).
pub const SCARD_PROTOCOL_T1: u32 = 0x0000_0002;
/// Raw protocol (vendor specific).
pub const SCARD_PROTOCOL_RAW: u32 = 0x0001_0000;

/// Share mode: exclusive access.
pub const SCARD_SHARE_EXCLUSIVE: u32 = 1;
/// Share mode: shared access.
pub const SCARD_SHARE_SHARED: u32 = 2;
/// Share mode: direct access (no card required).
pub const SCARD_SHARE_DIRECT: u32 = 3;

/// Card disposition: leave card as is.
pub const SCARD_LEAVE_CARD: u32 = 0;
/// Card disposition: reset the card.
pub const SCARD_RESET_CARD: u32 = 1;
/// Card disposition: power down the card.
pub const SCARD_UNPOWER_CARD: u32 = 2;
/// Card disposition: eject the card.
pub const SCARD_EJECT_CARD: u32 = 3;

// Reader state flags (SCARD_STATE_*)
pub const SCARD_STATE_UNAWARE: u32 = 0x0000_0000;
pub const SCARD_STATE_IGNORE: u32 = 0x0000_0001;
pub const SCARD_STATE_CHANGED: u32 = 0x0000_0002;
pub const SCARD_STATE_UNKNOWN: u32 = 0x0000_0004;
pub const SCARD_STATE_UNAVAILABLE: u32 = 0x0000_0008;
pub const SCARD_STATE_EMPTY: u32 = 0x0000_0010;
pub const SCARD_STATE_PRESENT: u32 = 0x0000_0020;
pub const SCARD_STATE_ATRMATCH: u32 = 0x0000_0040;
pub const SCARD_STATE_EXCLUSIVE: u32 = 0x0000_0080;
pub const SCARD_STATE_INUSE: u32 = 0x0000_0100;
pub const SCARD_STATE_MUTE: u32 = 0x0000_0200;
pub const SCARD_STATE_UNPOWERED: u32 = 0x0000_0400;

// ---------------------------------------------------------------------------
// SCARD Error Codes (MS-RDPESC / PC/SC)
// ---------------------------------------------------------------------------

pub const SCARD_S_SUCCESS: u32 = 0x0000_0000;
pub const SCARD_F_INTERNAL_ERROR: u32 = 0x8010_0001;
pub const SCARD_E_CANCELLED: u32 = 0x8010_0002;
pub const SCARD_E_INVALID_HANDLE: u32 = 0x8010_0003;
pub const SCARD_E_INVALID_PARAMETER: u32 = 0x8010_0004;
pub const SCARD_E_INVALID_TARGET: u32 = 0x8010_0005;
pub const SCARD_E_NO_MEMORY: u32 = 0x8010_0006;
pub const SCARD_E_INSUFFICIENT_BUFFER: u32 = 0x8010_0008;
pub const SCARD_E_UNKNOWN_READER: u32 = 0x8010_0009;
pub const SCARD_E_TIMEOUT: u32 = 0x8010_000A;
pub const SCARD_E_SHARING_VIOLATION: u32 = 0x8010_000B;
pub const SCARD_E_NO_SMARTCARD: u32 = 0x8010_000C;
pub const SCARD_E_UNKNOWN_CARD: u32 = 0x8010_000D;
pub const SCARD_E_PROTO_MISMATCH: u32 = 0x8010_000F;
pub const SCARD_E_NOT_READY: u32 = 0x8010_0010;
pub const SCARD_E_INVALID_VALUE: u32 = 0x8010_0011;
pub const SCARD_E_SYSTEM_CANCELLED: u32 = 0x8010_0012;
pub const SCARD_E_COMM_ERROR: u32 = 0x8010_0013;
pub const SCARD_F_UNKNOWN_ERROR: u32 = 0x8010_0014;
pub const SCARD_E_NOT_TRANSACTED: u32 = 0x8010_0016;
pub const SCARD_E_READER_UNAVAILABLE: u32 = 0x8010_0017;
pub const SCARD_E_NO_SERVICE: u32 = 0x8010_001D;
pub const SCARD_E_SERVICE_STOPPED: u32 = 0x8010_001E;
pub const SCARD_E_UNSUPPORTED_FEATURE: u32 = 0x8010_0022;
pub const SCARD_E_NO_READERS_AVAILABLE: u32 = 0x8010_002E;
pub const SCARD_W_UNSUPPORTED_CARD: u32 = 0x8010_0065;
pub const SCARD_W_UNRESPONSIVE_CARD: u32 = 0x8010_0066;
pub const SCARD_W_UNPOWERED_CARD: u32 = 0x8010_0067;
pub const SCARD_W_RESET_CARD: u32 = 0x8010_0068;
pub const SCARD_W_REMOVED_CARD: u32 = 0x8010_0069;
pub const SCARD_W_CARD_NOT_AUTHENTICATED: u32 = 0x8010_006F;

// NTSTATUS values used in IRP IoStatus
pub const STATUS_SUCCESS: u32 = 0x0000_0000;
pub const STATUS_UNSUCCESSFUL: u32 = 0xC000_0001;
pub const STATUS_NOT_SUPPORTED: u32 = 0xC000_00BB;
pub const STATUS_BUFFER_TOO_SMALL: u32 = 0xC000_0023;
pub const STATUS_DEVICE_DATA_ERROR: u32 = 0xC000_009C;
pub const STATUS_CANCELLED: u32 = 0xC000_0120;

/// IRP MajorFunction for device control (IOCTL dispatch)
pub const IRP_MJ_DEVICE_CONTROL: u32 = 0x0000_000E;

/// Maximum buffer size for SCardTransmit (66560 = 64.5 KB).
pub const SCARD_TRANSMIT_MAX: usize = 66560;

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Returns `true` if the given error code is an NTSTATUS (high 2 bits are 11).
pub const fn is_ntstatus(code: u32) -> bool {
    (code & 0xC000_0000) == 0xC000_0000
}

/// Where to place an error code in the IRP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorPlacement {
    /// Error goes in the IRP header's `IoStatus` field (NTSTATUS).
    IoStatus,
    /// Error goes in the NDR body's `ReturnCode` field (SCARD code).
    ReturnCode,
}

/// Classify an error code: does it belong in IoStatus or ReturnCode?
pub fn classify_error(code: u32) -> ErrorPlacement {
    if is_ntstatus(code) {
        ErrorPlacement::IoStatus
    } else {
        ErrorPlacement::ReturnCode
    }
}

// ---------------------------------------------------------------------------
// The Trait
// ---------------------------------------------------------------------------

/// Backend-agnostic trait for smartcard integration.
///
/// The consumer (uds-client, rdphtml5, tests, etc.) implements this trait
/// with whichever backend they need:
///
/// - **`DummySmartcardHandle`** — predefined responses (testing/development)
/// - **`PcscSmartcardHandle`** — delegates to `libpcsclite.so` via the `pcsc` crate
/// - **`WebSocketSmartcardHandle`** — proxies over WebSocket to a browser/backend
///
/// All methods are synchronous (blocking). The addin layer manages threading
/// (one thread per `ScardContext`), so the trait implementation doesn't need
/// to worry about async dispatch.
///
/// # Safety
///
/// Implementations must be `Send + Sync` because the same `Arc<dyn SmartcardIntegration>`
/// is shared across multiple threads (device thread + per-context threads).
pub trait SmartcardIntegration: Send + Sync + std::fmt::Debug {
    // === Context Management ===

    /// Establish a new smartcard resource manager context.
    ///
    /// `scope` is typically `SCARD_SCOPE_SYSTEM`.
    /// Returns a new unique `ScardContext`.
    fn establish_context(&self, scope: u32) -> Result<ScardContext, u32>;

    /// Release a previously established context.
    fn release_context(&self, ctx: &ScardContext) -> Result<(), u32>;

    /// Check whether a context handle is still valid.
    fn is_valid_context(&self, ctx: &ScardContext) -> bool;

    // === Reader Discovery ===

    /// List all readers currently available.
    ///
    /// `groups` filters by reader group names (pass `None` for all groups).
    fn list_readers(
        &self,
        ctx: &ScardContext,
        groups: Option<&[String]>,
    ) -> Result<Vec<String>, u32>;

    // === Card Connection ===

    /// Connect to a card in the named reader.
    ///
    /// - `share_mode`: `SCARD_SHARE_EXCLUSIVE`, `SCARD_SHARE_SHARED`, or `SCARD_SHARE_DIRECT`
    /// - `preferred_protocols`: bitmask of `SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1 | …`
    ///
    /// Returns a `ConnectResult` with the card handle and the negotiated protocol.
    fn connect(
        &self,
        ctx: &ScardContext,
        reader: &str,
        share_mode: u32,
        preferred_protocols: u32,
    ) -> Result<ConnectResult, u32>;

    /// Disconnect from a card.
    ///
    /// `disposition`: `SCARD_LEAVE_CARD`, `SCARD_RESET_CARD`, etc.
    fn disconnect(&self, handle: &ScardHandle, disposition: u32) -> Result<(), u32>;

    /// Re-establish a connection to a card that was previously connected.
    ///
    /// Returns the new `active_protocol`.
    fn reconnect(
        &self,
        handle: &ScardHandle,
        share_mode: u32,
        preferred_protocols: u32,
        initialization: u32,
    ) -> Result<u32, u32>;

    // === Card Communication ===

    /// Send a command APDU to the card and receive the response.
    fn transmit(
        &self,
        handle: &ScardHandle,
        send_pci: &ScardIORequest,
        data: &[u8],
    ) -> Result<TransmitResult, u32>;

    /// Send a control command to the reader.
    fn control(
        &self,
        handle: &ScardHandle,
        control_code: u32,
        in_data: &[u8],
    ) -> Result<Vec<u8>, u32>;

    // === Status & State ===

    /// Query the status of a connected card.
    fn status(&self, handle: &ScardHandle) -> Result<ScardStatus, u32>;

    /// Block until one of the readers changes state or the timeout expires.
    ///
    /// This is the most complex operation — see hallazgo E17 in the plan.
    fn get_status_change(
        &self,
        ctx: &ScardContext,
        timeout: Duration,
        reader_states: &[ReaderStateIn],
    ) -> Result<Vec<ReaderStateOut>, u32>;

    // === Transactions ===

    /// Begin an exclusive transaction on the card.
    fn begin_transaction(&self, handle: &ScardHandle) -> Result<(), u32>;

    /// End an exclusive transaction on the card.
    fn end_transaction(&self, handle: &ScardHandle, disposition: u32) -> Result<(), u32>;

    // === Attributes ===

    /// Get an attribute of the card or reader.
    fn get_attrib(&self, handle: &ScardHandle, attr_id: u32) -> Result<Vec<u8>, u32>;

    /// Set an attribute of the card or reader.
    fn set_attrib(&self, handle: &ScardHandle, attr_id: u32, data: &[u8]) -> Result<(), u32>;

    // === ATR Matching ===

    /// Locate cards by ATR pattern matching.
    ///
    /// `atrs` is a list of (pattern, mask) pairs. A card matches if for every byte:
    /// `(card_atr[i] & mask[i]) == (pattern[i] & mask[i])`.
    ///
    /// Returns the matching result for each reader.
    fn locate_cards_by_atr(
        &self,
        ctx: &ScardContext,
        atrs: &[(Vec<u8>, Vec<u8>)],
        reader_states: &[ReaderStateIn],
    ) -> Result<Vec<LocateCardResult>, u32> {
        // Default implementation: report no matches
        let _ = (ctx, atrs);
        Ok(reader_states
            .iter()
            .map(|rs| LocateCardResult {
                reader_name: rs.reader_name.clone(),
                atr_match: false,
                event_state: rs.current_state,
            })
            .collect())
    }

    // === Cancel ===

    /// Cancel all pending operations on the given context.
    ///
    /// This is called synchronously from the device thread when the server
    /// sends `SCARD_IOCTL_CANCEL`. Implementations should signal any
    /// blocked `get_status_change` calls to return immediately.
    fn cancel(&self, ctx: &ScardContext) -> Result<(), u32> {
        let _ = ctx;
        Ok(())
    }

    // === Meta ===

    /// Whether the smartcard subsystem is available and ready to serve requests.
    fn is_available(&self) -> bool;
}

// ---------------------------------------------------------------------------
// ATR Matching Helper
// ---------------------------------------------------------------------------

/// Compare a card ATR against a pattern + mask.
///
/// Returns `true` if `(atr[i] & mask[i]) == (pattern[i] & mask[i])` for every byte.
pub fn match_atr(atr: &[u8], pattern: &[u8], mask: &[u8]) -> bool {
    if atr.len() < pattern.len() || pattern.len() != mask.len() {
        return false;
    }
    atr.iter()
        .zip(pattern.iter())
        .zip(mask.iter())
        .all(|((a, p), m)| (a & m) == (p & m))
}

// ---------------------------------------------------------------------------
// Reader Filter Helper
// ---------------------------------------------------------------------------

/// Substring-based reader name filter (case-insensitive), matching FreeRDP behavior.
pub fn filter_reader(filter: Option<&str>, reader: &str) -> bool {
    match filter {
        None | Some("") => true,
        Some(f) => reader.to_lowercase().contains(&f.to_lowercase()),
    }
}

#[cfg(test)]
mod tests {
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
        let mask = vec![0xFF, 0xF0, 0xFF]; // second nibble of byte 1 is wildcard
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
        assert!(!is_ntstatus(0x8010000C)); // SCARD code
        assert!(!is_ntstatus(0x00000000)); // STATUS_SUCCESS
    }
}

// ---------------------------------------------------------------------------
// DummySmartcardHandle — for integration testing
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod dummy {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

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
            let cards = self.cards.read().unwrap();
            for card in cards.iter() {
                if let Some(h) = &card.handle
                    && h.raw() == handle.raw()
                {
                    return Ok(TransmitResult {
                        recv_pci: None,
                        recv_buffer: vec![0x90, 0x00],
                    });
                }
            }
            Err(SCARD_E_INVALID_HANDLE)
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
            _timeout: Duration,
            reader_states: &[ReaderStateIn],
        ) -> Result<Vec<ReaderStateOut>, u32> {
            let cards = self.cards.read().unwrap();
            Ok(reader_states
                .iter()
                .map(|rs| {
                    let present = cards.iter().any(|c| c.reader == rs.reader_name);
                    ReaderStateOut {
                        reader_name: rs.reader_name.clone(),
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
                .collect())
        }

        fn begin_transaction(&self, _handle: &ScardHandle) -> Result<(), u32> {
            Ok(())
        }

        fn end_transaction(&self, _handle: &ScardHandle, _disposition: u32) -> Result<(), u32> {
            Ok(())
        }

        fn get_attrib(&self, _handle: &ScardHandle, _attr_id: u32) -> Result<Vec<u8>, u32> {
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

        fn is_available(&self) -> bool {
            true
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn make_dummy() -> DummySmartcardHandle {
            let dummy = DummySmartcardHandle::new();
            dummy.add_card("Virtual Reader 0", vec![0x3B, 0x90, 0x95, 0x80, 0x1F, 0xC3]);
            dummy
        }

        #[test]
        fn dummy_establish_context() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            assert_ne!(ctx.raw(), 0);
            assert!(dummy.is_valid_context(&ctx));
        }

        #[test]
        fn dummy_release_context() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            dummy.release_context(&ctx).unwrap();
            assert!(!dummy.is_valid_context(&ctx));
        }

        #[test]
        fn dummy_list_readers() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            let readers = dummy.list_readers(&ctx, None).unwrap();
            assert_eq!(readers, vec!["Virtual Reader 0"]);
        }

        #[test]
        fn dummy_connect_and_transmit() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            let result = dummy
                .connect(
                    &ctx,
                    "Virtual Reader 0",
                    SCARD_SHARE_SHARED,
                    SCARD_PROTOCOL_T0,
                )
                .unwrap();
            assert_eq!(result.active_protocol, SCARD_PROTOCOL_T0);

            let transmit_result = dummy
                .transmit(&result.handle, &ScardIORequest::t0(), &[0x00, 0xA4])
                .unwrap();
            assert_eq!(transmit_result.recv_buffer, vec![0x90, 0x00]);
        }

        #[test]
        fn dummy_disconnect() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            let result = dummy
                .connect(
                    &ctx,
                    "Virtual Reader 0",
                    SCARD_SHARE_SHARED,
                    SCARD_PROTOCOL_T0,
                )
                .unwrap();
            dummy.disconnect(&result.handle, SCARD_LEAVE_CARD).unwrap();
        }

        #[test]
        fn dummy_status() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            let result = dummy
                .connect(
                    &ctx,
                    "Virtual Reader 0",
                    SCARD_SHARE_SHARED,
                    SCARD_PROTOCOL_T0,
                )
                .unwrap();
            let status = dummy.status(&result.handle).unwrap();
            assert_eq!(status.reader_names, vec!["Virtual Reader 0"]);
            assert_eq!(status.atr, vec![0x3B, 0x90, 0x95, 0x80, 0x1F, 0xC3]);
        }

        #[test]
        fn dummy_full_cycle() {
            let dummy = make_dummy();

            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            assert!(dummy.is_valid_context(&ctx));

            let readers = dummy.list_readers(&ctx, None).unwrap();
            assert!(!readers.is_empty());

            let connect_result = dummy
                .connect(&ctx, &readers[0], SCARD_SHARE_SHARED, SCARD_PROTOCOL_T0)
                .unwrap();

            let status = dummy.status(&connect_result.handle).unwrap();
            assert_eq!(status.state, SCARD_STATE_PRESENT);

            let transmit_result = dummy
                .transmit(
                    &connect_result.handle,
                    &ScardIORequest::t0(),
                    &[0x00, 0xA4, 0x04, 0x00],
                )
                .unwrap();
            assert_eq!(transmit_result.recv_buffer, vec![0x90, 0x00]);

            dummy
                .disconnect(&connect_result.handle, SCARD_LEAVE_CARD)
                .unwrap();
            dummy.release_context(&ctx).unwrap();
            assert!(!dummy.is_valid_context(&ctx));
        }

        #[test]
        fn dummy_unknown_reader_returns_error() {
            let dummy = make_dummy();
            let ctx = dummy.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
            let result = dummy.connect(&ctx, "NonExistent", SCARD_SHARE_SHARED, SCARD_PROTOCOL_T0);
            assert_eq!(result.unwrap_err(), SCARD_E_UNKNOWN_READER);
        }

        #[test]
        fn dummy_invalid_context_returns_error() {
            let dummy = make_dummy();
            let fake_ctx = ScardContext::from_raw(0xDEADBEEF);
            let result = dummy.list_readers(&fake_ctx, None);
            assert!(result.is_ok());
        }

        #[test]
        fn dummy_concurrent_contexts() {
            use std::sync::Arc;
            use std::thread;

            let dummy = Arc::new(make_dummy());
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let dummy_clone = dummy.clone();
                    thread::spawn(move || {
                        let ctx = dummy_clone.establish_context(SCARD_SCOPE_SYSTEM).unwrap();
                        let _readers = dummy_clone.list_readers(&ctx, None).unwrap();
                        dummy_clone.release_context(&ctx).unwrap();
                    })
                })
                .collect();

            for h in handles {
                h.join().unwrap();
            }
        }
    }
}
