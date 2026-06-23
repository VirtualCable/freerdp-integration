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
//! The addin layer (`addins/smartcard/`) will receive IRPs from the RDPDR channel,
//! decode them, and dispatch them to the appropriate method on the trait.

pub mod consts;
pub mod tests;
pub mod types;

pub use consts::*;
pub use types::*;

use std::time::Duration;

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
