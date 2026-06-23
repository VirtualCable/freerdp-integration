// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Smartcard opaque handles, auxiliary types, and error classification.

use std::sync::atomic::{AtomicU64, Ordering};

use super::consts::{SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1};

// ---------------------------------------------------------------------------
// Handle Generation
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

// ---------------------------------------------------------------------------
// Opaque Handles
// ---------------------------------------------------------------------------

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
