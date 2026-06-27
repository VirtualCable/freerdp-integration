# eUDS Technical Specification

> **Status**: DRAFT — Pending review before implementation
> **Version**: 0.1
> **Last updated**: 2026-06-27

---

## Table of Contents

1. [ATR Design](#1-atr-design)
2. [Custom APDU Protocol](#2-custom-apdu-protocol)
3. [Virtual File System](#3-virtual-file-system)
4. [Minidriver API — Required Functions](#4-minidriver-api--required-functions)
5. [CardGetProperty — Complete Property Table](#5-cardgetproperty--complete-property-table)
6. [Complete Enumeration Flow](#6-complete-enumeration-flow)
7. [Cryptographic Operations](#7-cryptographic-operations)
8. [Buffer Constraints & Edge Cases](#8-buffer-constraints--edge-cases)
9. [Memory Management Rules](#9-memory-management-rules)
10. [Calais Registry Configuration](#10-calais-registry-configuration)
11. [Open Questions & Review Notes](#11-open-questions--review-notes)

---

## 1. ATR Design

### 1.1 Problem with Previous ATR

The previously planned ATR `3B F7 18 00 00 80 31 FE 45 55 44 53 2D 43 61 72 64 C4` was **malformed**:

- `T0 = F7` → Y1=F (TA1, TB1, TC1, TD1 all present), K=7 (declares 7 historical bytes)
- But the interface byte chain consumes more bytes than expected:
  - `TD1 = 0x80` → declares TD2
  - `TD2 = 0x31` → declares TA3, TB3
  - `TA3 = 0xFE`, `TB3 = 0x45` → consumed as interface bytes, NOT historical
- Actual historical bytes = only 7 bytes (`55 44 53 2D 43 61 72` = `"UDS-Car"`), not 9
- The string `"eUDS-Card"` (9 chars) does not fit in K=7

### 1.2 Corrected ATR

```
3B 89 00 45 55 44 53 2D 43 61 72 64 97
```

| Byte | Value | Meaning |
|------|-------|---------|
| TS | `3B` | Direct convention |
| T0 | `89` | Y1=8 (only TD1 present), K=9 (9 historical bytes) |
| TD1 | `00` | Y2=0 (no more interface bytes), T=0 (protocol T=0) |
| H1 | `45` | `'e'` |
| H2 | `55` | `'U'` |
| H3 | `44` | `'D'` |
| H4 | `53` | `'S'` |
| H5 | `2D` | `'-'` |
| H6 | `43` | `'C'` |
| H7 | `61` | `'a'` |
| H8 | `72` | `'r'` |
| H9 | `64` | `'d'` |
| TCK | `97` | Checksum (XOR of T0..H9) |

**Total length**: 13 bytes (well within 36-byte RDP limit)

### 1.3 TCK Verification

```
XOR chain: 89 ^ 00 ^ 45 ^ 55 ^ 44 ^ 53 ^ 2D ^ 43 ^ 61 ^ 72 ^ 64 = 97
Verify:    97 ^ 97 = 00 ✓
```

### 1.4 ATRMask

Exact match (all FF):
```
FF FF FF FF FF FF FF FF FF FF FF FF FF
```

### 1.5 Why T=0?

- The FreeRDP addon already handles GET RESPONSE chaining (`61 XX`) automatically
- The emulated engine processes APDUs at the byte level (no T=0/T=1 framing)
- T=0 is the most widely supported protocol in minidriver implementations
- Simpler than T=1 for our use case

---

## 2. Custom APDU Protocol

We use CLA `0x80` (proprietary) for all custom commands to avoid collisions with ISO 7816-4 standard classes.

### 2.1 Command Summary

| # | Command | CLA | INS | P1 | P2 | Data In | Data Out |
|---|---------|-----|-----|----|----|---------|----------|
| 1 | SELECT Applet | `00` | `A4` | `04` | `00` | AID (9 bytes) | — |
| 2 | VERIFY PIN | `80` | `20` | `00` | `80` | PIN (ASCII) | — |
| 3 | GET CERTIFICATE | `80` | `B0` | `00` | `00` | — | DER cert |
| 4 | GET PUBLIC KEY | `80` | `46` | `00` | `00` | — | exp + mod |
| 5 | SIGN DATA | `80` | `2A` | `9E` | `9A` | DigestInfo+Hash | Signature |
| 6 | DECRYPT DATA | `80` | `2A` | `80` | `86` | Ciphertext | Plaintext |

### 2.2 Command Details

#### 2.2.1 SELECT Applet

Selects the eUDS application on the card.

```
C-APDU: 00 A4 04 00 09 45 55 44 53 2D 43 61 72 64
        │  │  │  │  │  └──── AID = "eUDS-Card" (ASCII) ────┘
        │  │  │  │  └── Lc = 9
        │  │  │  └── P2 = 00 (first or only occurrence)
        │  │  └── P1 = 04 (select by AID)
        │  └── INS = A4 (SELECT)
        └── CLA = 00 (ISO interindustry)

R-APDU: 90 00
```

**Notes**:
- Uses standard CLA=00 (not proprietary) because SELECT is an ISO command
- The AID matches the ATR historical bytes for consistency
- Must be sent first before any other command

#### 2.2.2 VERIFY PIN

Verifies the user PIN against the card's stored PIN.

```
C-APDU: 80 20 00 80 [Lc] [PIN_bytes]
        │  │  │  │  │   └── PIN in ASCII (4-8 bytes typically)
        │  │  │  │  └── Lc = length of PIN
        │  │  │  └── P2 = 80 (verify user PIN)
        │  │  └── P1 = 00
        │  └── INS = 20 (VERIFY)
        └── CLA = 80 (proprietary)

R-APDU (success):    90 00
R-APDU (wrong PIN):  63 CX    (X = retries remaining, e.g. 63 C2 = 2 left)
R-APDU (blocked):    69 83    (authentication method blocked)
R-APDU (not ready):  69 85    (conditions of use not satisfied)
```

**Notes**:
- PIN is sent as plain ASCII bytes (matching Base CSP behavior)
- Engine maintains retry counter (max 3 attempts)
- After 3 failed attempts, PIN is blocked (69 83)
- PIN verification is required before SIGN or DECRYPT operations

#### 2.2.3 GET CERTIFICATE

Retrieves the X.509 certificate in DER format. Used by the minidriver to serve `mscp\kxc00`.

```
C-APDU: 80 B0 00 00 00 [len_hi] [len_lo]
        │  │  │  │  │   │      └── Le_lo
        │  │  │  │  │   └── Le_hi (00 00 = return all available)
        │  │  │  │  └── Extended length indicator
        │  │  │  └── P2 = 00 (offset low)
        │  │  └── P1 = 00 (offset high)
        │  └── INS = B0 (READ BINARY)
        └── CLA = 80 (proprietary)

R-APDU: [DER_bytes] 90 00
```

**Notes**:
- Uses **extended APDU** format (Lc/Le are 2 bytes each)
- Returns the complete DER-encoded X.509 certificate
- Typical certificate size: 1,000–3,000 bytes (fits in extended APDU)
- If response > 65,535 bytes (impossible for X.509), engine would use chaining
- The minidriver caches the certificate after first retrieval
- **No compression**: the DER is returned raw (see Section 3.4 for rationale)

**Fallback for engines that don't support extended APDU**:
```
C-APDU: 80 B0 00 00 00    (Le=00 in short format = 256 bytes)
R-APDU: [256 bytes] 61 XX  (XX = remaining bytes, engine uses GET RESPONSE chaining)
```
The FreeRDP addon handles `61 XX` chaining automatically.

#### 2.2.4 GET PUBLIC KEY

Retrieves the RSA public key components (exponent + modulus) extracted from the certificate.

```
C-APDU: 80 46 00 00
        │  │  │  └── P2 = 00
        │  │  └── P1 = 00
        │  └── INS = 46 (proprietary: GET PUBLIC KEY)
        └── CLA = 80 (proprietary)

R-APDU: [exp_len_hi] [exp_len_lo] [exponent] [mod_len_hi] [mod_len_lo] [modulus] 90 00
```

**Response format**:
```
Offset  Size    Field           Example (RSA-2048)
──────  ────    ─────           ──────────────────
0       2       exp_len         00 03
2       var     exponent        01 00 01
2+N     2       mod_len         01 00
4+N     var     modulus         [256 bytes, big-endian]
```

**Notes**:
- The engine extracts these from the loaded certificate at startup
- Total response for RSA-2048: 2 + 3 + 2 + 256 = **263 bytes** + SW
- Since 263 > 256, the engine uses GET RESPONSE chaining for the last 7 bytes
- The FreeRDP addon handles this transparently
- The minidriver uses these components to build `BCRYPT_RSAKEY_BLOB` for `CardGetContainerInfo`
- Cached after first retrieval

#### 2.2.5 SIGN DATA

Performs RSA PKCS#1 v1.5 signature on a DigestInfo structure.

```
C-APDU: 80 2A 9E 9A [Lc] [DigestInfo_and_Hash]
        │  │  │  │  │   └── DigestInfo + Hash bytes
        │  │  │  │  └── Lc = length of data
        │  │  │  └── P2 = 9A (sign hash)
        │  │  └── P1 = 9E (CRT = sign with private key)
        │  └── INS = 2A (PERFORM SECURITY OPERATION)
        └── CLA = 80 (proprietary)

R-APDU: [signature: 256 bytes] 90 00
```

**Input format** (DigestInfo for SHA-256, 51 bytes):
```
30 31 30 0D 06 09 60 86 48 01 65 03 04 02 01 05 00 04 20 [32 bytes hash]
```

**Input format** (DigestInfo for SHA-1, 35 bytes):
```
30 21 30 09 06 05 2B 0E 03 02 1A 05 00 04 14 [20 bytes hash]
```

**Notes**:
- PIN must be verified first (else `69 82` security status not satisfied)
- Input fits in short APDU (max DigestInfo = ~51 bytes for SHA-256, well under Lc=255)
- Output is always 256 bytes for RSA-2048 (exactly fits Le=256 in short APDU)
- The engine performs: PKCS#1 v1.5 padding → raw RSA (`m^d mod n`)
- Supports SHA-1, SHA-256, SHA-384, SHA-512 (DigestInfo determines the hash)

#### 2.2.6 DECRYPT DATA

Performs RSA decryption (PKCS#1 v1.5 or OAEP).

```
C-APDU: 80 2A 80 86 00 01 00 [ciphertext: 256 bytes]
        │  │  │  │  │  │  │  └── Encrypted data (RSA block)
        │  │  │  │  │  │  └── Lc_lo = 00
        │  │  │  │  │  └── Lc_hi = 01 (Lc = 256)
        │  │  │  │  └── Extended length indicator
        │  │  │  └── P2 = 86 (decrypt)
        │  │  └── P1 = 80 (confidentiality)
        │  └── INS = 2A (PERFORM SECURITY_OPERATION)
        └── CLA = 80 (proprietary)

R-APDU: [plaintext] 90 00
```

**CRITICAL**: This command **requires extended APDU** because:
- RSA-2048 ciphertext = 256 bytes
- Short APDU Lc maximum = 255 bytes (1 byte)
- 256 > 255 → short APDU cannot carry the input

**Notes**:
- PIN must be verified first (else `69 82`)
- Engine must support extended APDU parsing (Lc as 2-byte field)
- The FreeRDP addon passes extended APDUs transparently (no interpretation)
- Output size depends on padding:
  - PKCS#1 v1.5: up to 245 bytes (256 - 11 overhead)
  - OAEP (SHA-1): up to 214 bytes (256 - 42 overhead)
- The minidriver must handle the padding format requested by the Base CSP

### 2.3 APDU Size Constraints Summary

| Command | Input Size | Output Size | APDU Format |
|---------|-----------|-------------|-------------|
| SELECT | 9 bytes | 2 (SW) | Short |
| VERIFY PIN | 4-8 bytes | 2 (SW) | Short |
| GET CERTIFICATE | 0 bytes | ~1-3 KB | **Extended** |
| GET PUBLIC KEY | 0 bytes | 263 bytes | Short (with chaining) |
| SIGN DATA | 35-67 bytes | 256 bytes | Short |
| DECRYPT DATA | **256 bytes** | ~214-245 bytes | **Extended** |

---

## 3. Virtual File System

The minidriver serves these files directly from memory. The card engine does NOT implement a filesystem.

### 3.1 File: `cardid` (root directory)

**Purpose**: Unique card identifier. Must match `CardGetProperty("Card Identifier")`.

**Content**: 16-byte binary GUID (static, generated once at minidriver startup).

```
Example: A1 B2 C3 D4 E5 F6 78 90 AB CD EF 01 23 45 67 89
```

**Access condition**: `EveryoneReadUserWriteAc` (E=R, U=R, A=RW)

**Notes**:
- The GUID is generated randomly when the minidriver first initializes
- It is cached in `pvVendorSpecific` for the lifetime of the context
- Both `CardReadFile(NULL, "cardid")` and `CardGetProperty("Card Identifier")` must return the same value

### 3.2 File: `cardcf` (root directory)

**Purpose**: Cache freshness counters. When counters change, the Base CSP invalidates its Windows-side cache.

**Content**: 6 bytes (`CARD_CACHE_FILE_FORMAT`)

```
Byte 0: bVersion             = 0x01
Byte 1: bPinsFreshness       = 0x01  (incremented after each PIN verify/deauth)
Byte 2-3: wContainersFreshness = 0x0001 (LE, incremented when containers change)
Byte 4-5: wFilesFreshness    = 0x0001 (LE, incremented when files change)
```

**Hex**: `01 01 01 00 01 00`

**Access condition**: `EveryoneReadUserWriteAc`

**Notes**:
- `bPinsFreshness` should increment when PIN state changes (verify/deauthenticate)
- For our read-only card, containers and files never change, so their counters stay constant
- The Base CSP uses these to decide whether to re-read cached data

### 3.3 File: `cardapps` (root directory)

**Purpose**: Lists the applications (CSP directories) on the card.

**Content**: 8 bytes

```
6D 73 63 70 00 00 00 00
m  s  c  p  \0 \0 \0 \0
```

**Access condition**: `EveryoneReadUserWriteAc`

**Notes**:
- We only register `mscp` (Microsoft CSP directory)
- Format: 8-byte record per app (name + zero padding)

### 3.4 File: `mscp\cmapfile` (mscp directory)

**Purpose**: Container map. Maps container indices to names and key sizes.

**Content**: Single `CONTAINER_MAP_RECORD` (86 bytes)

```c
typedef struct _CONTAINER_MAP_RECORD {
    WCHAR wszGuid[40];     // Container name (null-padded UTF-16)
    BYTE  bFlags;          // 0x03 = VALID | DEFAULT
    BYTE  bReserved;       // 0x00
    WORD  wSigKeySizeBits; // 0 (no signature key)
    WORD  wKeyExchangeKeySizeBits; // 2048
} CONTAINER_MAP_RECORD;    // Total: 86 bytes
```

**Hex layout**:
```
Offset  Size  Value                          Meaning
──────  ────  ─────                          ───────
00-79   80    "eUDS Container 00\0..."       Container name (UTF-16LE, null-padded)
80      1     03                             VALID (0x01) | DEFAULT (0x02)
81      1     00                             Reserved
82-83   2     00 00                          Sig key size = 0 (no sig key)
84-85   2     00 08                          Key exchange key size = 2048 (LE)
```

**Access condition**: `EveryoneReadUserWriteAc`

**Notes**:
- Single container design (index 0)
- `wszGuid` is null-terminated UTF-16LE, padded to 40 WCHAR (80 bytes)
- Name `"eUDS Container 00"` = 17 chars + null = 18 WCHAR = 36 bytes, remaining 44 bytes are zero
- `wSigKeySizeBits = 0` tells the CSP there is no separate signature key
- `wKeyExchangeKeySizeBits = 2048` tells the CSP the key exchange key is RSA-2048

### 3.5 File: `mscp\kxc00` (mscp directory)

**Purpose**: Key exchange certificate for container 0.

**Content**: Raw DER-encoded X.509 certificate (no compression).

**Obtained by**: Minidriver sends `GET CERTIFICATE` APDU to the engine, caches the result.

**Access condition**: `EveryoneReadUserWriteAc`

**Notes**:
- **No zlib compression**: `CARD_CAPABILITIES.fCertificateCompression = FALSE`
- This means the Base CSP reads the DER directly without decompression
- Rationale for no compression:
  - Simpler implementation (no zlib dependency in minidriver)
  - Typical certificate size (1-3 KB) fits easily in RDP buffers (max 66 KB)
  - Fewer failure modes
  - The compression savings are negligible for our use case

### 3.6 Directory: `mscp`

**Access condition**: `UserCreateDeleteDirAc` (User+Admin can create files, Everyone can list)

**Contents**: `cmapfile`, `kxc00`

### 3.7 File Listing Summary

| Directory | File | Size | Content |
|-----------|------|------|---------|
| root (NULL) | `cardid` | 16 bytes | Static GUID |
| root (NULL) | `cardcf` | 6 bytes | Cache freshness counters |
| root (NULL) | `cardapps` | 8 bytes | `"mscp\0\0\0\0"` |
| mscp | `cmapfile` | 86 bytes | Container map (1 record) |
| mscp | `kxc00` | ~1-3 KB | DER certificate (from engine) |

---

## 4. Minidriver API — Required Functions

### 4.1 Functions to Implement

| Function | Behavior | Return |
|----------|----------|--------|
| `CardAcquireContext` | Version negotiation, init context, set all function pointers | 0 (success) |
| `CardDeleteContext` | Free `pvVendorSpecific`, cleanup | 0 |
| `CardGetProperty` | Return properties (see Section 5) | 0 |
| `CardSetProperty` | Accept silently or return UNSUPPORTED for read-only props | 0 |
| `CardReadFile` | Serve virtual files (see Section 3) | 0 |
| `CardEnumFiles` | Return multi-string of filenames per directory | 0 |
| `CardGetFileInfo` | Return file size and access condition | 0 |
| `CardGetContainerInfo` | Return `CONTAINER_INFO` with `BCRYPT_RSAKEY_BLOB` | 0 |
| `CardGetContainerProperty` | Handle `CCP_PIN_IDENTIFIER` → PinId 0 | 0 |
| `CardAuthenticateEx` | Send VERIFY PIN APDU to engine | 0 |
| `CardDeauthenticateEx` | Clear PIN state in engine | 0 |
| `CardSignData` | Send SIGN APDU to engine | 0 |
| `CardRSADecrypt` | Send DECRYPT APDU to engine | 0 |
| `CardQueryKeySizes` | Delegate to CardGetProperty("Key Sizes") | 0 |
| `CardQueryCapabilities` | Delegate to CardGetProperty("Capabilities") | 0 |
| `CardQueryFreeSpace` | Delegate to CardGetProperty("Free Space") | 0 |

### 4.2 Functions Returning SCARD_E_UNSUPPORTED_FEATURE

| Function | Reason |
|----------|--------|
| `CardCreateDirectory` | Read-only card |
| `CardDeleteDirectory` | Read-only card |
| `CardCreateFile` | Read-only card |
| `CardWriteFile` | Read-only card |
| `CardDeleteFile` | Read-only card |
| `CardCreateContainer` | Read-only card, keys pre-provisioned |
| `CardCreateContainerEx` | Read-only card |
| `CardDeleteContainer` | Read-only card |
| `CardSetContainerProperty` | Read-only card |
| `CardChangeAuthenticatorEx` | PIN change not supported |
| `CardGetChallenge` | Challenge-response not supported |
| `CardAuthenticateChallenge` | Challenge-response not supported |
| `CardGetChallengeEx` | Challenge-response not supported |
| `CardUnblockPin` | PUK not supported |

### 4.3 CardAcquireContext Details

```rust
unsafe extern "system" fn CardAcquireContext(
    pCardData: PCARD_DATA,
    dwFlags: DWORD,
) -> DWORD {
    // 1. Version negotiation
    let requested = (*pCardData).dwVersion;
    if requested < 4 {
        return ERROR_REVISION_MISMATCH; // SCARD_E_INVALID_PARAMETER
    }
    (*pCardData).dwVersion = min(requested, 7);

    // 2. Validate inputs
    if (*pCardData).pbAtr.is_null() || (*pCardData).pwszCardName.is_null() {
        return SCARD_E_INVALID_PARAMETER;
    }
    if (*pCardData).pfnCspAlloc.is_null() || (*pCardData).pfnCspFree.is_null() {
        return SCARD_E_INVALID_PARAMETER;
    }

    // 3. Validate ATR matches our custom ATR
    let atr = slice::from_raw_parts((*pCardData).pbAtr, (*pCardData).cbAtr as usize);
    if atr != EXPECTED_ATR {
        return SCARD_E_UNKNOWN_CARD;
    }

    // 4. Allocate vendor-specific context
    let ctx = Box::new(EudsContext::new());
    (*pCardData).pvVendorSpecific = Box::into_raw(ctx) as PVOID;

    // 5. Set all function pointers
    (*pCardData).pfnCardDeleteContext = Some(CardDeleteContext);
    (*pCardData).pfnCardGetProperty = Some(CardGetProperty);
    (*pCardData).pfnCardSetProperty = Some(CardSetProperty);
    (*pCardData).pfnCardReadFile = Some(CardReadFile);
    (*pCardData).pfnCardEnumFiles = Some(CardEnumFiles);
    (*pCardData).pfnCardGetFileInfo = Some(CardGetFileInfo);
    (*pCardData).pfnCardGetContainerInfo = Some(CardGetContainerInfo);
    (*pCardData).pfnCardGetContainerProperty = Some(CardGetContainerProperty);
    (*pCardData).pfnCardAuthenticateEx = Some(CardAuthenticateEx);
    (*pCardData).pfnCardDeauthenticateEx = Some(CardDeauthenticateEx);
    (*pCardData).pfnCardSignData = Some(CardSignData);
    (*pCardData).pfnCardRSADecrypt = Some(CardRSADecrypt);
    (*pCardData).pfnCardQueryKeySizes = Some(CardQueryKeySizes);
    (*pCardData).pfnCardQueryCapabilities = Some(CardQueryCapabilities);
    (*pCardData).pfnCardQueryFreeSpace = Some(CardQueryFreeSpace);
    // ... unsupported functions set to return SCARD_E_UNSUPPORTED_FEATURE

    0 // SCARD_S_SUCCESS
}
```

**CRITICAL**: `pvVendorSpecific` MUST be non-NULL after return. The Base CSP checks this.

---

## 5. CardGetProperty — Complete Property Table

### 5.1 Required Properties

| Property Name | Type | Size | Value | Notes |
|---------------|------|------|-------|-------|
| `"Card Identifier"` | BYTE[16] | 16 | Static GUID | Must match `cardid` file |
| `"Read Only Mode"` | BOOL | 4 | `0x00000000` (FALSE) | Allows enumeration |
| `"Supports Windows x.509 Enrollment"` | BOOL | 4 | `0x00000001` (TRUE) | **CRITICAL**: enables cert enumeration |
| `"Capabilities"` | CARD_CAPABILITIES | 12 | See below | |
| `"Key Sizes"` | CARD_KEY_SIZES | 20 | See below | dwFlags = AT_KEYEXCHANGE |
| `"Free Space"` | CARD_FREE_SPACE_INFO | 12 | See below | |
| `"Cache Mode"` | DWORD | 4 | `0x00000002` (SESSION_ONLY) | |
| `"PIN Information"` | PIN_INFO | 36 | See below | dwFlags = PinId |
| `"PIN List"` | PIN_SET | 4 | `0x00000001` | PIN 0 active |

### 5.2 Property Values in Detail

#### Capabilities
```rust
CARD_CAPABILITIES {
    dwVersion: 1,
    fCertificateCompression: FALSE,  // We serve DER raw
    fKeyGen: FALSE,                  // No on-card key generation
}
// Size: 12 bytes
```

#### Key Sizes (for AT_KEYEXCHANGE = 2)
```rust
CARD_KEY_SIZES {
    dwVersion: 1,
    dwMinimumBitlen: 2048,
    dwDefaultBitlen: 2048,
    dwMaximumBitlen: 2048,
    dwIncrementalBitlen: 0,  // Only 2048 supported
}
// Size: 20 bytes
```

#### Free Space
```rust
CARD_FREE_SPACE_INFO {
    dwVersion: 1,
    dwBytesAvailable: CARD_DATA_VALUE_UNKNOWN,  // Unknown/not applicable
    dwKeyContainersAvailable: 0,                 // 0 free (1 total, 1 used)
    dwMaxKeyContainers: 1,                       // We support 1 container
}
// Size: 12 bytes
```

#### PIN Information (dwFlags = 0, i.e., PinId 0)
```rust
PIN_INFO {
    dwVersion: 6,
    PinType: AlphaNumericPinType,       // 1
    PinPurpose: AuthenticationPin,      // 1
    dwChangePermission: 0,              // No one can change
    dwUnblockPermission: 0,             // No one can unblock
    PinCachePolicy: PIN_CACHE_POLICY {
        dwVersion: 6,
        PinCachePolicyType: PinCacheNormal,  // 0
        dwPinCachePolicyInfo: 0,
    },
    dwFlags: 0,
}
// Size: 36 bytes
```

### 5.3 Optional Properties (return SCARD_E_UNSUPPORTED_FEATURE if not implemented)

| Property | Recommendation |
|----------|---------------|
| `"Card Serial Number"` | Optional, can return same as Card Identifier |
| `"Authenticated State"` | Optional, return `0x00000001` if PIN verified |
| `"PIN Strength Verify"` | Optional, return `CARD_PIN_STRENGTH_PLAINTEXT` |
| `"Key Import Support"` | Return `0` (no key import) |
| `"Enum Algorithms"` | Optional, not needed for read-only |
| `"Padding Schemes"` | Optional, not needed for basic operations |
| `"Chaining Modes"` | Optional, not needed |

---

## 6. Complete Enumeration Flow

This is the exact sequence of calls the Base CSP makes when `certutil -scinfo` or any application enumerates the smart card.

### 6.1 Phase 1: Card Detection & Driver Loading

```
1. SCardSvr detects card insertion (via RDP redirect)
2. SCardSvr reads ATR: 3B 89 00 45 55 44 53 2D 43 61 72 64 97
3. SCardSvr matches ATR against Calais database
4. Finds "eUDS Custom Card" entry
5. Loads euds_minidriver.dll
6. Calls CardAcquireContext(pCardData, 0)
   → Minidriver: version negotiation, validate ATR, set pvVendorSpecific
   → Returns: SCARD_S_SUCCESS
```

### 6.2 Phase 2: Property Verification

```
7. CardGetProperty("Card Identifier")
   → Returns: 16-byte GUID (e.g., A1 B2 C3 D4 ...)

8. CardReadFile(NULL, "cardcf")
   → Returns: 01 01 01 00 01 00 (6 bytes)

9. CardGetProperty("Read Only Mode")
   → Returns: FALSE (0x00000000)

10. CardGetProperty("Supports Windows x.509 Enrollment")
    → Returns: TRUE (0x00000001)
    ★ CRITICAL: If FALSE or unsupported, CSP stops here!

11. CardGetProperty("Capabilities")
    → Returns: {fCertComp=FALSE, fKeyGen=FALSE}

12. CardGetProperty("Cache Mode")
    → Returns: SESSION_ONLY (2)
```

### 6.3 Phase 3: Container Discovery

```
13. CardReadFile(NULL, "cardapps")
    → Returns: "mscp\0\0\0\0" (8 bytes)

14. CardReadFile("mscp", "cmapfile")
    → Returns: 86-byte CONTAINER_MAP_RECORD
    → CSP finds: container 0, name "eUDS Container 00", flags=0x03

15. CardGetContainerInfo(bContainerIndex=0)
    → Minidriver sends GET PUBLIC KEY APDU to engine
    → Engine returns: [exp_len] [exponent] [mod_len] [modulus]
    → Minidriver builds BCRYPT_RSAKEY_BLOB:
        RSA1 magic (4B) + BitLength=2048 (4B) +
        cbPublicExp=3 (4B) + cbModulus=256 (4B) +
        cbPrime1=0 (4B) + cbPrime2=0 (4B) +
        exponent (3B) + modulus (256B) = 283 bytes
    → Returns CONTAINER_INFO:
        cbSigPublicKey = 0, pbSigPublicKey = NULL
        cbKeyExPublicKey = 283, pbKeyExPublicKey = [BCRYPT_RSAKEY_BLOB]

16. CardReadFile("mscp", "kxc00")
    → Minidriver sends GET CERTIFICATE APDU to engine
    → Engine returns: DER certificate (~1-3 KB)
    → Minidriver allocates buffer with pfnCspAlloc, copies DER
    → Returns: DER bytes
```

### 6.4 Phase 4: PIN Authentication (on demand)

```
17. CardGetProperty("PIN List")
    → Returns: 0x00000001 (PIN 0 active)

18. CardGetProperty("PIN Information", dwFlags=0)
    → Returns: PIN_INFO (36 bytes)

19. User enters PIN in Windows dialog
20. CardAuthenticateEx(PinId=0, dwFlags=0, pbPinData="1234")
    → Minidriver sends VERIFY PIN APDU: 80 20 00 80 04 31 32 33 34
    → Engine verifies PIN
    → Returns: SCARD_S_SUCCESS (or SCARD_E_INVALID_VALUE if wrong)
```

### 6.5 Phase 5: Cryptographic Operations

```
21. CardSignData(pInfo)
    → pInfo contains: bContainerIndex, aiHashAlg, pbData (hash), dwPaddingType
    → Minidriver builds APDU: 80 2A 9E 9A [Lc] [DigestInfo+Hash]
    → Engine performs RSA sign
    → Returns: signature (256 bytes) in pInfo->pbSignedData

22. CardRSADecrypt(pInfo)
    → pInfo contains: bContainerIndex, pbData (ciphertext), dwPaddingType
    → Minidriver builds APDU: 80 2A 80 86 00 01 00 [256 bytes]
    → Engine performs RSA decrypt
    → Returns: plaintext in pInfo->pbData
```

### 6.6 Visual Flow

```
Windows                    Minidriver                    Engine (Linux)
  │                           │                              │
  │──CardAcquireContext──────►│                              │
  │◄──SUCCESS─────────────────│                              │
  │                           │                              │
  │──GetProperty(CardID)─────►│                              │
  │◄──16-byte GUID────────────│                              │
  │                           │                              │
  │──ReadFile(cardcf)────────►│                              │
  │◄──6 bytes─────────────────│                              │
  │                           │                              │
  │──GetProperty(ReadOnly)───►│                              │
  │◄──FALSE───────────────────│                              │
  │                           │                              │
  │──GetProperty(X509Enroll)─►│                              │
  │◄──TRUE────────────────────│  ★ CRITICAL                  │
  │                           │                              │
  │──ReadFile(cardapps)──────►│                              │
  │◄──"mscp\0\0\0\0"─────────│                              │
  │                           │                              │
  │──ReadFile(cmapfile)──────►│                              │
  │◄──86 bytes────────────────│                              │
  │                           │                              │
  │──GetContainerInfo(0)─────►│                              │
  │                           │──GET PUBLIC KEY APDU────────►│
  │                           │◄──exp+mod────────────────────│
  │◄──CONTAINER_INFO──────────│                              │
  │                           │                              │
  │──ReadFile(kxc00)─────────►│                              │
  │                           │──GET CERTIFICATE APDU───────►│
  │                           │◄──DER cert───────────────────│
  │◄──DER cert────────────────│                              │
  │                           │                              │
  │  [certutil shows cert]    │                              │
  │                           │                              │
  │──AuthenticateEx(PIN)─────►│                              │
  │                           │──VERIFY PIN APDU────────────►│
  │                           │◄──90 00──────────────────────│
  │◄──SUCCESS─────────────────│                              │
  │                           │                              │
  │──SignData(hash)──────────►│                              │
  │                           │──SIGN APDU──────────────────►│
  │                           │◄──signature──────────────────│
  │◄──signature───────────────│                              │
```

---

## 7. Cryptographic Operations

### 7.1 RSA Key Parameters

| Parameter | Value |
|-----------|-------|
| Algorithm | RSA |
| Key size | 2048 bits (256 bytes) |
| Public exponent | 65537 (0x010001) |
| Padding (sign) | PKCS#1 v1.5 |
| Padding (decrypt) | PKCS#1 v1.5 or OAEP (per CSP request) |

### 7.2 BCRYPT_RSAKEY_BLOB Format

The public key returned in `CONTAINER_INFO.pbKeyExPublicKey` must be in this exact format:

```
Offset  Size  Field           Value           Description
──────  ────  ─────           ─────           ───────────
0       4     Magic           0x31415352      "RSA1" (public key)
4       4     BitLength       0x00000800      2048 bits
8       4     cbPublicExp     0x00000003      3 bytes
12      4     cbModulus       0x00000100      256 bytes
16      4     cbPrime1        0x00000000      Not available
20      4     cbPrime2        0x00000000      Not available
24      3     Exponent        01 00 01        65537
27      256   Modulus         [256 bytes]     Big-endian, left-padded with zeros
```

**Total size**: 283 bytes

### 7.3 SIGN Operation Flow

```
Base CSP                          Minidriver                      Engine
  │                                  │                              │
  │──CardSignData───────────────────►│                              │
  │  pInfo->aiHashAlg = CALG_SHA_256 │                              │
  │  pInfo->pbData = [32-byte hash]  │                              │
  │  pInfo->dwPaddingType = PKCS1    │                              │
  │                                  │                              │
  │                                  │──Build DigestInfo────────────│
  │                                  │  30 31 30 0D 06 09 ...      │
  │                                  │  + 32-byte hash = 51 bytes   │
  │                                  │                              │
  │                                  │──APDU: 80 2A 9E 9A 33 ─────►│
  │                                  │  [51 bytes DigestInfo+Hash]  │
  │                                  │                              │
  │                                  │                    PKCS#1 pad:│
  │                                  │                    00 01 FF..FF 00 [DI+Hash]│
  │                                  │                    RSA: m^d mod n
  │                                  │                              │
  │                                  │◄──[256 bytes signature]──────│
  │                                  │                              │
  │◄──pInfo->pbSignedData────────────│                              │
  │  [256 bytes, allocated by MD]    │                              │
```

### 7.4 DECRYPT Operation Flow

```
Base CSP                          Minidriver                      Engine
  │                                  │                              │
  │──CardRSADecrypt─────────────────►│                              │
  │  pInfo->pbData = [256 bytes]     │                              │
  │  pInfo->dwPaddingType = PKCS1    │                              │
  │                                  │                              │
  │                                  │──APDU: 80 2A 80 86 ────────►│
  │                                  │  00 01 00 [256 bytes]        │
  │                                  │  (extended APDU)              │
  │                                  │                              │
  │                                  │                    RSA: c^d mod n
  │                                  │                    PKCS#1 unpad
  │                                  │                              │
  │                                  │◄──[plaintext]────────────────│
  │                                  │                              │
  │◄──pInfo->pbData──────────────────│                              │
  │  [decrypted data]                │                              │
```

---

## 8. Buffer Constraints & Edge Cases

### 8.1 RDP Channel Limits (MS-RDPESC)

| Parameter | Limit | Our Usage | Safe? |
|-----------|-------|-----------|-------|
| APDU send buffer | 66,560 bytes | max 256 bytes (DECRYPT) | ✓ |
| APDU receive buffer | 66,560 bytes | max ~3 KB (certificate) | ✓ |
| ATR length | 36 bytes | 13 bytes | ✓ |
| Reader name | 65,536 bytes | ~20 bytes | ✓ |

### 8.2 APDU Size Analysis

| Command | Send Size | Recv Size | Format | Issue? |
|---------|-----------|-----------|--------|--------|
| SELECT | 14 bytes | 2 bytes | Short | ✓ |
| VERIFY PIN | ~10 bytes | 2 bytes | Short | ✓ |
| GET CERTIFICATE | 7 bytes | ~2 KB | Extended | ✓ |
| GET PUBLIC KEY | 4 bytes | 263 bytes | Short + chaining | ✓ |
| SIGN DATA | ~57 bytes | 256 bytes | Short | ✓ |
| DECRYPT DATA | 263 bytes | ~245 bytes | **Extended** | ✓ |

### 8.3 Edge Cases

| Case | Handling |
|------|----------|
| Card removed during operation | Engine returns `6E 00` (CLA not supported) or connection error |
| PIN blocked (3 failed attempts) | Engine returns `69 83`, minidriver returns `SCARD_W_CHV_BLOCKED` |
| Certificate too large for single APDU | Use GET RESPONSE chaining (handled by FreeRDP addon) |
| SIGN without PIN verified | Engine returns `69 82`, minidriver returns `SCARD_W_CARD_NOT_AUTHENTICATED` |
| DECRYPT without PIN verified | Engine returns `69 82`, minidriver returns `SCARD_W_CARD_NOT_AUTHENTICATED` |
| Invalid container index | Minidriver returns `SCARD_E_NO_KEY_CONTAINER` |
| CardGetProperty with unknown property | Return `SCARD_E_UNSUPPORTED_FEATURE` |
| CardReadFile with unknown file | Return `SCARD_E_FILE_NOT_FOUND` |

### 8.4 GET PUBLIC KEY Response > 256 bytes

The GET PUBLIC KEY response is 263 bytes (2 + 3 + 2 + 256). Since short APDU Le max = 256:

```
Engine sends first 256 bytes + SW 61 07
FreeRDP addon sends: 00 C0 00 00 07 (GET RESPONSE)
Engine sends remaining 7 bytes + SW 90 00
FreeRDP addon concatenates → minidriver receives 263 bytes
```

This is handled automatically by the FreeRDP addon's GET RESPONSE chaining logic (`handlers.rs:557-580`).

---

## 9. Memory Management Rules

### 9.1 Allocation

The minidriver **MUST** use `pCardData->pfnCspAlloc` for all buffers returned to the Base CSP:

```rust
// CORRECT:
let buffer = unsafe { ((*pCardData).pfnCspAlloc)(size) };
ptr::copy_nonoverlapping(data.as_ptr(), buffer, size);
*ppbData = buffer;
*pcbData = size;

// WRONG (will crash):
let buffer = vec![0u8; size];
*ppbData = buffer.as_mut_ptr();  // ← freed by Rust, CSP tries to free again
```

### 9.2 Deallocation

The Base CSP frees returned buffers using `pCardData->pfnCspFree`. The minidriver must NOT free these buffers itself.

### 9.3 Vendor-Specific Context

```rust
struct EudsContext {
    card_id: [u8; 16],           // Static GUID
    cert_der: Option<Vec<u8>>,   // Cached certificate (lazy loaded)
    pub_key_blob: Option<Vec<u8>>, // Cached BCRYPT_RSAKEY_BLOB (lazy loaded)
    pin_verified: bool,
    pin_freshness: u8,           // Incremented on verify/deauth
}
```

Allocated in `CardAcquireContext` with `Box::new()`, freed in `CardDeleteContext` with `Box::from_raw()`.

### 9.4 CardReadFile Allocation Pattern

```rust
unsafe extern "system" fn CardReadFile(
    pCardData: PCARD_DATA,
    pszDirectoryName: LPSTR,
    pszFileName: LPSTR,
    _dwFlags: DWORD,
    ppbData: *mut PBYTE,
    pcbData: PDWORD,
) -> DWORD {
    let data = match get_file_data(pCardData, pszDirectoryName, pszFileName) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let size = data.len();
    let buffer = ((*pCardData).pfnCspAlloc)(size);
    if buffer.is_null() {
        return SCARD_E_NO_MEMORY;
    }
    ptr::copy_nonoverlapping(data.as_ptr(), buffer, size);
    *ppbData = buffer;
    *pcbData = size as DWORD;
    0
}
```

---

## 10. Calais Registry Configuration

### 10.1 Registry Entries

```registry
[HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\eUDS Custom Card]
"ATR"=hex:3B,89,00,45,55,44,53,2D,43,61,72,64,97
"ATRMask"=hex:FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF
"Crypto Provider"="Microsoft Base Smart Card Crypto Provider"
"Smart Card Key Storage Provider"="Microsoft Smart Card Key Storage Provider"
"80000001"="C:\\temp\\euds_minidriver.dll"
```

### 10.2 WoW64 (32-bit processes on 64-bit Windows)

```registry
[HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\Cryptography\Calais\SmartCards\eUDS Custom Card]
"ATR"=hex:3B,89,00,45,55,44,53,2D,43,61,72,64,97
"ATRMask"=hex:FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF
"Crypto Provider"="Microsoft Base Smart Card Crypto Provider"
"Smart Card Key Storage Provider"="Microsoft Smart Card Key Storage Provider"
"80000001"="C:\\temp\\euds_minidriver_x86.dll"
```

### 10.3 Registration Script

```powershell
# Register eUDS Custom Card minidriver
$atrPath = "HKLM:\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\eUDS Custom Card"
New-Item -Path $atrPath -Force
Set-ItemProperty -Path $atrPath -Name "ATR" -Value ([byte[]](0x3B,0x89,0x00,0x45,0x55,0x44,0x53,0x2D,0x43,0x61,0x72,0x64,0x97))
Set-ItemProperty -Path $atrPath -Name "ATRMask" -Value ([byte[]](0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF))
Set-ItemProperty -Path $atrPath -Name "Crypto Provider" -Value "Microsoft Base Smart Card Crypto Provider"
Set-ItemProperty -Path $atrPath -Name "Smart Card Key Storage Provider" -Value "Microsoft Smart Card Key Storage Provider"
Set-ItemProperty -Path $atrPath -Name "80000001" -Value "C:\temp\euds_minidriver.dll"

# Restart Smart Card service to pick up changes
Restart-Service SCardSvr
```

---

## 11. Open Questions & Review Notes

### 11.1 Items Requiring Validation

1. **Extended APDU support in FreeRDP addon**: The addon passes APDUs transparently, but we should verify that the MS-RDPESC encoding handles extended APDUs correctly. The `cbSendLength` max is 66,560 bytes, so 263 bytes is well within limits.

2. **BCRYPT_RSAKEY_BLOB format**: We assume the Base CSP expects this exact format for `CONTAINER_INFO.pbKeyExPublicKey`. This should be verified against the actual Windows Base CSP behavior (e.g., by testing with a known working minidriver).

3. **Certificate format in kxc00**: We chose DER raw (no compression). If the Base CSP expects compressed format regardless of `fCertificateCompression`, we would need to adjust. This should be tested.

4. **Container name encoding**: The `wszGuid` field in `CONTAINER_MAP_RECORD` is UTF-16LE. We use `"eUDS Container 00"` — this should be verified to not cause issues with any Windows component that expects a GUID format.

5. **PIN freshness counter**: We increment `bPinsFreshness` on verify/deauth. The Base CSP uses this to invalidate its PIN cache. We should verify this works correctly with the Windows PIN dialog.

### 11.2 Design Decisions Made

| Decision | Choice | Rationale |
|----------|--------|-----------|
| ATR protocol | T=0 | Simplest, FreeRDP addon handles chaining |
| Certificate compression | None (DER raw) | Simpler, negligible size difference |
| Public key retrieval | Dedicated APDU | Avoids ASN.1 parser in minidriver |
| Container count | 1 (single key) | Simplest, matches our use case |
| Key type | Key exchange only | No separate signature key needed |
| Cache mode | SESSION_ONLY | Avoids persistent cache issues |
| PIN encoding | ASCII | Matches Base CSP behavior |

### 11.3 Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Extended APDU not supported by some Windows component | DECRYPT fails | Fallback to command chaining |
| BCRYPT_RSAKEY_BLOB format incorrect | certutil shows wrong key | Verify with known working minidriver |
| Container name not GUID format | Some apps may reject | Test with certutil first |
| ATR T=0 causes issues with some Windows versions | Card not recognized | Fallback to T=1 ATR variant |
