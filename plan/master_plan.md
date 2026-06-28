# Project eUDS: The Custom Virtual Smart Card & Minidriver Master Plan

> **Detailed technical specification**: See `plan/euds_technical_spec.md` for ATR validation, APDU protocol details, buffer constraints, and complete enumeration flow.

## 1. Vision & Strategy

The goal of Project **eUDS** is to build a highly reliable, custom virtual smart card ecosystem for UDS Enterprise. 

Instead of struggling with generic, opaque, and complex standards like GIDS/PIV and fighting Microsoft's inbox minidriver (`msclmd.dll`), we will build:
1. **eUDS Custom Card**: An emulated smartcard running on the Linux client that responds to a minimal, high-speed custom APDU protocol.
2. **eUDS Minidriver**: A 100% custom Windows Smart Card Minidriver DLL (`euds_minidriver.dll`) paired with the eUDS card via a custom ATR.

By doing this, we control both ends of the wire. We keep the APDU protocol extremely thin and delegate card system complexity to the minidriver, guaranteeing 100% stable container enumeration, certificate display, and card enrollment on Windows.

---

## 2. Core Architecture

```
┌────────────────────────────────────────────────────────────────────────────┐
│                              WINDOWS DESKTOP                               │
│                                                                            │
│       Base CSP / KSP ───► euds_minidriver.dll ◄───[Reads certs directly]   │
│                                  │                                         │
│                                  ▼ SCardTransmit (Custom APDUs)            │
│                              SCardSvr                                      │
└──────────────────────────────────┬─────────────────────────────────────────┘
                                   │ RDP Redirect (MS-RDPESC)
┌──────────────────────────────────▼─────────────────────────────────────────┐
│                            LINUX CLIENT (UDS)                              │
│                                                                            │
│       FreeRDP smartcard channel ───► eUDS Engine (Virtual Card)            │
│                                           │                                │
│                                           ▼                                │
│                                  UDS Smartcard Backend                     │
│                                  (Env vars, Cert PEM, Private Key)         │
└────────────────────────────────────────────────────────────────────────────┘
```

### 2.1 The Division of Labor
- **The Minidriver (Windows)**: Handles all Windows Smart Card Module API requests. It serves static layout files (such as `cardcf`, `cmapfile`, and public key blobs) directly from memory or configuration, bypassing card-side reads. It manages certificate extraction and decompression.
- **The Engine (Linux)**: Emulates the eUDS smart card. It registers a custom eUDS ATR. It is responsible for only two actions: **verifying the PIN** and **performing private key cryptographic operations (signing / decryption)**.

---

## 3. The Custom Card: eUDS Card Specs

### 3.1 ATR (Answer To Reset)
We will register a custom ATR that represents eUDS and guarantees perfect Plug and Play (PnP) discovery on Windows:
```
3B 89 00 45 55 44 53 2D 43 61 72 64 97
```
- **T0 = 0x89**: Y1=8 (only TD1 present), K=9 (9 historical bytes)
- **TD1 = 0x00**: T=0 protocol, no more interface bytes
- **Historical Bytes**: `45 55 44 53 2D 43 61 72 64` = ASCII `"eUDS-Card"` (9 bytes)
- **TCK = 0x97**: XOR checksum (T0..H9 XOR = 00 verified)
- **Total length**: 13 bytes (well within 36-byte RDP limit)

> **NOTE**: The previous ATR `3B F7 18 00 00 80 31 FE ...` was malformed — interface byte chain consumed bytes meant as historical, leaving only 7 historical bytes instead of 9. See `plan/euds_technical_spec.md` Section 1 for full analysis.

### 3.2 File System & Storage (Minidriver-managed)
The eUDS card does **not** need to implement an actual filesystem or BER-TLV directory. The minidriver will fake the filesystem layer. When the Base CSP asks for files, the minidriver serves them instantly:

- **`cardid`**: Returns a static unique GUID.
- **`cardcf`**: Returns `[0x01, 0x01, 0x01, 0x00, 0x01, 0x00]` (CARD_CACHE_FILE_FORMAT: version=1, pinsFreshness=1, containersFreshness=1, filesFreshness=1).
- **`cardapps`**: Returns `"mscp\0\0\0\0"`.
- **`mscp\cmapfile`**: Returns our custom container map:
  - Container Index: `0`
  - Container Name: `"eUDS Container 00"`
  - Key Exchange Key size: `2048` bits
  - Signature Key size: `0` (we use a single-key card design)
  - Flags: `0x03` (VALID | DEFAULT)
- **`mscp\kxc00`**: Returns the user's X.509 certificate in raw DER format (no compression).
  - The minidriver fetches the certificate via the `GET CERTIFICATE` APDU, caches it, and serves it directly.
  - `CARD_CAPABILITIES.fCertificateCompression = FALSE` tells the Base CSP to read DER directly.

---

## 4. The eUDS Custom APDU Protocol (The Wire)

To keep communications high-speed and rock-solid, we define a minimal set of APDUs. We use a custom CLA `0x80` (Proprietary) to avoid collisions with standard ISO classes.

### 4.1 SELECT Applet (Optional but clean)
- **Command (C-APDU)**: `00 A4 04 00 09 45 55 44 53 2D 43 61 72 64` (SELECT AID `"eUDS-Card"`)
- **Response (R-APDU)**: `90 00` (Success)

### 4.2 VERIFY PIN (INS 0x20)
- **Command (C-APDU)**: `80 20 00 80 [Lc] [PIN_Bytes]`
  - `P1` = `00`
  - `P2` = `80` (Verify User PIN)
- **Response (R-APDU)**:
  - `90 00` = PIN OK
  - `63 CX` = PIN Wrong, X retries remaining (e.g. `63 C2` = 2 left)
  - `69 83` = PIN Blocked

### 4.3 GET CERTIFICATE (INS 0xB0)
Used once on startup by the minidriver to fetch the X.509 certificate in DER format.
- **Command (C-APDU)**: `80 B0 00 00 00 00 00` (Extended APDU case 2: READ BINARY, offset=0, Le=0=max)
- **Response (R-APDU)**: `[DER_Bytes] 90 00`
- **Notes**: Uses extended APDU case 2 format (no Lc, Le=2 bytes = 00 00). The FreeRDP addon handles GET RESPONSE chaining automatically if the engine chunks the response.

### 4.4 GET PUBLIC KEY (INS 0x46) — NEW
Used by the minidriver to get RSA public key components for `CardGetContainerInfo`.
- **Command (C-APDU)**: `80 46 00 00`
- **Response (R-APDU)**: `[exp_len:2] [exponent] [mod_len:2] [modulus] 90 00`
- **Notes**: Returns 263 bytes for RSA-2048. Since 263 > 256, the engine uses GET RESPONSE chaining for the last 7 bytes (handled by FreeRDP addon). The minidriver uses these to build a `BCRYPT_RSAKEY_BLOB`.

### 4.5 PERFORM SECURITY OPERATION: SIGN DATA (INS 0x2A)
- **Command (C-APDU)**: `80 2A 9E 9A [Lc] [DigestInfo_and_Hash_Bytes] 00`
  - `P1` = `9E`
  - `P2` = `9A` (Sign hash)
  - `Le` = `00` (expect 256 bytes response)
- **Response (R-APDU)**:
  - `[Raw_RSA_Signature_Bytes] 90 00` (256 bytes for RSA-2048)
  - `69 82` = Security Status Not Satisfied (PIN not verified)

### 4.6 PERFORM SECURITY OPERATION: DECRYPT DATA (INS 0x2A)
- **Command (C-APDU)**: `80 2A 80 86 00 01 00 [Encrypted_Key_Bytes: 256] 00 00`
  - `P1` = `80`
  - `P2` = `86` (Decrypt data)
  - **CRITICAL**: Uses **extended APDU case 4** because RSA-2048 ciphertext = 256 bytes, which exceeds the short APDU Lc maximum of 255 bytes. Must include `Le_hi Le_lo` (00 00 = max available) at the end per ISO 7816-4.
- **Response (R-APDU)**:
  - `[Decrypted_Session_Key] 90 00`
  - `69 82` = Security Status Not Satisfied (PIN not verified)

---

## 5. Development Master Plan

Our development is divided into three sequential phases to ensure perfect testing and avoid regressions.

### Phase 1: eUDS Card Emulation (Linux client)
We will implement the eUDS Card Engine in the `uds-client` project:
1. Create `euds_engine.rs` implementing the custom APDUs (SELECT, VERIFY PIN, GET CERT, GET PUBLIC KEY, SIGN, DECRYPT).
2. Register the custom ATR: `3B 89 00 45 55 44 53 2D 43 61 72 64 97`.
3. Read the certificate and private key from the environment variables (same as current setup).
4. **Engine stateful per-connection si PIN required (clave encriptada), stateless si no (clave sin encriptar)**. VERIFY PIN setea flag per-connection; SIGN/DECRYPT chequean flag. Sin PIN required → engine stateless, ops directas.
5. Run unit tests to verify signing/decryption against the local private key.

### Phase 2: eUDS Minidriver (Windows dll)
We will create a clean, dedicated minidriver project: `euds-smartcard-minidriver`.
1. Fork the codebase of `uds-scard-minidriver` into a fresh project.
2. Implement `CardAcquireContext` to support dwVersion 7.
3. Handle `CardGetProperty` for all core properties:
   - `"Card Identifier"` (16-byte random GUID)
    - `"Read Only Mode"` (returns `TRUE` to block writes at Base CSP layer)
    - `"Supports Windows x.509 Enrollment"` (returns `FALSE` since card is read-only)
   - `"PIN Information"` (returns 36 bytes indicating PIN characteristics)
   - `"Authenticated State"` (returns PIN_SET bitmask — **required**)
   - `"Card Serial Number"` (returns same GUID as Card Identifier)
4. Handle `CardSetProperty` for writable properties:
   - `"Cache Mode"`, `"PIN Information"`, `"Parent Window"`, `"PIN Context String"`
5. Handle `CardReadFile`:
   - `"cardcf"` (returns hardcoded 6 bytes)
   - `"cardapps"` (returns `"mscp\0\0\0\0"`)
   - `"mscp\cmapfile"` (returns 86-byte `CONTAINER_MAP_RECORD` with `"eUDS Container 00"`)
   - `"mscp\kxc00"` (queries certificate from card via `GET CERTIFICATE` APDU, returns DER raw — no compression)
5. Handle `CardGetContainerInfo`:
   - Sends `GET PUBLIC KEY` APDU to engine to retrieve modulus + exponent.
   - Builds `BCRYPT_RSAKEY_BLOB` (283 bytes) and returns it in `CONTAINER_INFO.pbKeyExPublicKey`.
6. Handle `CardAuthenticateEx`:
   - Supports `CARD_AUTHENTICATE_GENERATE_SESSION_PIN`, `CARD_AUTHENTICATE_SESSION_PIN`, `CARD_PIN_SILENT_CONTEXT` flags
   - Sends VERIFY PIN APDU (`80 20 00 80 [Lc] [PIN]`)
   - Manages per-session PIN state (verified, retries, blocked, session PIN)
7. Handle `CardDeauthenticateEx`:
   - Accepts `PIN_SET` bitmask, clears state for each PIN in the mask
8. Handle `CardSignData` and `CardRSADecrypt`:
   - Forwards the request over APDU via `SCardTransmit`
   - **DECRYPT uses extended APDU case 4** (`80 2A 80 86 00 01 00 [256 bytes] 00 00`)
9. **Thread-safe `EudsContext`**: All mutable state protected by `RwLock`/`Mutex`
10. **Per-session state**: Each `CardAcquireContext` creates fresh `EudsContext` (no shared PIN state across processes)
11. Register via Calais under the `eUDS Custom Card` ATR.

### Phase 3: Integration & Testing
1. Connect via RDP using `gui-tester`.
2. Windows detects `eUDS Custom Card` → loads `euds_minidriver.dll`.
3. Run `certutil -scinfo`.
    - The CSP queries `"Supports Windows x.509 Enrollment"`, gets `FALSE` (complying with MS spec §7.4 for read-only cards).
    - The CSP queries `"Read Only Mode"`, gets `TRUE`.
   - The CSP reads `mscp\cmapfile`, finds `"eUDS Container 00"`.
   - The CSP calls `CardGetContainerInfo(0)`, gets the public key.
   - The CSP reads the certificate from `mscp\kxc00`.
   - **Result**: `certutil` displays the certificate and container correctly!
4. Test login, signing, or smartcard-based authorization.
