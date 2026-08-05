# eUDS Futures — Post-MVP Roadmap

> Documenta ideas, investigaciones y features post-MVP. No bloquea implementación actual.

---

## 1. KEM-ML-ML Key Exchange via APDU (Post-Quantum PIN Protection)

### Motivation
Protect PIN from MITM on RDP smart card redirection channel, even if TLS/NLA is disabled or compromised.

### Mechanism: ML-KEM (Kyber) via Extended APDU

| Operation | APDU (CLA=0x80) | Data Sizes (ML-KEM-768) |
|-----------|-----------------|-------------------------|
| Get Engine Public Key | `80 50 00 00 00 00 00` (Case 2 ext) | Response: 1184 bytes |
| Encapsulate (Client) | Local ops | Client generates ciphertext (1088B) + shared_secret (32B) |
| Decapsulate (Engine) | `80 51 00 00 00 04 48 [1088B]` (Case 4 ext) | Response: 32 bytes shared_secret |

### Flow

```
Minidriver (Windows)                    Engine (Client)
     │                                        │
     ├─ SELECT Applet ──────────────────────►│
     ├─ GET KEML PUBKEY ────────────────────►│
     │◄─ 1184B pubkey ───────────────────────│
     │                                        │
     │  KEM.Encapsulate(pubkey)                │
     │  → ciphertext (1088B) + ss (32B)       │
     │                                        │
     ├─ KEML DECAPSULATE [ciphertext] ───────►│
     │◄─ 32B shared_secret ──────────────────│
     │                                        │
     │  session_key = KDF(ss, "eUDS-session") │
     │                                        │
     ├─ VERIFY PIN [PIN ⊕ session_key] ──────►│
     │                                        │
     └─ SIGN/DECRYPT [data ⊕ session_key] ───►│
```

### Benefits

- **PIN never in cleartext** on wire
- **Post-quantum**: ML-KEM is NIST PQC standard
- **Forward secrecy**: Each session = new KEM exchange
- **Fits in APDU**: ML-KEM-768 sizes fit in extended APDU (65KB limit)

### Implementation Notes

- Rust crate: `ml-kem` or `pqcrypto-kyber` (no-std compatible?)
- Engine needs `rand_core` + `zeroize` for key handling
- Minidriver needs same crate (Windows target)
- Fallback to classic PIN if KEM not supported (version negotiation)

---

## 2. Browser Integration (WebAuthn / PKCS#11)

### Goal
Same eUDS certificate/key usable from browser via WebAuthn or native messaging.

### Architecture

```
Browser (WebAuthn)          Native Messaging Host          eUDS Engine
     │                            │                            │
     ├─ credentialCreate ────────►│                            │
     │                            ├─ APDU via Native Messaging ►│
     │                            │◄─ response ────────────────┤
     │◄─ attestation ─────────────┤                            │
```

### Challenges

- Native messaging host = small Rust binary (same engine logic)
- WebAuthn expects COSE keys, not raw RSA
- User consent UI in browser vs native PIN dialog
- Origin binding (relying party ID)

### MVP Scope

- Native messaging host binary (Windows/Linux/macOS)
- Chrome/Edge/Firefox manifest
- COSE key conversion (RSA-2048 → COSE_Key)

---

## 3. Certificate Auto-Enrollment / Renewal

### Goal
Windows auto-enrollment via GPO works with eUDS card.

### Requirements

- `CP_SUPPORTS_WIN_X509_ENROLLMENT = TRUE`
- Implement `CardCreateContainer`, `CardWriteFile`, `CardCreateFile`
- Engine must support key generation (RSA-2048 on-card)
- Certificate template enrollment via `certutil` / `certreq`

### Challenges

- Key generation in engine (RSA-2048, secure RNG)
- Certificate request format (PKCS#10) parsing
- Private key never leaves engine — CSR signed internally

---

## 4. Multi-Container Support

### Current
Single container: `"eUDS Container 00"`

### Future
Multiple containers per card:
- Container 0: Authentication cert
- Container 1: Signing cert  
- Container 2: Encryption cert
- Each with own PIN/policy

### Changes

- `cmapfile` multiple records
- `kxc00`, `kxc01`, `ksc00`, etc.
- `CardGetContainerInfo` per index
- Per-container PIN state in engine

---

## 5. Hardware Token Integration (YubiKey / Nitrokey)

### Goal
eUDS minidriver can also talk to real hardware tokens via CCID.

### Architecture

```
Minidriver ──SCardTransmit──► Engine ──CCID/PCSC──► YubiKey
```

### Benefits

- Same minidriver for virtual + hardware
- Migration path: start virtual, move to hardware
- Engine abstracts transport (RDP vs local CCID)

---

## 6. Certificate Transparency / Audit Log

### Goal
Log all signing operations for compliance.

### Implementation

- Engine logs: timestamp, operation, hash algorithm, key ID
- Structured logging (JSON) → SIEM integration
- Tamper-evident log (hash chain)

---

## 7. Remote Attestation

### Goal
Prove to remote party that engine is genuine eUDS build.

### Mechanism

- Engine has embedded certificate (issued by UDS CA)
- `GET ATTESTATION` APDU returns signed quote
- Quote includes: engine version, git hash, build timestamp, measurements

---

## Priority Matrix

| Feature | Effort | Impact | Target |
|---------|--------|--------|--------|
| KEM-ML PIN protection | Medium | High (security) | v1.1 |
| Browser/WebAuthn | High | High (reach) | v1.2 |
| Auto-enrollment | High | Medium (enterprise) | v1.3 |
| Multi-container | Medium | Medium | v1.2 |
| Hardware token | High | Low (niche) | v2.0 |
| Audit log | Low | Medium | v1.1 |
| Remote attestation | Medium | Low | v1.3 |

---

## Notes

- All futures are **post-MVP**. Current MVP = virtual card + minidriver + RDP redirect working.
- Engine multiplatform (Windows/Linux/macOS) is core requirement for all futures.
- Keep APDU protocol extensible: new INS codes, version negotiation in SELECT.