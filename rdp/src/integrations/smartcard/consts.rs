// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Smartcard protocol and error constants.

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
