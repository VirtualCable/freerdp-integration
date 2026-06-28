# ISO 7816-4 Relevant Rules for Custom APDU Protocol Design

> Extracted from ISO/IEC 7816-4:2005(E) — "Organization, security and commands for interchange"
> Focus: Rules applicable to proprietary/custom APDU protocol design.

---

## Terminology (ISO §5.1)

| Term | Definition |
|------|-----------|
| **Nc** | Number of bytes in the command data field. The Lc field encodes Nc. If the Lc field is absent, then Nc is zero. |
| **Ne** | Maximum number of bytes expected in the response data field. The Le field encodes Ne. If the Le field is absent, then Ne is zero. |
| **Nr** | Number of bytes in the response data field. Nr shall be less than or equal to Ne. |

---

## 1. APDU Structure

### Command APDU Layout

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | CLA | 1 byte | Class byte |
| 1 | INS | 1 byte | Instruction byte |
| 2 | P1 | 1 byte | Parameter 1 |
| 3 | P2 | 1 byte | Parameter 2 |
| 4 | Lc | 0, 1, or 3 bytes | Length of command data (absent if Nc=0) |
| 4+Lc | Data | Nc bytes | Command data field (absent if Nc=0) |
| end | Le | 0, 1, 2, or 3 bytes | Expected length of response (absent if Ne=0) |

### Response APDU Layout

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | Data | Nr bytes | Response data field (Nr ≤ Ne, absent if Nr=0) |
| end | SW1 | 1 byte | Status byte 1 |
| end+1 | SW2 | 1 byte | Status byte 2 |

> **ISO 7816-4 §5.1:** "There shall be no interleaving of command-response pairs across the interface, i.e., the response APDU shall be received before initiating another command-response pair."

> **ISO 7816-4 §5.1:** "In any command-response pair comprising both Lc and Le fields, short and extended length fields shall not be combined: either both of them are short, or both of them are extended."

---

## 2. APDU Cases

The standard defines four structural cases based on presence of command data (Nc) and expected response data (Ne):

### Case 1: No Command Data, No Response Data
```
Command:  [CLA] [INS] [P1] [P2]
Response: [SW1] [SW2]
```
- Minimum command: 4 bytes
- Minimum response: 2 bytes

### Case 2: No Command Data, Response Data Expected
```
Command:  [CLA] [INS] [P1] [P2] [Le]
Response: [Data (Nr bytes)] [SW1] [SW2]
```
- Short Le: 1 byte (Ne = 1..256)
- Extended Le: 2 or 3 bytes (Ne = 1..65536)

### Case 3: Command Data, No Response Data
```
Command:  [CLA] [INS] [P1] [P2] [Lc] [Data (Nc bytes)]
Response: [SW1] [SW2]
```
- Short Lc: 1 byte (Nc = 1..255)
- Extended Lc: 3 bytes (Nc = 1..65535)

### Case 4: Command Data AND Response Data
```
Command:  [CLA] [INS] [P1] [P2] [Lc] [Data (Nc bytes)] [Le]
Response: [Data (Nr bytes)] [SW1] [SW2]
```

### Byte Count Summary

| Case | Min Command | Max Command (short) | Max Command (extended) | Min Response |
|------|-------------|---------------------|------------------------|--------------|
| 1 | 4 | 4 | 4 | 2 |
| 2 | 5 | 5 | 7 | 2 |
| 3 | 5 | 260 | 65540 | 2 |
| 4 | 6 | 261 | 65544 | 2 |

---

## 3. CLA Byte (Class)

### Bit Structure (Interindustry Class, bit 8 = 0)

| Bit 8 | Bit 7 | Bit 6 | Bit 5 | Bit 4 | Bit 3 | Bit 2 | Bit 1 |
|-------|-------|-------|-------|-------|-------|-------|-------|
| 0 | 0 | 0 | Chain | SM1 | SM0 | Ch1 | Ch0 |

| Bits | Meaning |
|------|---------|
| b8=0 | Interindustry class |
| b8=1 | **Proprietary class** (application-defined meaning for b7-b1) |
| b7-b6 | `00` = first interindustry values, `01` = further interindustry values |
| b5 | **Command chaining**: `0` = last/only command, `1` = not last command of chain |
| b4-b3 | Secure messaging: `00` = no SM, `01` = proprietary SM, `10` = SM per §6 (header not processed), `11` = SM per §6 (header authenticated) |
| b2-b1 | Logical channel number (0-3 for `000x xxxx`, 0-19 for `01xx xxxx`) |

### Valid/Invalid Values

| Value | Status | Reason |
|-------|--------|--------|
| `0x00`-`0xFE` | Valid | — |
| `0xFF` | **INVALID** | Reserved per ISO/IEC 7816-3 |
| `0x80`-`0xFE` | Valid | Proprietary class (bit 8 = 1) |

> **ISO 7816-4 §5.1.1:** "CLA indicates the class of the command. Due to specifications in ISO/IEC 7816-3, the value 'FF' is invalid. Bit 8 of CLA distinguishes between the interindustry class and the proprietary class."

> **ISO 7816-4 §5.1.1:** "Bit 8 set to 1 indicates the proprietary class, except for the value 'FF' which is invalid. The application-context defines the other bits."

### Proprietary CLA Usage

For custom protocols, use CLA with bit 8 = 1 (`0x80`-`0xFE`):
- All bits b7-b1 are application-defined
- Common convention: `0x80`, `0x84`, `0x90`, `0xA0`, `0xB0`, `0xC0` (but `0xC0` conflicts with GET RESPONSE)
- Avoid `0xFF` (invalid)

---

## 4. INS Byte (Instruction)

### Valid Ranges

| Range | Status |
|-------|--------|
| `0x00`-`0x5F` | Valid (interindustry commands) |
| `0x60`-`0x6F` | **INVALID** (conflicts with SW1) |
| `0x70`-`0x8F` | Valid (interindustry commands) |
| `0x90`-`0x9F` | **INVALID** (conflicts with SW1) |
| `0xA0`-`0xFF` | Valid (interindustry commands) |

> **ISO 7816-4 §5.1.2:** "INS indicates the command to process. Due to specifications in ISO/IEC 7816-3, the values '6X' and '9X' are invalid."

### Proprietary INS Rules

- In the interindustry class, any valid INS code not defined in ISO/IEC 7816 is **reserved for future use** by ISO/IEC JTC 1/SC 17
- In the **proprietary class** (CLA with bit 8 = 1), INS codes are application-defined
- **Bit 1 of INS** (odd/even): In the **interindustry class only**, if bit 1 = 1 (odd INS), data fields shall be BER-TLV encoded. **This convention does NOT apply in the proprietary class.**

### Common Interindustry INS Codes to Avoid

| INS | Command | Notes |
|-----|---------|-------|
| `0x20` | VERIFY | PIN verification |
| `0x22` | MANAGE SECURITY ENVIRONMENT | |
| `0x2A` | PERFORM SECURITY OPERATION | Crypto operations |
| `0xA4` | SELECT | File/application selection |
| `0xC0` | GET RESPONSE | Retrieve data after SW1=61XX |
| `0xB0` | READ BINARY | |
| `0xCA` | GET DATA | |
| `0x84` | GET CHALLENGE | |
| `0x82` | EXTERNAL AUTHENTICATE | |
| `0x88` | INTERNAL AUTHENTICATE | |

---

## 5. Status Words (SW1-SW2)

### Structural Scheme

```
SW1-SW2
├── Process completed
│   └── Normal processing: '9000' and '61XX'
├── Process aborted
    ├── Warning processing: '62XX' and '63XX'
    ├── Execution error: '64XX' to '66XX'
    └── Checking error: '67XX' to '6FXX'
```

> **ISO 7816-4 §5.1.3:** "SW1-SW2 indicates the processing state. Due to specifications in ISO/IEC 7816-3, any value different from '6XXX' and '9XXX' is invalid; any value '60XX' is also invalid."

### Interindustry SW1-SW2 Summary

| SW1-SW2 | Category | Meaning |
|---------|----------|---------|
| `9000` | Normal | Success — no further qualification |
| `61XX` | Normal | More data available; SW2 = bytes still available |
| `62XX` | Warning | NV memory **unchanged**; see SW2 for detail |
| `63XX` | Warning | NV memory **changed**; see SW2 for detail |
| `64XX`-`66XX` | Error | Execution error |
| `6700` | Error | Wrong length (Lc/Le) |
| `68XX` | Error | CLA functions not supported |
| `69XX` | Error | Command not allowed |
| `6AXX` | Error | Wrong parameters P1-P2 |
| `6B00` | Error | Wrong parameters P1-P2 |
| `6CXX` | Error | Wrong Le; SW2 = exact Le to use |
| `6D00` | Error | INS not supported or invalid |
| `6E00` | Error | CLA not supported |
| `6F00` | Error | No precise diagnosis |

### Key Interindustry SW Details

#### `61XX` — More Data Available
- Process completed successfully
- SW2 encodes the number of data bytes still available
- Next command should be **GET RESPONSE** (INS=`0xC0`) with same CLA and Le=SW2

#### `63CX` — Counter (Warning, NV Changed)
- `C` = nibble `1100` (bits 8-5)
- `X` = remaining retries (0-15)
- Common after failed VERIFY: `63C0` = no retries left (blocked), `63C3` = 3 retries left
- Exact meaning depends on the command

#### `6700` — Wrong Length
- Lc or Le field has incorrect length or value
- No further indication

#### Security-Related (`69XX`)

| SW | Meaning |
|----|---------|
| `6982` | Security status not satisfied |
| `6983` | Authentication method blocked |
| `6984` | Reference data not usable |
| `6985` | Conditions of use not satisfied |
| `6986` | Command not allowed (no current EF) |
| `6987` | Expected SM data objects missing |
| `6988` | Incorrect SM data objects |

#### Parameter-Related (`6AXX`)

| SW | Meaning |
|----|---------|
| `6A80` | Incorrect parameters in command data field |
| `6A81` | Function not supported |
| `6A82` | File or application not found |
| `6A83` | Record not found |
| `6A84` | Not enough memory space in the file |
| `6A85` | Nc inconsistent with TLV structure |
| `6A86` | Incorrect parameters P1-P2 |
| `6A87` | Nc inconsistent with parameters P1-P2 |
| `6A88` | Referenced data or reference data not found |
| `6A89` | File already exists |
| `6A8A` | DF name already exists |

### NV Memory State Rules

| SW1 Range | NV Memory State |
|-----------|-----------------|
| `63XX`, `65XX` | **Changed** |
| `62XX`, `64XX`-`66XX`, `67XX`-`6FXX` (except 63/65) | **Unchanged** |

> **ISO 7816-4 §5.1.3:** "If the process is aborted with a value of SW1 from '64' to '6F', then the response data field shall be absent."

---

## 6. Extended Length Fields

### When to Use Extended Length

Extended length is used when Nc > 255 or Ne > 255. The card must advertise support via the **third software function table** in historical bytes or EF.ATR (bit 6 set to 1 = extended Lc/Le supported).

### Lc Field Encoding

| Type | Format | Nc Range | Example |
|------|--------|----------|---------|
| Absent | — | Nc = 0 | — |
| Short | 1 byte: `Nc` | 1-255 | `0xFF` = 255 bytes |
| Extended | 3 bytes: `0x00` `Nc_hi` `Nc_lo` | 1-65535 | `0x00 0x01 0x00` = 256 bytes |

> **ISO 7816-4 §5.1:** "A short Lc field consists of one byte not set to '00'. From '01' to 'FF', the byte encodes Nc from one to 255."
>
> "An extended Lc field consists of three bytes: one byte set to '00' followed by two bytes not set to '0000'. From '0001' to 'FFFF', the two bytes encode Nc from one to 65 535."

### Le Field Encoding

| Type | Format | Ne Range | Special |
|------|--------|----------|---------|
| Absent | — | Ne = 0 | No response data expected |
| Short | 1 byte | 1-256 | `0x00` = **256** bytes |
| Extended (no Lc) | 3 bytes: `0x00` `Ne_hi` `Ne_lo` | 1-65536 | `0x00 0x00 0x00` = **65536** bytes |
| Extended (with Lc) | 2 bytes: `Ne_hi` `Ne_lo` | 1-65536 | `0x00 0x00` = **65536** bytes |

> **ISO 7816-4 §5.1:** "If the byte is set to '00', then Ne is 256." (short Le)
>
> "If the two bytes are set to '0000', then Ne is 65 536." (extended Le)

### Nc/Ne = 0 Special Meaning

| Field | Encoded as | Actual Meaning |
|-------|-----------|----------------|
| Short Lc | `0x00` | **Invalid** (Lc must be absent for Nc=0) |
| Extended Lc | `0x00 0x00 0x00` | **Invalid** (Lc must be absent for Nc=0) |
| Short Le | `0x00` | Ne = **256** |
| Extended Le | `0x00 0x00 0x00` or `0x00 0x00` | Ne = **65536** |

### Extended APDU Case 4 Layout

When both extended Lc and extended Le are present:
```
[CLA] [INS] [P1] [P2] [0x00] [Nc_hi] [Nc_lo] [Data...Nc bytes...] [Ne_hi] [Ne_lo]
```
Note: Le is only 2 bytes (not 3) when extended Lc is present.

---

## 7. Command Chaining

### Mechanism

Command chaining allows splitting a large data transfer across multiple APDUs.

| CLA Bit 5 | Meaning |
|-----------|---------|
| `0` | Last or only command of the chain |
| `1` | Not the last command of the chain |

### Rules

> **ISO 7816-4 §5.1.1.1:** "For chaining in the interindustry class, bit 5 of CLA shall be used while the other seven bits are constant."

> **ISO 7816-4 §5.1.1.1:** "In response to a command that is not the last command of a chain, SW1-SW2 set to '9000' means that the process has been completed so far; warning indications are prohibited."

### Chaining Status Words

| SW | Meaning |
|----|---------|
| `9000` | Chain segment accepted (intermediate command) |
| `6883` | Last command of the chain expected |
| `6884` | Command chaining not supported |

### Chain Flow

```
[CLA(b5=1)] [INS] [P1] [P2] [Lc] [Data part 1] → 9000
[CLA(b5=1)] [INS] [P1] [P2] [Lc] [Data part 2] → 9000
[CLA(b5=0)] [INS] [P1] [P2] [Lc] [Data part N] → 9000 (or other final SW)
```

> **ISO 7816-4 §5.1.1.1:** "This document specifies the card behaviour only in the case where, once initiated, a chain is terminated before initiating a command-response pair not part of the chain. Otherwise the card behaviour is not specified."

---

## 8. GET RESPONSE (INS = 0xC0)

### Purpose

Retrieves response data that could not be transmitted in the original response APDU (typically after SW1=`61XX`).

### Command Format

| Field | Value |
|-------|-------|
| CLA | Same CLA as the preceding command |
| INS | `0xC0` |
| P1-P2 | `0x00 0x00` |
| Lc | Absent |
| Le | Number of bytes to retrieve (use SW2 from `61XX`) |

### Response

| Field | Content |
|-------|---------|
| Data | Up to Ne bytes of the pending response |
| SW1-SW2 | `61XX` if more data remains, `9000` if complete |

> **ISO 7816-4 §5.1.3:** "If SW1 is set to '61', then the process is completed and before issuing any other command, a GET RESPONSE command may be issued with the same CLA and using SW2 (number of data bytes still available) as short Le field."

### GET Response Flow

```
Command → SW1=61, SW2=0x80 (128 bytes available)
GET RESPONSE (Le=0x80) → [128 bytes data] SW1=61, SW2=0x40 (64 more bytes)
GET RESPONSE (Le=0x40) → [64 bytes data] SW1=90, SW2=0x00 (complete)
```

---

## 9. SELECT (INS = 0xA4)

### Command Format

| Field | Value |
|-------|-------|
| CLA | As defined in §5.1.1 |
| INS | `0xA4` |
| P1 | Selection mode (see below) |
| P2 | Response mode (see below) |
| Lc | Length of data field |
| Data | File identifier / path / DF name / AID |
| Le | Expected length of FCI |

### P1 — Selection Mode

| P1 | Meaning | Data Field |
|----|---------|------------|
| `0x00` | Select by file identifier | File identifier (2 bytes) or absent |
| `0x01` | Select child DF | DF identifier (2 bytes) |
| `0x02` | Select EF under current DF | EF identifier (2 bytes) |
| `0x03` | Select parent DF of current DF | Absent |
| `0x04` | **Select by DF name (AID)** | DF name / AID (1-16 bytes) |
| `0x08` | Select by path (from MF) | Path without MF identifier |
| `0x09` | Select by path (from current DF) | Path without current DF identifier |

### P2 — Response Mode

| P2 (b4-b3) | Meaning |
|------------|---------|
| `0x00` | Return FCI template (optional FCI tag) |
| `0x04` | Return FCP template (mandatory FCP tag) |
| `0x08` | Return FMD template (mandatory FMD tag) |
| `0x0C` | No response data (or proprietary if Le present) |

### Common AID Selection

```
CLA=0x00 INS=0xA4 P1=0x04 P2=0x00 Lc=<AID_len> Data=<AID> Le=0x00
```

> **ISO 7816-4 §8.2.2.2:** "The card shall support a SELECT command with CLA INS P1 P2 set to '00A4 0400' for the first selection with a given and preferably complete application identifier in the command data field."

### SELECT Status Words

| SW | Meaning |
|----|---------|
| `9000` | Success |
| `6283` | Selected file deactivated |
| `6284` | FCI not formatted per §5.3.3 |
| `6A80` | Incorrect parameters in data field |
| `6A81` | Function not supported |
| `6A82` | File or application not found |
| `6A86` | Incorrect P1-P2 |
| `6A87` | Nc inconsistent with P1-P2 |

---

## 10. VERIFY (INS = 0x20)

### Command Format

| Field | Value |
|-------|-------|
| CLA | As defined in §5.1.1 |
| INS | `0x20` (or `0x21` for BER-TLV verification data) |
| P1 | `0x00` (no further info) |
| P2 | Reference data qualifier (see below) |
| Lc | Length of verification data (absent to check status) |
| Data | Verification data (e.g., PIN) |
| Le | Absent |

### P2 — Reference Data Qualifier

| Bits | Meaning |
|------|---------|
| b8=0 | Global reference data (MF-specific) |
| b8=1 | Specific reference data (DF-specific) |
| b7=0 | No SE reference |
| b7=1 | Reserved |
| b6-b1 | Qualifier (PIN number, key number, or short EF ID) |

### VERIFY Status Words

| SW | Meaning |
|----|---------|
| `9000` | Verification successful |
| `63C0` | Verification failed, **no retries left** (blocked) |
| `63CX` | Verification failed, X retries remaining (1-15) |
| `6982` | Security status not satisfied |
| `6983` | Authentication method blocked |
| `6A88` | Reference data not found |

### Check Retry Counter (No Data)

```
CLA=0x00 INS=0x20 P1=0x00 P2=0x00 Lc=absent → 63CX (retries) or 9000 (not required)
```

> **ISO 7816-4 §7.5.6:** "The absence of command data field is used to check whether the verification is required (SW1-SW2 = '63CX' where 'X' encodes the number of further allowed retries), or not (SW1-SW2 = '9000')."

---

## 11. PERFORM SECURITY OPERATION (INS = 0x2A)

> Note: PSO is formally defined in ISO/IEC 7816-8. The following covers the GIDS implementation pattern.

### Command Format

| Field | Value |
|-------|-------|
| CLA | `0x00` or `0x10` (with command chaining) |
| INS | `0x2A` |
| P1 | Tag for response data element, or `0x00` (no response data) |
| P2 | Tag for command data element, or `0x00` (no command data) |
| Lc | Length of command data |
| Data | Value of the DO specified in P2 |
| Le | Expected response length |

### GIDS PSO Operations

| Function | P1 | P2 | Command Data | Response Data |
|----------|----|----|-------------|---------------|
| PSO: COMPUTE DIGITAL SIGNATURE | `0x9E` | `0x9A` | DER-encoded digest info | Plain signature |
| PSO: ENCIPHER | `0x86` | `0x80` | Plain data | Enciphered value |
| PSO: DECIPHER | `0x80` | `0x86` | Cryptogram | Deciphered value |

### PSO Status Words

| SW | Meaning |
|----|---------|
| `9000` | Successful execution |
| `6700` | Wrong Lc |
| `6982` | Security status not satisfied |
| `6A86` | Incorrect P1-P2 |

> **GIDS Spec:** "GIDS does not define default algorithm. As a result, the key to use with a PSO command must have been explicitly specified initially with a MSE SET command."

### Typical PSO Flow

```
1. MSE SET (INS=0x22) → Select key/algorithm
2. PSO (INS=0x2A) → Perform crypto operation
```

---

## Quick Reference: Proprietary Protocol Design Checklist

### DOs
- [ ] Use CLA with bit 8 = 1 (`0x80`-`0xFE`) for proprietary class
- [ ] Avoid INS `0x6X` and `0x9X` (invalid per ISO 7816-3)
- [ ] Avoid CLA `0xFF` (invalid per ISO 7816-3)
- [ ] Avoid SW1-SW2 `0x60XX` (invalid)
- [ ] Use `0x9000` for success
- [ ] Use `0x61XX` for more data available (requires GET RESPONSE support)
- [ ] Use `0x6CXX` for wrong Le (SW2 = correct Le)
- [ ] Use `0x63CX` for retry counters
- [ ] Use `0x6700` for wrong length
- [ ] Use `0x6982` for security not satisfied
- [ ] Use `0x6A86` for wrong P1-P2
- [ ] Use `0x6A80` for wrong data field
- [ ] Short Lc: `0x01`-`0xFF` (never `0x00`)
- [ ] Short Le: `0x00` = 256, `0x01`-`0xFF` = 1-255
- [ ] Extended Lc: `0x00` + 2-byte big-endian (never `0x00 0x00 0x00`)
- [ ] Extended Le: `0x00 0x00` = 65536 (when Lc present) or `0x00 0x00 0x00` (when Lc absent)
- [ ] Do not mix short and extended length fields in same APDU
- [ ] Command chaining: bit 5 of CLA, `0` = last, `1` = more coming
- [ ] No interleaving: must receive full response before next command

### DON'Ts
- [ ] Don't use INS `0xC0` (GET RESPONSE) for proprietary commands
- [ ] Don't use INS `0xA4` (SELECT) for proprietary commands
- [ ] Don't use INS `0x20` (VERIFY) for proprietary commands
- [ ] Don't use INS `0x2A` (PSO) for proprietary commands
- [ ] Don't return response data with SW1 `0x64`-`0x6F`
- [ ] Don't return warning SW (`0x62`, `0x63`) for intermediate chain commands

---

## 12. Additional ISO 7816-4 Rules

### RFU Bits

> **ISO 7816-4 §5.1:** "In CLA, INS, and SW bytes, RFU bits shall be set to 0 unless otherwise specified."

### Proprietary CLA Chaining Caveat

> ISO command chaining (CLA bit 5) is specified for the interindustry class only. In the proprietary class, chaining behavior is application-defined.

### P1-P2 Default Convention

> **ISO 7816-4 §5.1:** "A parameter byte set to '00' generally provides no further qualification."

### Proprietary SW Codes

> Values `67XX`, `6BXX`, `6DXX`, `6EXX`, `6FXX` and `9XXX` are proprietary, except `6700`, `6B00`, `6D00`, `6E00`, `6F00` and `9000` which are interindustry.
