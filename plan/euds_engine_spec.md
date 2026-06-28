# eUDS Engine Specification — `uds-client` / `crates/channels`

> **Status**: DESIGN REVIEW — Pending implementation
> **Target**: `crates/channels/src/smartcard/emulated/`
> **Platform**: Multiplatform (Windows, Linux, macOS)
> **Dependencies**: `rsa`, `num-bigint`, `rand`, `pem` (existing)

---

## 1. Overview

Replace the current GIDS-based `GidsEngine` with a new **eUDS Engine** implementing a minimal custom APDU protocol. The engine runs in the `uds-client` (multiplatform: Windows/Linux/macOS) and communicates with the Windows minidriver via the FreeRDP smartcard channel over RDP.

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Custom APDU protocol** (CLA=0x80) | Avoid GIDS/PIV complexity, full control |
| **Conditional PIN** | `EmptyPinType` if key unencrypted; `AlphaNumericPinType` if encrypted |
| **Per-connection session state** | Only if PIN required; stateless otherwise |
| **Extended APDU support** | Required for DECRYPT (256-byte ciphertext) and large certs |
| **Multiplatform** | Engine runs in `uds-client` on Windows/Linux/macOS |

---

## 2. File Structure

```
crates/channels/src/smartcard/emulated/
├── consts.rs              # Existing — ADD eUDS constants
├── helpers.rs             # Existing — VERIFY/EXTEND extended APDU parsing
├── euds_types.rs          # NEW — Core types (PinMode, SessionState, EudsEngine)
├── euds_engine.rs         # NEW — Main engine logic
├── mod.rs                 # MODIFY — Add EudsEngine as new emulated backend
├── tests.rs               # MODIFY — Replace GIDS tests with eUDS tests
├── types.rs               # DEPRECATE (move to euds_types.rs)
└── consts.rs              # ADD eUDS constants (INS, CLA, SW, ATR, AID)
```

**Files to create**: `euds_types.rs`, `euds_engine.rs`
**Files to modify**: `consts.rs`, `helpers.rs`, `mod.rs`, `tests.rs`
**File to deprecate**: `types.rs` (move relevant types to `euds_types.rs`)

---

## 3. Constants — `consts.rs` (Additions)

```rust
// ============================================================================
// eUDS Custom APDU Protocol Constants
// ============================================================================

// APDU Class
pub const EUDS_CLA: u8 = 0x80;           // Proprietary class
// APDU Instructions
pub const EUDS_INS_SELECT: u8 = 0xA4;    // Standard SELECT (CLA=0x00)
pub const EUDS_INS_VERIFY: u8 = 0xB1;    // Proprietary: VERIFY PIN
pub const EUDS_INS_GET_CERT: u8 = 0xB4;  // Proprietary: GET CERTIFICATE
pub const EUDS_INS_GET_PUBKEY: u8 = 0x46; // GET PUBLIC KEY (custom)
pub const EUDS_INS_GET_RESPONSE: u8 = 0xC0; // GET RESPONSE (chaining)
pub const EUDS_INS_SIGN: u8 = 0xB2;      // Proprietary: SIGN DATA
pub const EUDS_INS_DECRYPT: u8 = 0xB3;   // Proprietary: DECRYPT DATA

// PSO P1/P2 (used with SIGN and DECRYPT)
pub const EUDS_SIGN_P1: u8 = 0x9E;
pub const EUDS_SIGN_P2: u8 = 0x9A;
pub const EUDS_DEC_P1: u8 = 0x80;
pub const EUDS_DEC_P2: u8 = 0x86;

// eUDS Custom AID
pub const EUDS_AID: &[u8] = b"eUDS-Card"; // 9 bytes: 65 75 44 53 2D 43 61 72 64

// Status Words (add to existing)
pub const SW_SUCCESS: u16 = 0x9000;
pub const SW_MORE_DATA_BASE: u16 = 0x6100; // 61 XX
pub const SW_WRONG_LC: u16 = 0x6700;
pub const SW_COMMAND_NOT_ALLOWED: u16 = 0x6986;
pub const SW_SECURITY_STATUS_NOT_SATISFIED: u16 = 0x6982;
pub const SW_AUTH_METHOD_BLOCKED: u16 = 0x6983;
pub const SW_VERIFY_FAILED_BASE: u16 = 0x63C0; // 63 CX
pub const SW_REF_DATA_NOT_FOUND: u16 = 0x6A88;
pub const SW_FILE_NOT_FOUND: u16 = 0x6A82;
pub const SW_INVALID_P1P2: u16 = 0x6A86;
pub const SW_INVALID_COMMAND_DATA: u16 = 0x6A80;

// eUDS ATR (corrected ISO 7816-3)
pub const EUDS_ATR: &[u8] = &[
    0x3B, 0x89, 0x01, 0x45, 0x55, 0x44, 0x53, 0x2D, 0x43, 0x61, 0x72, 0x64, 0x96
];
// 3B 89 01 45 55 44 53 2D 43 61 72 64 96
// TS=3B, T0=89 (Y1=8, K=9), TD1=01 (T=1), H="eUDS-Card", TCK=96

// Reader name
pub const EUDS_READER_NAME: &str = "eUDS Virtual Smartcard Reader";
```

---

## 4. Core Types — `euds_types.rs` (NEW FILE)

```rust
// crates/channels/src/smartcard/emulated/euds_types.rs
//! Core types for eUDS Engine

use num_bigint::BigUint;
use rsa::RsaPrivateKey;
use std::collections::HashMap;

/// PIN requirement based on key encryption state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    /// Key is encrypted — PIN = passphrase to decrypt
    Required,
    /// Key is NOT encrypted — no PIN needed
    NotRequired,
}

/// Per-connection session state
#[derive(Debug, Clone)]
pub struct SessionState {
    pub pin_verified: bool,
    pub pin_retries: u8,
    /// Chaining state for responses > 256 bytes
    pub chaining_buffer: Option<Vec<u8>>,
}

/// Manual Default: pin_retries must start at DEFAULT_PIN_RETRIES (3), NOT 0
impl Default for SessionState {
    fn default() -> Self {
        SessionState {
            pin_verified: false,
            pin_retries: super::consts::DEFAULT_PIN_RETRIES,
            chaining_buffer: None,
        }
    }
}

/// Per-connection session key
pub type ConnectionId = u64;

/// APDU header parsed from raw APDU bytes
#[derive(Debug, Clone, Copy)]
pub struct ApduHeader {
    pub cla: u8,
    pub ins: u8,
    pub p1: u8,
    pub p2: u8,
}

/// Main eUDS Engine state
pub struct EudsEngine {
    /// Certificate in DER format
    pub cert_der: Vec<u8>,
    /// RSA private key
    pub private_key: RsaPrivateKey,
    /// PIN mode (determines PIN behavior)
    pub pin_mode: PinMode,
    /// PIN passphrase (only used if PinMode::Required)
    pub pin: String,
    /// RSA modulus (n)
    pub n: BigUint,
    /// RSA private exponent (d)
    pub d: BigUint,
    /// RSA public exponent (e) — extracted from key, NOT hardcoded
    pub e: BigUint,
    /// Key size in bytes (e.g., 256 for RSA-2048)
    pub key_size: usize,
    /// Per-connection session state
    pub sessions: HashMap<u64, SessionState>,
}

/// Default PIN retry count
pub const DEFAULT_PIN_RETRIES: u8 = 3;
```

---

## 5. Engine Implementation — `euds_engine.rs` (NEW FILE)

```rust
// crates/channels/src/smartcard/emulated/euds_engine.rs
//! eUDS Engine — Custom APDU protocol processor

use num_bigint::BigUint;
use rsa::RsaPrivateKey;
use std::collections::HashMap;

use super::consts::*;
use super::euds_types::*;
use super::helpers::*;

impl EudsEngine {
    /// Create new eUDS Engine
    pub fn new(
        cert_der: Vec<u8>,
        private_key: RsaPrivateKey,
        pin: String,
        pin_mode: PinMode,
    ) -> Self {
        // Extract n, e, d from the private key via PKCS#1 DER parsing
        let pkcs1_der = private_key
            .to_pkcs1_der()
            .expect("failed to serialize RSA key to PKCS#1 DER");
        let (n, e, d) = super::helpers::parse_rsa_pkcs1_components(pkcs1_der.as_bytes())
            .expect("failed to parse RSA PKCS#1 components (n, e, d)");
        let key_size = (n.bits() as usize).div_ceil(8);

        EudsEngine {
            cert_der,
            private_key,
            pin_mode,
            pin,
            n,
            d,
            e,
            key_size,
            sessions: HashMap::new(),
        }
    }

    /// Get or create session state for a connection
    fn get_session(&mut self, conn_id: ConnectionId) -> &mut SessionState {
        self.sessions
            .entry(conn_id)
            .or_insert_with(SessionState::default)
    }

    /// Main APDU processing entry point
    pub fn process_apdu(&mut self, conn_id: ConnectionId, apdu: &[u8]) -> Vec<u8> {
        // Parse APDU header (handles both short and extended)
        let Some(header) = parse_apdu_header(apdu) else {
            return make_status(SW_WRONG_LC);
        };
        let (data, le) = extract_apdu_data(apdu);

        // Clear stale chaining buffer on any new command (except GET RESPONSE)
        if header.ins != EUDS_INS_GET_RESPONSE {
            if let Some(session) = self.sessions.get_mut(&conn_id) {
                session.chaining_buffer = None;
            }
        }

        match header.ins {
            EUDS_INS_SELECT => self.select(header.p1, header.p2, data),
            EUDS_INS_VERIFY => self.verify_pin(conn_id, header.p1, header.p2, data),
            EUDS_INS_GET_CERT => self.get_certificate(conn_id, header.p1, header.p2, le),
            EUDS_INS_GET_PUBKEY => self.get_public_key(conn_id),
            EUDS_INS_GET_RESPONSE => self.get_response(conn_id, le),
            EUDS_INS_SIGN => {
                // PIN check before dispatch (engine needs &self for sessions)
                if self.pin_mode == PinMode::Required {
                    let session = self.get_session(conn_id);
                    if !session.pin_verified {
                        return make_status(SW_SECURITY_STATUS_NOT_SATISFIED);
                    }
                }
                self.sign(header.p1, header.p2, data)
            }
            EUDS_INS_DECRYPT => {
                // PIN check before dispatch (engine needs &self for sessions)
                if self.pin_mode == PinMode::Required {
                    let session = self.get_session(conn_id);
                    if !session.pin_verified {
                        return make_status(SW_SECURITY_STATUS_NOT_SATISFIED);
                    }
                }
                self.decrypt(header.p1, header.p2, data)
            }
            _ => make_status(SW_COMMAND_NOT_ALLOWED),
        }
    }

    // ---------------------------------------------------------------------
    // SELECT Applet (INS=0xA4, CLA=0x00)
    // ---------------------------------------------------------------------
    fn select(&self, p1: u8, p2: u8, data: &[u8]) -> Vec<u8> {
        // Only support SELECT by AID (P1=0x04)
        if p1 == 0x04 && data == EUDS_AID {
            make_status(SW_SUCCESS)
        } else {
            make_status(SW_FILE_NOT_FOUND)
        }
    }

    // ---------------------------------------------------------------------
    // VERIFY PIN (INS=0xB1, CLA=0x80)
    // ---------------------------------------------------------------------
    fn verify_pin(&mut self, conn_id: ConnectionId, p1: u8, p2: u8, data: &[u8]) -> Vec<u8> {
        // PIN not required mode (EmptyPinType) — always succeed
        if self.pin_mode == PinMode::NotRequired {
            return make_status(SW_SUCCESS);
        }

        // Only support P1=0x00, P2=0x80 (verify user PIN)
        if p1 != 0x00 || p2 != 0x80 {
            return make_status(SW_INVALID_P1P2);
        }

        let session = self.get_session(conn_id);

        // Check if blocked
        if session.pin_retries == 0 {
            return make_status(SW_AUTH_METHOD_BLOCKED);
        }

        // Verify PIN using constant-time comparison to prevent timing side channels
        if constant_time_eq(data, self.pin.as_bytes()) {
            session.pin_verified = true;
            session.pin_retries = DEFAULT_PIN_RETRIES;
            make_status(SW_SUCCESS)
        } else {
            session.pin_verified = false;
            session.pin_retries -= 1;
            let sw = SW_VERIFY_FAILED_BASE | (session.pin_retries as u16);
            make_status(sw)
        }
    }

    // ---------------------------------------------------------------------
    // GET CERTIFICATE (INS=0xB4, CLA=0x80) — Extended APDU Case 2
    // ---------------------------------------------------------------------
    fn get_certificate(&mut self, conn_id: ConnectionId, p1: u8, p2: u8, _le: Option<u16>) -> Vec<u8> {
        // Only support offset=0 (p1=0, p2=0)
        if p1 != 0x00 || p2 != 0x00 {
            return make_status(SW_INVALID_P1P2);
        }

        // Return full DER certificate using T=0 chaining
        let cert_der = self.cert_der.clone();
        self.handle_chaining(conn_id, &cert_der, None)
    }

// ---------------------------------------------------------------------
    // GET PUBLIC KEY (INS=0x46, CLA=0x80)
    // ---------------------------------------------------------------------
    fn get_public_key(&mut self, conn_id: ConnectionId) -> Vec<u8> {
        // Response format: raw length-prefixed (NOT TLV)
        // [exp_len:2 BE] [exponent] [mod_len:2 BE] [modulus]
        let exp_bytes = self.e.to_bytes_be(); // typically 3 bytes: 01 00 01
        let mod_bytes = self.n.to_bytes_be(); // 256 bytes for RSA-2048

        let mut resp = Vec::with_capacity(2 + exp_bytes.len() + 2 + mod_bytes.len());
        resp.extend_from_slice(&(exp_bytes.len() as u16).to_be_bytes());
        resp.extend_from_slice(&exp_bytes);
        resp.extend_from_slice(&(mod_bytes.len() as u16).to_be_bytes());
        resp.extend_from_slice(&mod_bytes);

        // Total: 2 + 3 + 2 + 256 = 263 bytes → may need chaining
        self.handle_chaining(conn_id, &resp, None)
    }

    // Handle T=0 chaining for responses > 256 bytes
    // `le` parameter limits chunk size for GET RESPONSE (ISO 7816-4 §7.6.1)
    fn handle_chaining(&mut self, conn_id: ConnectionId, data: &[u8], le: Option<u16>) -> Vec<u8> {
        let max_chunk = le.unwrap_or(256) as usize;
        
        if data.len() <= max_chunk && self.get_session(conn_id).chaining_buffer.is_none() {
            return make_response(data, SW_SUCCESS);
        }

        let session = self.get_session(conn_id);
        
        // Check if we're in the middle of a chaining sequence (GET RESPONSE)
        if let Some(buffer) = session.chaining_buffer.take() {
            // Return next chunk, limited by Le from GET RESPONSE
            let take = buffer.len().min(max_chunk);
            let chunk = &buffer[..take];
            let remaining = &buffer[take..];
            if remaining.is_empty() {
                make_response(chunk, SW_SUCCESS)
            } else {
                session.chaining_buffer = Some(remaining.to_vec());
                make_response(chunk, SW_MORE_DATA_BASE | (remaining.len().min(0xFF) as u16))
            }
        } else {
            // First call - store full response in buffer, return first chunk + 61 XX
            session.chaining_buffer = Some(data.to_vec());
            let chunk = &data[..max_chunk];
            let remaining = data.len() - max_chunk;
            make_response(chunk, SW_MORE_DATA_BASE | (remaining.min(0xFF) as u16))
        }
    }

    // ---------------------------------------------------------------------
    // GET RESPONSE (INS=0xC0, CLA=0x80 — must match original command's CLA per ISO 7816-4 §7.6.1)
    // ---------------------------------------------------------------------
    fn get_response(&mut self, conn_id: ConnectionId, le: Option<u16>) -> Vec<u8> {
        self.handle_chaining(conn_id, &[], le)
    }

    // ---------------------------------------------------------------------
    // SIGN DATA (INS=0xB2, CLA=0x80)
    // ---------------------------------------------------------------------
    fn sign(&self, p1: u8, p2: u8, data: &[u8]) -> Vec<u8> {
        if p1 != EUDS_SIGN_P1 || p2 != EUDS_SIGN_P2 {
            return make_status(SW_INVALID_P1P2);
        }
        // Check PIN if required
        if self.pin_mode == PinMode::Required {
            // PIN check handled in process_apdu before dispatch
        }
        // Input: DigestInfo structure (e.g., SHA-256 = 51 bytes)
        // Output: Raw RSA signature (256 bytes for RSA-2048)
        match self.rsa_pkcs1_sign(data) {
            Ok(sig) => make_response(&sig, SW_SUCCESS),
            Err(e) => {
                log::error!("eUDS: RSA sign failed: {}", e);
                make_status(SW_INVALID_COMMAND_DATA)
            }
        }
    }

    // ---------------------------------------------------------------------
    // DECRYPT DATA (INS=0xB3, CLA=0x80) — Extended APDU Case 4
    // ---------------------------------------------------------------------
    fn decrypt(&self, p1: u8, p2: u8, ciphertext: &[u8]) -> Vec<u8> {
        if p1 != EUDS_DEC_P1 || p2 != EUDS_DEC_P2 {
            return make_status(SW_INVALID_P1P2);
        }
        // Input: 256 bytes (RSA-2048 block) — sent via Extended APDU Case 4
        // C-APDU: 80 B3 80 86 00 01 00 [256 bytes] 00 00

        // Default to PKCS#1 v1.5 unpad (minidriver should send padding info via separate mechanism if OAEP needed)
        match self.rsa_decrypt_pkcs1(ciphertext) {
            Ok(pt) => make_response(&pt, SW_SUCCESS),
            Err(e) => {
                log::error!("eUDS: RSA decrypt failed: {}", e);
                make_status(SW_INVALID_COMMAND_DATA)
            }
        }
    }

    // ---------------------------------------------------------------------
    // RSA Operations
    // ---------------------------------------------------------------------
    fn rsa_raw(&self, value: &[u8]) -> Vec<u8> {
        let v = BigUint::from_bytes_be(value);
        let result = v.modpow(&self.d, &self.n);
        let mut bytes = result.to_bytes_be();
        if bytes.len() < self.key_size {
            let mut padded = vec![0u8; self.key_size - bytes.len()];
            padded.extend_from_slice(&bytes);
            bytes = padded;
        }
        bytes
    }

    fn rsa_pkcs1_sign(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() + 11 > self.key_size {
            return Err("Data too large".to_string());
        }
        let mut em = vec![0u8; self.key_size];
        em[0] = 0x00;
        em[1] = 0x01;
        let ps_len = self.key_size - data.len() - 3;
        for i in 0..ps_len {
            em[2 + i] = 0xFF;
        }
        em[2 + ps_len] = 0x00;
        em[3 + ps_len..].copy_from_slice(data);
        Ok(self.rsa_raw(&em))
    }

    fn rsa_decrypt_pkcs1(&self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let em = self.rsa_raw(ciphertext);
        if em.len() < 11 || em[0] != 0x00 || em[1] != 0x02 {
            return Err("Invalid padding".to_string());
        }
        let sep = em[2..].iter().position(|&b| b == 0x00);
        match sep {
            Some(idx) if idx >= 8 => Ok(em[3 + idx..].to_vec()),
            _ => Err("Invalid padding".to_string()),
        }
    }

    // OAEP decrypt (future: if minidriver requests OAEP via padding info)
    fn rsa_decrypt_oaep(&self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let em = self.rsa_raw(ciphertext);
        if em.len() < 42 || em[0] != 0x00 {
            return Err("Invalid OAEP".to_string());
        }
        let h_len = 20;
        let seed_mask = mgf1(&em[1 + h_len..], h_len);
        let seed: Vec<u8> = em[1..1 + h_len]
            .iter()
            .zip(seed_mask.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let db_mask = mgf1(&seed, em.len() - 1 - h_len);
        let db: Vec<u8> = em[1 + h_len..]
            .iter()
            .zip(db_mask.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let sep = db[h_len..].iter().position(|&b| b == 0x01);
        match sep {
            Some(idx) => Ok(db[h_len + idx + 1..].to_vec()),
            None => Err("Invalid OAEP".to_string()),
        }
    }

    // ---------------------------------------------------------------------
    // Public Key Components (for GET PUBLIC KEY)
    // ---------------------------------------------------------------------
    pub fn public_exponent_bytes(&self) -> Vec<u8> {
        self.e.to_bytes_be()
    }

    pub fn modulus_bytes(&self) -> Vec<u8> {
        self.n.to_bytes_be()
    }
}

// Note: Drop impl with zeroize omitted for MVP.
// To enable: add "zeroize" to rsa crate features, then implement:
//   impl Drop for EudsEngine {
//       fn drop(&mut self) {
//           use zeroize::Zeroize;
//           self.private_key.zeroize();
//           self.pin.zeroize();
//           self.sessions.clear();
//       }
//   }

// -------------------------------------------------------------------------
// Helper: Response builders (move to helpers.rs or keep here)
// -------------------------------------------------------------------------
fn make_status(sw: u16) -> Vec<u8> {
    vec![(sw >> 8) as u8, sw as u8]
}

fn make_response(data: &[u8], sw: u16) -> Vec<u8> {
    let mut resp = data.to_vec();
    resp.push((sw >> 8) as u8);
    resp.push(sw as u8);
    resp
}

// MGF1 for OAEP (reuse existing from helpers if available)
fn mgf1(seed: &[u8], len: usize) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    let mut out = Vec::with_capacity(len);
    let mut counter = 0u32;
    while out.len() < len {
        let mut hasher = Sha1::new();
        hasher.update(seed);
        hasher.update(&counter.to_be_bytes());
        let hash = hasher.finalize();
        out.extend_from_slice(&hash);
        counter += 1;
    }
    out.truncate(len);
    out
}
```

---

## 6. Helpers Extension — `helpers.rs` (Verify/Extend)

```rust
// Ensure these functions exist and handle extended APDUs correctly:

/// Parse APDU header — returns (cla, ins, p1, p2)
pub fn parse_apdu_header(apdu: &[u8]) -> Option<ApduHeader> {
    if apdu.len() < 4 {
        return None;
    }
    Some(ApduHeader {
        cla: apdu[0],
        ins: apdu[1],
        p1: apdu[2],
        p2: apdu[3],
    })
}

/// Extract data and Le from APDU — MUST handle extended APDU Case 2 and Case 4
/// Case 2 (no data in, data out): CLA INS P1 P2 00 Le_hi Le_lo
/// Case 4 (data in, data out): CLA INS P1 P2 00 Lc_hi Lc_lo [data] Le_hi Le_lo
pub fn extract_apdu_data(apdu: &[u8]) -> (&[u8], Option<u16>) {
    let len = apdu.len();
    if len <= 4 {
        return (&[], None);
    }

    if len == 5 {
        // Case 2 short: CLA INS P1 P2 Le
        let le = apdu[4] as u16;
        let le = if le == 0 { 256 } else { le };
        return (&[], Some(le));
    }

    let first_len_byte = apdu[4];
    if first_len_byte != 0 {
        // Short format (Case 3 or Case 4)
        let lc = first_len_byte as usize;
        if 5 + lc <= len {
            let data = &apdu[5..5 + lc];
            let le = if 5 + lc < len {
                let val = apdu[5 + lc] as u16;
                Some(if val == 0 { 256 } else { val })
            } else {
                None
            };
            return (data, le);
        }
    } else {
        // Extended format
        if len == 7 {
            // Case 2 extended: CLA INS P1 P2 00 Le_hi Le_lo
            let le = ((apdu[5] as u16) << 8) | (apdu[6] as u16);
            let le = if le == 0 { 65536 } else { le };
            return (&[], Some(le));
        }
        if len >= 7 {
            let lc = ((apdu[5] as usize) << 8) | (apdu[6] as usize);
            if 7 + lc <= len {
                let data = &apdu[7..7 + lc];
                let le = if 7 + lc + 2 <= len {
                    let val = ((apdu[7 + lc] as u16) << 8) | (apdu[7 + lc + 1] as u16);
                    Some(if val == 0 { 65536 } else { val })
                } else if 7 + lc + 1 <= len {
                    let val = apdu[7 + lc] as u16;
                    Some(if val == 0 { 256 } else { val })
                } else {
                    None
                };
                return (data, le);
            }
        }
    }

    (&[], None)
}

/// Parse RSA PKCS#1 private key DER → (n, e, d)
/// PKCS#1 structure: SEQUENCE { version, modulus(n), publicExponent(e), privateExponent(d), ... }
pub fn parse_rsa_pkcs1_components(der: &[u8]) -> Option<(BigUint, BigUint, BigUint)> {
    let mut pos = 0;
    if der[pos] != 0x30 {
        return None;
    }
    pos += 1;
    pos += read_der_length(&der[pos..])?.1;
    // Skip version INTEGER
    let (_, after) = read_integer(&der[pos..])?;
    pos += after;
    // Read modulus (n)
    let (n, after) = read_integer(&der[pos..])?;
    pos += after;
    // Read public exponent (e) — previously skipped!
    let (e, after) = read_integer(&der[pos..])?;
    pos += after;
    // Read private exponent (d)
    let (d, _) = read_integer(&der[pos..])?;
    Some((n, e, d))
}

fn read_der_length(data: &[u8]) -> Option<(usize, usize)> {
    if data.is_empty() {
        return None;
    }
    let first = data[0];
    if first < 0x80 {
        Some((first as usize, 1))
    } else {
        let num_bytes = (first & 0x7F) as usize;
        if num_bytes > 4 || data.len() < 1 + num_bytes {
            return None;
        }
        let mut len = 0usize;
        for i in 0..num_bytes {
            len = (len << 8) | (data[1 + i] as usize);
        }
        Some((len, 1 + num_bytes))
    }
}

fn read_integer(data: &[u8]) -> Option<(BigUint, usize)> {
    if data.is_empty() || data[0] != 0x02 {
        return None;
    }
    let (len, len_size) = read_der_length(&data[1..])?;
    let start = 1 + len_size;
    let end = start + len;
    if end > data.len() {
        return None;
    }
    let value = if len > 0 && data[start] == 0 {
        BigUint::from_bytes_be(&data[start + 1..end])
    } else {
        BigUint::from_bytes_be(&data[start..end])
    };
    Some((value, end))
}

/// TLV encoding
pub fn tlv_write(buf: &mut Vec<u8>, tag: u16, data: &[u8]) {
    if tag > 0xFF {
        buf.push((tag >> 8) as u8);
        buf.push((tag & 0xFF) as u8);
    } else {
        buf.push(tag as u8);
    }
    tlv_write_length(buf, data.len());
    buf.extend_from_slice(data);
}

fn tlv_write_length(buf: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        buf.push(len as u8);
    } else if len < 0x100 {
        buf.push(0x81);
        buf.push(len as u8);
    } else {
        buf.push(0x82);
        buf.push((len >> 8) as u8);
        buf.push((len & 0xFF) as u8);
    }
}

pub fn tlv_find(data: &[u8], tag: u16) -> Option<&[u8]> {
    let mut offset = 0;
    while offset < data.len() {
        if offset >= data.len() {
            break;
        }
        let first_byte = data[offset];
        let (current_tag, tag_len) = if (first_byte & 0x1F) == 0x1F && offset + 1 < data.len() {
            (((first_byte as u16) << 8) | (data[offset + 1] as u16), 2)
        } else {
            (first_byte as u16, 1)
        };
        offset += tag_len;
        if offset >= data.len() {
            break;
        }

        let first_len_byte = data[offset];
        offset += 1;
        let value_len = if first_len_byte < 0x80 {
            first_len_byte as usize
        } else if first_len_byte == 0x81 {
            if offset >= data.len() {
                break;
            }
            let l = data[offset] as usize;
            offset += 1;
            l
        } else if first_len_byte == 0x82 {
            if offset + 1 >= data.len() {
                break;
            }
            let l = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
            offset += 2;
            l
        } else {
            break;
        };

        if current_tag == tag {
            return Some(&data[offset..offset + value_len]);
        }
        offset += value_len;
    }
    None
}

/// Constant-time byte array comparison (prevents timing side channels)
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}
```
```
```

**Verify existing `helpers.rs` has these** — if not, add them.

---

## 7. Integration — `mod.rs` (MODIFICATIONS)

```rust
// crates/channels/src/smartcard/emulated/mod.rs

// 1. New imports
mod euds_types;
mod euds_engine;
use self::euds_types::{PinMode, SessionState, ConnectionId};
use self::euds_engine::EudsEngine;
use self::consts::*;

// 2. Update EmulatedBackend
pub(crate) struct EmulatedBackend {
    engine: Mutex<EudsEngine>,
}

// 3. Update status() with new ATR
fn status(&self, _: &ScardHandle) -> Result<ScardStatus, u32> {
    Ok(ScardStatus {
        reader_names: vec![EUDS_READER_NAME.to_string()],
        state: SCARD_STATE_PRESENT,
        protocol: SCARD_PROTOCOL_T1,
        atr: EUDS_ATR.to_vec(),  // NEW ATR
    })
}

// 4. Update get_status_change() ATR
fn get_status_change(...) {
    // Use EUDS_ATR in ReaderStateOut
    atr: EUDS_ATR.to_vec(),
    ...
}

// 4. try_from_env() — detect PinMode from PEM header
impl EmulatedBackend {
    pub fn try_from_env() -> Option<Self> {
        let cert_path = std::env::var("UDS_SMARTCARD_CERT_PEM").ok()?;
        let key_path = std::env::var("UDS_SMARTCARD_KEY_PEM").ok()?;
        let pin = std::env::var("UDS_SMARTCARD_PIN").unwrap_or_default();

        let cert_pem = std::fs::read_to_string(&cert_path).ok()?;
        let key_pem = std::fs::read_to_string(&key_path).ok()?;

        let cert_der = pem::parse(&cert_pem).ok()?.into_contents();
        let private_key = RsaPrivateKey::from_pkcs8_pem(&key_pem).ok()?;

        // Detect if key is encrypted
        let pin_mode = if key_pem.contains("ENCRYPTED") || key_pem.contains("ENCRYPTED PRIVATE KEY") {
            PinMode::Required
        } else {
            PinMode::NotRequired
        };

        let engine = EudsEngine::new(cert_der, private_key, pin, pin_mode);
        Some(EmulatedBackend {
            engine: Mutex::new(engine),
        })
    }
}

// 5. transmit() — pass connection_id to engine
fn transmit(&self, handle: &ScardHandle, _: &ScardIORequest, data: &[u8]) -> Result<TransmitResult, u32> {
    let mut engine = self.engine.lock().map_err(|_| SCARD_F_INTERNAL_ERROR)?;
    // ScardHandle has a private u64 field — use .raw() accessor
    let conn_id = handle.raw();
    Ok(TransmitResult {
        recv_pci: None,
        recv_buffer: engine.process_apdu(conn_id, data),
    })
}
```

---

## 8. Tests — `tests.rs` (REPLACE)

```rust
// crates/channels/src/smartcard/emulated/tests.rs

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPrivateKey;

    fn test_engine(pin_mode: PinMode) -> EudsEngine {
        let private_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let cert_der = vec![0x30, 0x82]; // dummy
        let pin = if pin_mode == PinMode::Required { "testpin" } else { "" };
        EudsEngine::new(cert_der, private_key, pin.to_string(), pin_mode)
    }

    #[test]
    fn test_select_euds_applet() {
        let mut engine = test_engine(PinMode::NotRequired);
        let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, 0x09];
        apdu.extend(EUDS_AID);
        apdu.push(0x00); // Le = 00 (expect FCI response per ISO 7816-4 §7.1.1)
        let resp = engine.process_apdu(1, &apdu);
        assert_eq!(resp, vec![0x90, 0x00]);
    }

    #[test]
    fn test_select_wrong_aid() {
        let mut engine = test_engine(PinMode::NotRequired);
        let apdu = vec![0x00, 0xA4, 0x04, 0x00, 0x05, 0x00, 0x01, 0x02, 0x03, 0x04, 0x00];
        let resp = engine.process_apdu(1, &apdu);
        assert_eq!(resp[0], 0x6A); // SW_FILE_NOT_FOUND
    }

    #[test]
    fn test_verify_pin_not_required() {
        let mut engine = test_engine(PinMode::NotRequired);
        let apdu = vec![0x80, 0xB1, 0x00, 0x80, 0x04, 0x31, 0x32, 0x33, 0x34];
        let resp = engine.process_apdu(1, &apdu);
        assert_eq!(resp, vec![0x90, 0x00]); // Always success
    }

    #[test]
    fn test_verify_pin_correct() {
        let mut engine = test_engine(PinMode::Required);
        let apdu = vec![0x80, 0xB1, 0x00, 0x80, 0x04, 0x74, 0x65, 0x73, 0x74];
        let resp = engine.process_apdu(1, &apdu);
        assert_eq!(resp, vec![0x90, 0x00]);
    }

    #[test]
    fn test_verify_pin_wrong() {
        let mut engine = test_engine(PinMode::Required);
        let apdu = vec![0x80, 0xB1, 0x00, 0x80, 0x05, 0x77, 0x72, 0x6F, 0x6E, 0x67];
        let resp = engine.process_apdu(1, &apdu);
        assert_eq!(resp[0], 0x63); // SW_VERIFY_FAILED
        assert_eq!(resp[1], 0xC2); // 2 retries left
    }

    #[test]
    fn test_get_certificate() {
        let mut engine = test_engine(PinMode::NotRequired);
        // Extended APDU Case 2: 80 B4 00 00 00 00
        let apdu = vec![0x80, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x00];
        let resp = engine.process_apdu(1, &apdu);
        assert!(resp.ends_with(&[0x90, 0x00]));
        assert_eq!(resp.len(), 2 + 2); // 2 bytes cert + 2 bytes SW
    }

    #[test]
    fn test_get_large_certificate_chaining() {
        let private_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let large_cert_der = vec![0x42; 300]; // 300 bytes cert > 256
        let mut engine = EudsEngine::new(large_cert_der, private_key, "".to_string(), PinMode::NotRequired);

        // Extended APDU Case 2
        let apdu = vec![0x80, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x00];
        let resp = engine.process_apdu(1, &apdu);

        // First chunk: 256 bytes + 61 2C (44 bytes remaining)
        assert_eq!(resp.len(), 256 + 2);
        assert_eq!(resp[256], 0x61);
        assert_eq!(resp[257], 44);

        // Fetch remaining 44 bytes via GET RESPONSE: 80 C0 00 00 2C (CLA=0x80 matches original command)
        let get_resp_apdu = vec![0x80, 0xC0, 0x00, 0x00, 44];
        let resp2 = engine.process_apdu(1, &get_resp_apdu);
        assert_eq!(resp2.len(), 44 + 2);
        assert!(resp2.ends_with(&[0x90, 0x00]));
    }

    #[test]
    fn test_get_public_key() {
        let mut engine = test_engine(PinMode::NotRequired);
        // Extended APDU Case 2: 80 46 00 00 00 01 07 (Le=263)
        let apdu = vec![0x80, 0x46, 0x00, 0x00, 0x00, 0x01, 0x07];
        let resp = engine.process_apdu(1, &apdu);
        
        // Response > 256 bytes, so first chunk must return 256 bytes of data + 61 07 (7 bytes remaining)
        assert_eq!(resp.len(), 256 + 2);
        assert_eq!(resp[256], 0x61);
        assert_eq!(resp[257], 0x07);

        // Fetch remaining 7 bytes via GET RESPONSE APDU: 80 C0 00 00 07 (CLA=0x80 matches original)
        let get_resp_apdu = vec![0x80, 0xC0, 0x00, 0x00, 0x07];
        let resp2 = engine.process_apdu(1, &get_resp_apdu);
        assert_eq!(resp2.len(), 7 + 2);
        assert!(resp2.ends_with(&[0x90, 0x00]));
    }

    #[test]
    fn test_sign_without_pin_fails() {
        let mut engine = test_engine(PinMode::Required);
        // SIGN without PIN verified — valid APDU with Lc=1, 1 dummy byte, Le=00
        let apdu = vec![0x80, 0xB2, 0x9E, 0x9A, 0x01, 0x00, 0x00];
        let resp = engine.process_apdu(1, &apdu);
        assert_eq!(resp, vec![0x69, 0x82]); // SW_SECURITY_STATUS_NOT_SATISFIED
    }

    #[test]
    fn test_sign_with_pin() {
        let mut engine = test_engine(PinMode::Required);
        // Verify PIN first
        let verify = vec![0x80, 0xB1, 0x00, 0x80, 0x04, 0x74, 0x65, 0x73, 0x74];
        engine.process_apdu(1, &verify);
        // SIGN with dummy digest info
        let mut sign = vec![0x80, 0xB2, 0x9E, 0x9A, 0x20]; // Lc=32
        sign.extend_from_slice(&[0x30; 32]); // dummy DigestInfo
        sign.push(0x00); // Le=00 (expect 256 bytes response)
        let resp = engine.process_apdu(1, &sign);
        assert!(resp.ends_with(&[0x90, 0x00]));
        assert_eq!(resp.len(), 256 + 2); // 256 byte sig + SW
    }

    #[test]
    fn test_decrypt_extended_apdu() {
        use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
        let private_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let cert_der = vec![0x30, 0x82];
        let mut engine = EudsEngine::new(cert_der, private_key.clone(), "testpin".to_string(), PinMode::Required);
        // Verify PIN
        let verify = vec![0x80, 0xB1, 0x00, 0x80, 0x04, 0x74, 0x65, 0x73, 0x74];
        engine.process_apdu(1, &verify);

        // Encrypt a known plaintext with the public key
        let pub_key = RsaPublicKey::from(&private_key);
        let plaintext = b"eUDS test payload!";
        let ciphertext = pub_key.encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, plaintext).unwrap();
        assert_eq!(ciphertext.len(), 256); // RSA-2048

        // DECRYPT Extended APDU Case 4: 80 B3 80 86 00 01 00 [256B] 00 00
        let mut decrypt = vec![0x80, 0xB3, 0x80, 0x86, 0x00, 0x01, 0x00];
        decrypt.extend(&ciphertext);
        decrypt.extend([0x00, 0x00]); // Le = 2 bytes (extended, 0 = max)
        let resp = engine.process_apdu(1, &decrypt);
        assert!(resp.ends_with(&[0x90, 0x00]));
        // Verify decrypted data matches original
        let decrypted = &resp[..resp.len() - 2];
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_empty_pin_mode_no_pin_needed_for_sign() {
        let mut engine = test_engine(PinMode::NotRequired);
        // Sign directly without PIN
        let mut sign = vec![0x80, 0xB2, 0x9E, 0x9A, 0x20];
        sign.extend(vec![0x30; 32]);
        sign.push(0x00); // Le=00
        let resp = engine.process_apdu(1, &sign);
        assert!(resp.ends_with(&[0x90, 0x00]));
    }

    #[test]
    fn test_atr_correct() {
        let engine = test_engine(PinMode::NotRequired);
        // ATR check would be in EmulatedBackend::status()
        // Verify constant
        assert_eq!(EUDS_ATR, &[
            0x3B, 0x89, 0x01, 0x45, 0x55, 0x44, 0x53, 0x2D,
            0x43, 0x61, 0x72, 0x64, 0x96
        ]);
    }

    #[test]
    fn test_per_connection_state() {
        let mut engine = test_engine(PinMode::Required);
        // Connection 1: verify PIN
        let verify = vec![0x80, 0xB1, 0x00, 0x80, 0x04, 0x74, 0x65, 0x73, 0x74];
        engine.process_apdu(1, &verify);
        // Connection 2: should NOT have PIN verified
        let sign = vec![0x80, 0xB2, 0x9E, 0x9A, 0x01, 0x00, 0x00];
        let resp = engine.process_apdu(2, &sign);
        assert_eq!(resp, vec![0x69, 0x82]); // Not verified on conn 2
    }
}
```

---

## 9. Cargo.toml Dependencies (Verify)

```toml
# crates/channels/Cargo.toml — verify these exist
[dependencies]
rsa = { version = "0.9", features = ["pkcs1v15", "pkcs8", "sha1", "sha2", "pkcs1"] }
num-bigint = "0.4"
num-traits = "0.2"
pem = "1.0"
rand = "0.8"
sha1 = "0.10"
sha2 = "0.10"
rand_core = "0.6"
zeroize = "1.0"  # for future key zeroization
```

---

## 10. Environment Variables (Documentation)

```bash
# ============================================================================
# eUDS Smartcard Environment Variables
# ============================================================================

# REQUIRED: Enable emulated smartcard
UDS_SMARTCARD_EMULATED=1

# REQUIRED: Certificate PEM file path
UDS_SMARTCARD_CERT_PEM=/path/to/cert.pem

# REQUIRED: Private key PEM file path
# If key is ENCRYPTED (PEM header contains "ENCRYPTED"):
#   - PIN required (PinMode::Required)
#   - UDS_SMARTCARD_PIN = passphrase to decrypt key
# If key is NOT encrypted:
#   - PIN not required (PinMode::NotRequired)
#   - UDS_SMARTCARD_PIN can be empty or omitted
UDS_SMARTCARD_KEY_PEM=/path/to/key.pem

# OPTIONAL: PIN/passphrase
# Required only if key is encrypted
UDS_SMARTCARD_PIN=mypassphrase

# Example: Unencrypted key (testing) — NO PIN needed
# UDS_SMARTCARD_EMULATED=1
# UDS_SMARTCARD_CERT_PEM=/tmp/cert.pem
# UDS_SMARTCARD_KEY_PEM=/tmp/key_unencrypted.pem
# UDS_SMARTCARD_PIN=

# Example: Encrypted key (production) — PIN required
# UDS_SMARTCARD_EMULATED=1
# UDS_SMARTCARD_CERT_PEM=/tmp/cert.pem
# UDS_SMARTCARD_KEY_PEM=/tmp/key_encrypted.pem
# UDS_SMARTCARD_PIN=mysecurepassphrase
```

---

## 11. Implementation Checklist

| Step | File | Action | Status |
|------|------|--------|--------|
| 1 | `euds_types.rs` | Create new file with PinMode, SessionState, EudsEngine struct | ⏳ |
| 2 | `euds_engine.rs` | Create new file with process_apdu + 6 APDU handlers | ⏳ |
| 3 | `consts.rs` | Add eUDS constants (INS, CLA, SW, ATR, AID) | ⏳ |
| 4 | `helpers.rs` | Verify extended APDU parsing (Case 2, Case 4) | ⏳ |
| 5 | `mod.rs` | Add EudsEngine alongside existing backends, update ATR, PinMode detection | ⏳ |
| 6 | `tests.rs` | Replace GIDS tests with eUDS tests | ⏳ |
| 7 | `types.rs` | Deprecate (move VirtualFs, SessionState to euds_types if needed) | ⏳ |
| 8 | `Cargo.toml` | Verify deps (rsa, num-bigint, rand, sha1, sha2, zeroize) | ⏳ |
| 9 | Build & Test | `cargo test -p channels smartcard::emulated` | ⏳ |

---

## 12. Notes for Implementation

1. **Connection ID**: Use `ScardHandle`'s internal ID as `ConnectionId`. The handle is created per `connect()` call.

2. **Session Cleanup**: Sessions persist for the lifetime of the engine. In production, consider TTL cleanup, but for MVP it's fine.

3. **Extended APDU**: Critical for DECRYPT (256-byte ciphertext > 255 short APDU limit). Verify `helpers.rs` handles Case 4 correctly.

5. **OAEP Decrypt**: Currently only PKCS#1 v1.5 implemented. Add OAEP if minidriver requests it (future).

6. **Zeroize**: Use `zeroize` crate for sensitive data in `Drop` impl (future hardening).

5. **Thread Safety**: `Mutex<EudsEngine>` in `EmulatedBackend` — fine for MVP.

6. **No VirtualFs**: eUDS engine does NOT implement GIDS virtual filesystem. Minidriver serves all files.

---

## 13. Review Checklist for External AI

When reviewing, please verify:

- [ ] APDU formats match spec (Case 2 for GET CERT, Case 4 for DECRYPT)
- [ ] SIGN APDU includes Le=00 (short) or Le=0000 (extended) — **critical**
- [ ] DECRYPT uses Extended APDU Case 4 with Le=0000
- [ ] GET PUBLIC KEY response format (raw length-prefixed: exp_len+exp+mod_len+mod) is parseable by minidriver
- [ ] PinMode detection from PEM header is correct
- [ ] EmptyPinType → no PIN APDU needed, SIGN/DECRYPT work directly
- [ ] Per-connection session state isolation works
- [ ] ATR is correct ISO 7816-3 (TCK verified)
- [ ] No GIDS VFS types leak into eUDS engine
- [ ] Tests cover both PinMode variants
- [ ] Extended APDU parsing in helpers.rs handles Case 2 and Case 4
- [ ] No panic paths in process_apdu (use Result, return status words)

---

**End of Specification**

This document is the complete blueprint for implementing the eUDS Engine in `uds-client/crates/channels`. Ready for external AI review.