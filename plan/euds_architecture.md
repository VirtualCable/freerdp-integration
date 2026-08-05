# eUDS Architecture — Visual Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                        WINDOWS HOST                                  │
│                                                                      │
│  ┌────────────────────┐    ┌──────────────────────────────┐          │
│  │  Application        │    │  Application                 │          │
│  │  (certutil, Outlook)│    │  (Browser, lsass)            │          │
│  └────────┬───────────┘    └──────────────┬───────────────┘          │
│           │                               │                          │
│           ▼                               ▼                          │
│  ┌────────────────────────────────────────────────────────┐          │
│  │  Base CSP / KSP (Microsoft)                            │          │
│  │  • CardAcquireContext         • CardGetContainerInfo    │          │
│  │  • CardReadFile               • CardSignData            │          │
│  │  • CardGetProperty            • CardRSADecrypt          │          │
│  └────────────────────┬───────────────────────────────────┘          │
│                       │ Card Module API                              │
│                       ▼                                              │
│  ┌────────────────────────────────────────────────────────┐          │
│  │  euds_minidriver.dll (OURS)                             │          │
│  │  ┌─────────────────────────────────────────────────┐   │          │
│  │  │ EudsContext (per CardAcquireContext)            │   │          │
│  │  │  • card_id: [u8; 16]                            │   │          │
│  │  │  • cert_der: Option<Vec<u8>>  (cached)          │   │          │
│  │  │  • pub_key_blob: Option<Vec<u8>> (cached)       │   │          │
│  │  │  • pin_verified: bool                           │   │          │
│  │  │  • pin_freshness: u8                            │   │          │
│  │  │  • cache_mode: u32                              │   │          │
│  │  │  Thread-safe: RwLock / Mutex                    │   │          │
│  │  └─────────────────────────────────────────────────┘   │          │
│  │                                                         │          │
│  │  Serves static VFS:                                     │          │
│  │   • cardid    → 16B GUID                                │          │
│  │   • cardcf    → 6B  cache freshness                     │          │
│  │   • cardapps  → "mscp\0\0\0\0"                          │          │
│  │   • mscp\cmapfile → 86B CONTAINER_MAP_RECORD            │          │
│  │   • mscp\kxc00 → DER cert (fetched via APDU)            │          │
│  │                                                         │          │
│  │  Crypto ops via SCardTransmit():                        │          │
│  │   • CardSignData → SIGN APDU                            │          │
│  │   • CardRSADecrypt → DECRYPT APDU                       │          │
│  │   • CardAuthenticateEx → VERIFY PIN APDU                │          │
│  └────────────────────┬───────────────────────────────────┘          │
│                       │ SCardTransmit / SCardControl                  │
│                       ▼                                              │
│  ┌────────────────────────────────────────────────────────┐          │
│  │  SCardSvr (Windows Smart Card Service)                  │          │
│  │  • Detects ATR → loads euds_minidriver.dll via Calais   │          │
│  │  • Routes SCard API calls to RDP smart card channel      │          │
│  └────────────────────┬───────────────────────────────────┘          │
│                       │ RDP Device Redirection                        │
│                       │ (MS-RDPESC over DR_CONTROL)                   │
│                       │ Encrypted: TLS 1.2+ / NLA                     │
└───────────────────────┼───────────────────────────────────────────────┘
                        │
                        ▼
┌──────────────────────────────────────────────────────────────────────┐
│                       LINUX / macOS CLIENT                            │
│  (uds-client — cualquier plataforma)                                    │
│                                                                        │
│  ┌──────────────────────────────────────────────────────────┐         │
│  │  FreeRDP Smartcard Channel Addon (rdp/src/addins/smartcard/)│       │
│  │  • handlers.rs: IOCTL dispatcher                          │         │
│  │  • device.rs: RDP device callbacks                        │         │
│  │  • consts.rs: IOCTL codes                                 │         │
│  │  • mod.rs: device service entry                           │         │
│  └────────────────────┬─────────────────────────────────────┘         │
│                       │ SmartcardIntegration trait                    │
│                       ▼                                               │
│  ┌──────────────────────────────────────────────────────────┐         │
│  │  SmartcardIntegration impl (rdp/src/integrations/smartcard/)│       │
│  │                                                            │         │
│  │  trait SmartcardIntegration {                              │         │
│  │      fn transmit(handle, pci, data) -> TransmitResult;     │         │
│  │      fn status(handle) -> ScardStatus;      // ATR aquí    │         │
│  │      fn connect(reader) -> ConnectResult;                  │         │
│  │      fn list_readers() -> Vec<String>;                     │         │
│  │      fn control(handle, code, data) -> Vec<u8>;            │         │
│  │  }                                                         │         │
│  │  Implementación actual: SmartcardHandle (mod.rs)           │         │
│  │  • Backend seleccionado por env:                           │         │
│  │    - UDS_SMARTCARD_EMULATED=1 → EmulatedBackend            │         │
│  │    - else → DummyBackend                                    │         │
│  └────────────────────┬─────────────────────────────────────┘         │
│                       │                                               │
│                       ▼                                               │
│  ┌──────────────────────────────────────────────────────────┐         │
│  │  eUDS Engine (rdp/src/integrations/smartcard/emulated/)   │         │
│  │  ┌─────────────────────────────────────────────────┐     │         │
│  │  │ EudsEngine                                       │     │         │
│  │  │  • cert_der: Vec<u8>     (DER certificate)      │     │         │
│  │  │  • private_key: RsaPrivateKey                   │     │         │
│  │  │  • pin_mode: PinMode                             │     │         │
│  │  │  • pin: String          (passphrase if Req)      │     │         │
│  │  │  • n: BigUint, d: BigUint, e: BigUint            │     │         │
│  │  │  • sessions: HashMap<ConnectionId, SessionState> │     │         │
│  │  │                                                 │     │         │
│  │  │  process_apdu(conn_id, apdu) -> Vec<u8>          │     │         │
│  │  │    ├─ 0xA4 → SELECT    (AID "eUDS-Card")         │     │         │
│  │  │    ├─ 0xB1 → VERIFY PIN (condicional)            │     │         │
│  │  │    ├─ 0xB4 → GET CERT  (extended APDU)          │     │
│  │  │    ├─ 0x46 → GET PUBKEY (extended APDU)         │     │
│  │  │    ├─ 0xB2 → SIGN      (9E 9A)                  │     │
│  │  │    └─ 0xB3 → DECRYPT   (80 86)                  │     │
│  │  └─────────────────────────────────────────────────┘     │         │
│  │                                                          │         │
│  │  Lee certificado y clave desde env vars:                 │         │
│  │   • UDS_SMARTCARD_CERT_PEM → archivo PEM                 │         │
│  │   • UDS_SMARTCARD_KEY_PEM  → archivo PEM                 │         │
│  │   • UDS_SMARTCARD_PIN → passphrase (si clave encriptada)  │         │
│  └──────────────────────────────────────────────────────────┘         │
│                                                                        │
│  La clave privada NUNCA sale del engine.                               │
│  Solo responde a APDUs de firma/desencriptación.                       │
└────────────────────────────────────────────────────────────────────────┘
```

## Capa de Transporte

```
Windows SCardSvr                       Linux FreeRDP Addon
       │                                        │
       │  RDP Virtual Channel (MS-RDPESC)       │
       │  ┌──────────────────────────────────┐  │
       │  │  Protocolo: DR_CONTROL_REQ/RSP    │  │
       │  │  Cifrado: TLS 1.2+ (NLA)         │  │
       │  │  APDU max: 66,560 bytes           │  │
       │  │  ATR max: 36 bytes                │  │
       │  │  T=1 protocol: extended APDUs + GET RESPONSE fallback  │  │
       │  └──────────────────────────────────┘  │
       │                                        │
```

---

## API Flow: Enumeración de Certificados (certutil -scinfo)

```
Windows                     Minidriver                    Engine
   │                           │                             │
   │ 1. SCardSvr detecta ATR  │                             │
   │    ATR: 3B 89 01 45 55... │                             │
   │ 2. Calais → euds_minidriver.dll                         │
   │                           │                             │
   │──CardAcquireContext──────►│                             │
   │◄──SCARD_S_SUCCESS─────────│                             │
   │                           │                             │
   │──GetProperty(CardId)─────►│                             │
   │◄──16-byte GUID────────────│                             │
   │                           │                             │
   │──ReadFile(cardcf)────────►│                             │
   │◄──6 bytes cache───────────│                             │
   │                           │                             │
   │──GetProperty(ReadOnly)───►│                             │
   │◄──TRUE────────────────────│                             │
   │                           │                             │
   │──GetProperty(X509)───────►│                             │
   │◄－－－❮FALSE❯────────────────│  ←★ READ-ONLY CARD              │
   │                           │                             │
   │──ReadFile(cardapps)──────►│                             │
   │◄──"mscp"──────────────────│                             │
   │                           │                             │
   │──ReadFile(mscp\cmapfile)─►│                             │
   │◄──86 bytes────────────────│                             │
   │                           │                             │
   │──GetContainerInfo(0)─────►│                             │
   │                           │──GET PUBKEY APDU───────────►│
   │                           │  80 46 00 00                │
   │                           │◄──263B pubkey───────────────│
   │◄──BCRYPT_RSAKEY_BLOB──────│                             │
   │                           │                             │
   │──ReadFile(mscp\kxc00)────►│                             │
   │                           │──GET CERT APDU─────────────►│
   │                           │  80 B4 00 00 00 00 00       │
   │                           │◄──DER cert──────────────────│
   │◄──certificate─────────────│                             │
   │                           │                             │
   │ [certutil shows cert]     │                             │
```

---

## API Flow: Operación de Firma

```
Windows                     Minidriver                    Engine
   │                           │                             │
   │──CardAuthenticateEx──────►│                             │
    │   PinId=1, dwFlags=0      │                             │
    │                           │──VERIFY PIN APDU───────────►│
    │                           │  80 B1 00 80 04 [PIN]       │
    │                           │◄──90 00 (OK)────────────────│
    │◄──SCARD_S_SUCCESS─────────│                             │
    │                           │                             │
    │──CardSignData────────────►│                             │
    │   aiHashAlg=SHA256        │                             │
    │   pbData=[32B hash]       │                             │
    │   dwPaddingType=PKCS1     │                             │
    │                           │──SIGN APDU─────────────────►│
    │                           │  80 B2 9E 9A 20 [hash] 00  │
   │                           │                             │
   │                           │              PKCS#1 v1.5:   │
   │                           │              00 01 FF..FF 00│
   │                           │              + DigestInfo    │
   │                           │              m^d mod n      │
   │                           │                             │
    │                           │◄──61 00─────────────────────│
    │                           │──GET RESPONSE──────────────►│
    │                           │  80 C0 00 00 00             │
    │                           │◄──[256B sig] 90 00──────────│
    │◄──[256 bytes signature]───│                             │
```

**NOTA**: El chaining `61 XX` → `GET RESPONSE` lo maneja automáticamente el FreeRDP addon (`handlers.rs:557-580`). El minidriver solo ve la respuesta completa.

---

## API Flow: Operación de Descifrado

```
Windows                     Minidriver                    Engine
   │                           │                             │
   │──CardRSADecrypt──────────►│                             │
   │   pbData=[256B cipher]    │                             │
   │   dwPaddingType=PKCS1     │                             │
   │                           │──DECRYPT APDU──────────────►│
    │                           │  80 B3 80 86 00 01 00      │
   │                           │  [256 bytes ciphertext]     │
   │                           │  00 00                     │
   │                           │                             │
   │                           │              RSA: c^d mod n  │
   │                           │              PKCS#1 unpad   │
   │                           │                             │
   │                           │◄──[plaintext] 90 00─────────│
   │◄──[decrypted data]────────│                             │
```

---

## Diagrama de Estados del PIN

```
                 ┌──────────────────┐
                 │  NO VERIFICADO   │◄──────────── Deauthenticate or
                 │  pin_verified=false│             connection reset
                 └────────┬─────────┘
                          │ VERIFY PIN (correcta)
                          ▼
                 ┌──────────────────┐
                 │   VERIFICADO     │────── SIGN/DECRYPT ──────► OK
                 │  pin_verified=true│
                 └────────┬─────────┘
                          │ VERIFY PIN (incorrecta)
                          │  → pin_retries--
                          ▼
                 ┌──────────────────┐
                 │  INTENTO FALLIDO │
                 │  pin_retries > 0 │
                 └────────┬─────────┘
                          │ pin_retries == 0
                          ▼
                 ┌──────────────────┐
                 │    BLOQUEADO     │
                 │  pin_retries = 0 │────── VERIFY PIN ──────► 69 83
                 └──────────────────┘
```

Solo aplica si `PinMode::Required` (clave PEM encriptada).
Si `PinMode::NotRequired`: directamente VERIFICADO, sin PIN necesario.

---

## Configuración de Entorno

```bash
# ─── Testing / Desarrollo: Clave SIN encriptar ───
export UDS_SMARTCARD_EMULATED=1
export UDS_SMARTCARD_CERT_PEM=/path/to/cert.pem
export UDS_SMARTCARD_KEY_PEM=/path/to/key.pem      # PEM sin "ENCRYPTED"
export UDS_SMARTCARD_PIN=                           # vacío = PinMode::NotRequired

# ─── Producción: Clave ENCRYPTADA ───
export UDS_SMARTCARD_EMULATED=1
export UDS_SMARTCARD_CERT_PEM=/path/to/cert.pem
export UDS_SMARTCARD_KEY_PEM=/path/to/key_encrypted.pem  # PEM con "ENCRYPTED"
export UDS_SMARTCARD_PIN=mi_passphrase_segura       # PinMode::Required
```

---

## Límites de Buffer

| Componente | Máximo | Nuestro Uso | ¿Cabe? |
|------------|--------|-------------|--------|
| APDU Send (MS-RDPESC) | 66,560 bytes | 265 bytes (DECRYPT) | ✅ |
| APDU Recv (MS-RDPESC) | 66,560 bytes | ~3 KB (certificado) | ✅ |
| ATR length | 36 bytes | 13 bytes | ✅ |
| Short APDU Lc | 255 bytes | N/A (usamos extended para >255) | ✅ |
| Extended APDU Lc | 65,535 bytes | 256 bytes (DECRYPT) | ✅ |
| GET RESPONSE chain | 256 bytes/chunk | 263 bytes (PUBKEY) → 2 chunks | ✅ |

---

## Registro en Windows (Calais)

```powershell
# 64-bit DLL registry
$path = "HKLM:\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\eUDS Custom Card"
New-Item -Path $path -Force
Set-ItemProperty -Path $path -Name "ATR" -Value @(0x3B,0x89,0x01,0x45,0x55,0x44,0x53,0x2D,0x43,0x61,0x72,0x64,0x96)
Set-ItemProperty -Path $path -Name "ATRMask" -Value @(0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF)
Set-ItemProperty -Path $path -Name "Crypto Provider" -Value "Microsoft Base Smart Card Crypto Provider"
Set-ItemProperty -Path $path -Name "Smart Card Key Storage Provider" -Value "Microsoft Smart Card Key Storage Provider"
Set-ItemProperty -Path $path -Name "80000001" -Value "C:\temp\euds_minidriver.dll"

# 32-bit WoW64
$path32 = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Cryptography\Calais\SmartCards\eUDS Custom Card"
New-Item -Path $path32 -Force
Set-ItemProperty -Path $path32 -Name "ATR" -Value @(0x3B,0x89,0x01,0x45,0x55,0x44,0x53,0x2D,0x43,0x61,0x72,0x64,0x96)
Set-ItemProperty -Path $path32 -Name "ATRMask" -Value @(0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF)
Set-ItemProperty -Path $path32 -Name "Crypto Provider" -Value "Microsoft Base Smart Card Crypto Provider"
Set-ItemProperty -Path $path32 -Name "Smart Card Key Storage Provider" -Value "Microsoft Smart Card Key Storage Provider"
Set-ItemProperty -Path $path32 -Name "80000001" -Value "C:\temp\euds_minidriver_x86.dll"
```
