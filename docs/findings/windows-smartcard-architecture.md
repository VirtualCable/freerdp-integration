# Windows Smart Card Subsystem & Base CSP/KSP Architecture

This document analyzes how Windows interacts with smart cards and custom minidrivers, documenting the exact sequence of events from card insertion to container enumeration and cryptographic operations.

---

## 1. Overview of the Windows Smart Card Architecture

The Windows smart card architecture is split into three main layers:

```
                  ┌─────────────────────────────────────────┐
                  │      Applications (certutil, etc.)      │
                  └────────────────────┬────────────────────┘
                                       │ CryptoAPI / CNG
                  ┌────────────────────▼────────────────────┐
                  │ Base CSP (legacy) / Smart Card KSP (CNG)│
                  └────────────────────┬────────────────────┘
                                       │ Card Module API (V7+)
                  ┌────────────────────▼────────────────────┐
                  │      Card Minidriver (Custom DLL)       │
                  └────────────────────┬────────────────────┘
                                       │ SCard API (PC/SC)
                  ┌────────────────────▼────────────────────┐
                  │       Smart Card Service (SCardSvr)     │
                  └────────────────────┬────────────────────┘
                                       │ RDP Redirect / CCID
                  ┌────────────────────▼────────────────────┐
                  │      eUDS Virtual Card / Physical Card   │
                  └─────────────────────────────────────────┘
```

### 1.1 The Role of SCardSvr (Smart Card Service)
- Manages reader states (`SCardGetStatusChangeW`, `SCardEstablishContext`).
- Detects card insertions and queries the ATR (Answer To Reset).
- Matches the ATR against registered entries in the **Calais Database** (`HKLM\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards`).
- Exposes the raw transceive interface (`SCardTransmit`) to send APDUs.

### 1.2 The Role of Base CSP / Smart Card KSP
- Implements the high-level CryptoAPI and CNG interfaces (certificate lookup, key generation, signing, decryption).
- Acts as an orchestrator. It does not know how to talk APDU; instead, it loads the **Card Minidriver** registered for the card's ATR.
- Connects to the card's minidriver via the **Card Module API** (by calling `CardAcquireContext` exported by the minidriver DLL).

### 1.3 The Role of the Card Minidriver
- Implements the Card Module API (`cardmod.h`).
- Translates logical requests from the Base CSP (e.g., "Read cmapfile", "Sign this hash", "Get container info") into card-specific formats or directly answers them.
- Serves as the key to customizing smart card behavior on Windows without rewriting a full CSP.

---

## 2. Card Detection & Driver Matching (The Calais Layer)

When SCardSvr detects an ATR, it searches the Windows Registry to determine which minidriver to load.

### 2.1 The Registry Keys
```registry
[HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\eUDS Custom Card]
"ATR"=hex:3B,89,01,45,55,44,53,2D,43,61,72,64,96
"ATRMask"=hex:FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF,FF
"Crypto Provider"="Microsoft Base Smart Card Crypto Provider"
"Smart Card Key Storage Provider"="Microsoft Smart Card Key Storage Provider"
"80000001"="C:\\temp\\euds_minidriver.dll"
```

- **`80000001`**: Under 64-bit Windows, this maps directly to the minidriver DLL path. (In 32-bit WoW64 processes, it reads from the corresponding `WOW6432Node` subkey).
- **ATR/ATRMask**: SCardSvr performs a bitwise AND match: `Inserted_ATR & ATRMask == ATR & ATRMask`. If a match is found, Windows couples the reader and card with this specific provider.

### 2.2 Deriving the Device ID (PnP)
Windows Plug and Play (PnP) reads the ATR historical bytes to identify the card. 
- **CRITICAL REQUIREMENT**: If the ATR does not contain historical bytes (meaning `T0` lower nibble is `0`), PnP cannot derive a unique device ID and may fail with `SCARD_E_UNEXPECTED`, leaving the reader in an "insert card" state.
- **eUDS Custom Card ATR Choice**:
  ```
  3B 89 01 45 55 44 53 2D 43 61 72 64 96
  ```
  - `3B` = Direct Convention.
  - `89` = Only TD1 present (Y1=8), **9 historical bytes** (K=9).
  - `TD1 = 00` = T=0 protocol, no more interface bytes.
  - Historical bytes: `45 55 44 53 2D 43 61 72 64` = ASCII `"eUDS-Card"` (9 bytes).
  - `TCK = 97` = XOR checksum (verified: XOR of T0..H9..TCK = 00).
  - Total length: 13 bytes.
  - This guarantees perfect PnP detection on Windows.

---

## 3. The Core Issue with the Inbox Minidriver (msclmd)

Previously, we tried to build our card as GIDS-compatible and let Microsoft's inbox minidriver (`msclmd.dll`) manage it. This failed because `msclmd` is a highly complex, opaque state machine:

1. **PiV vs. GIDS Heuristic**: `msclmd` probes the card with `SELECT PIV AID` and `SELECT GIDS AID`. If either succeeds or fails in unexpected ways, it shifts modes silently.
2. **In-Memory Cache Interference**: `msclmd` relies aggressively on SCard level cache (`SCardReadCacheW`, `SCardWriteCacheW`) to avoid sending APDUs. Once a cache mismatch or stale state occurs, it locks up and stops querying the card entirely, leaving `certutil` showing empty containers.
3. **Strict Validation**: GIDS demands a complete, pre-configured file structure (`cardcf`, `cardapps`, `cmapfile`, `cardid`, DF1F) with exact byte alignments. A single off-by-one error or missing BER-TLV tag in responses causes `msclmd` to silently abort.

### Solution: The 100% Custom Minidriver (No msclmd)
By building our own custom minidriver (`euds_minidriver.dll`), we **bypass msclmd completely**. 
- We control exactly what file layout, containers, and properties are exposed to the Base CSP.
- We don't have to emulate any complex files like `cardcf` or `cmapfile` on the smartcard itself if we don't want to; we can intercept those reads in the minidriver and answer them directly with hardcoded configurations!
- Cryptographic operations (signing, decryption) are sent over our custom wire protocol directly to the virtual card.

---

## 4. CSP/KSP Interaction Sequence with a Minidriver

When `certutil` or an enrollment agent initiates access, the Base CSP/KSP drives the following state machine against the minidriver:

### Step 1: Context Acquisition
- Base CSP calls `CardAcquireContext(pCardData, dwFlags)`.
- The minidriver populates `CARD_DATA` with its function pointers.
- **CRITICAL SETUP**:
  - `pvVendorSpecific` must be set to a non-NULL static pointer or context block. If left NULL, the CSP assumes initialization failed and aborts.
  - Core function pointers (e.g., `pfnCardReadFile`, `pfnCardGetProperty`, `pfnCardGetContainerInfo`) must be set.

### Step 2: Property Verification
The CSP queries card capabilities to see what features are supported:
- **`CardGetProperty("Card Identifier")`**: Must return a unique 16-byte card GUID.
- **`CardReadFile("(null)", "cardcf")`**: The cache file.
  - If we support caching, we return 6 bytes.
  - **Freshness Counters**: If the freshness counters (`wContainersFreshness`, `wFilesFreshness`) change, the CSP invalidates its local Windows cache and re-reads the card.
- **`CardGetProperty("Read Only Mode")`**: Returns `TRUE` — card is read-only (MS §7.4). All write ops blocked at Base CSP layer.
- **`CardGetProperty("Supports Windows x.509 Enrollment")`**: Returns `FALSE` — enrollment not supported on read-only card (MS §7.4). Certificate enumeration still works via `CardReadFile("mscp", "kxc00")` + `CardGetContainerInfo`.

### Step 3: Container Discovery & Enumeration
- **`CardReadFile("mscp", "cmapfile")`**: The container map file.
  - Contains packed `CONTAINER_MAP_RECORD` entries (86 bytes each).
  - Each record defines the container name (e.g., `"Private Key 00"`), flags (`0x03` = VALID | DEFAULT), and supported key sizes (e.g., `2048` bits).
- **`CardGetContainerInfo(bContainerIndex)`**:
  - Returns `CONTAINER_INFO` containing the public key blobs (Signature and Key Exchange public keys).
  - The CSP uses this to extract the public key modulus and exponent.
- **`CardReadFile("mscp", "kxc00")` / `CardReadFile("mscp", "kxs00")`**:
  - Contains the actual X.509 certificates associated with key exchange and signature keys.
  - Format: 2-byte container index followed by zlib-compressed DER certificate bytes.

### Step 4: PIN Authentication
- **`CardAuthenticatePin`**: Called when the CSP needs to sign or access private keys.

### Step 5: Cryptographic Operations
- **`CardSignData`**: Base CSP passes a hash, signing flags, and container index. Minidriver signs the hash and returns the raw RSA signature.
- **`CardRSADecrypt`**: Used for key exchange decryption.

---

## 5. Architectural Design Principles for eUDS

To make the system robust and simple, we follow these architectural rules:

1. **Card API Emulation Level**: The virtual card on the Linux side does **NOT** need to implement a full ISO 7816-4 filesystem. It only needs to answer a very simple wire protocol for private key operations.
2. **Minidriver is the Brain**: Our Windows minidriver serves `cardcf`, `cmapfile`, and public keys directly from hardcoded structures or local caching, keeping the card APDU layer extremely thin.
3. **ATR Registry Registration**: We register our custom `eUDS-Card` ATR so that SCardSvr couples it directly with `euds_minidriver.dll`, bypassing the Microsoft inbox minidriver entirely.
