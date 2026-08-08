# Smartcard: Fase de Conexión y Descubrimiento — Hallazgos 2026-08-05

> Documento de referencia sobre cómo msclmd descubre la tarjeta redirigida **al conectar**,
> y sobre el detalle crítico de **orden de bytes** del módulo RSA en `ContainerInfo_00`.
> Este documento corrige y amplía conclusiones anteriores de `smartcard-native-vs-ours.md`.

---

## Contexto

- Canal: nuestro addin Rust (`rdp/src/addins/smartcard/`) + backend pcsc (`uds-client/crates/channels/src/smartcard/native/`).
- Tarjeta: N99 + Java Card (EJCOP 242 R3), ATR `3BF913...`, lector "Generic EMV Smartcard Reader 0".
- RDP client (nosotros) en el host físico; `certutil -scinfo` en la VM remota (msclmd redirigido por RDPSC).

## Resumen Ejecutivo

1. **msclmd hace TODO el descubrimiento de la tarjeta en la fase de CONEXIÓN** (antes de que
   certutil haga nada). Al conectar, emite `READ_CACHE` para cada entrada; al recibir `MISS`
   (cache en memoria vacío), **lee la tarjeta por TRANSMIT** y rellena la caché con `WRITE_CACHE`.
2. El **pubkey del contenedor se lee de la tarjeta SIN PIN** mediante `GET DATA 7F49` + `GET RESPONSE`
   (en trozos de ≤258 bytes). No se necesita `SCARD_E_INSUFFICIENT_BUFFER` ni el OS cache.
3. El **módulo del pubkey se guarda en `ContainerInfo_00` en little-endian** (invertido respecto a
   como lo devuelve la tarjeta). Por eso el OS cache mostraba `4D11...` y el `7F49` de la tarjeta
   devuelve `82E2...`: **son la misma clave**, en distinto orden de bytes.
4. El PIN (VERIFY) solo lo pide certutil cuando necesita **operar con la clave privada**
   (firma/descifrado), nunca para leer el contenedor.

---

## Secuencia observada en la fase de conexión (log real, run "a cero")

Log: cache OS limpiado (tarjeta extraída y reintroducida), sin certutil, solo conectar + esperar + cerrar.
Timestamps del run de las 17:33.

| Tiempo | Evento | Detalle |
|--------|--------|---------|
| 17:33:30 | SCardAccessStartedEvent / EstablishContext / ListReaders | Channel registrado, lector detectado |
| 17:33:32 | SELECT PIV / SELECT MF / SELECT AID | `00 A4 04 00 09 A0 00 00 03 97 42 54 46 59 00` (GIDS) |
| 17:33:32 | GET DATA DF1F (cardapps) / DF20 (cardid) / DF22 (cardcf) / 7F73 | Lectura de metadatos de la tarjeta |
| 17:33:32 | `READ_CACHE` → **MISS** (`Cached_CardProperty_*`, cmapfile, etc.) | msclmd pregunta por cada entrada |
| 17:33:32 | TRANSMIT (lectura de la tarjeta) → `WRITE_CACHE` | msclmd rellena CardProperty, cmapfile (102 B)... |
| 17:33:33 | `READ_CACHE` → **MISS** (`Cached_ContainerInfo_00`) | `ReturnCode: SCARD_W_CACHE_ITEM_NOT_FOUND` |
| 17:33:33 | TRANSMIT `00 CB A0 00 04 5C 02 DF 20 00` | lee cardid (18 B) |
| 17:33:33 | TRANSMIT `00 CB 3F FF 0A 70 08 84 01 81 A5 03 7F 49 80 00` | `GET DATA 7F49` → **258 bytes** |
| 17:33:33 | TRANSMIT `00 C0 00 00 0E` | `GET RESPONSE` → **16 bytes** (resto del DO) |
| 17:33:33 | `WRITE_CACHE` `Cached_ContainerInfo_00` (308 bytes) | msclmd construye y cachea el contenedor |
| 17:33:33+ | `READ_CACHE` → **HIT** (308 bytes) | certutil/uso posterior lee del cache |

**Clave:** el `7F49` devuelve 258 bytes (Le=0x00 del caller, 256 + SW) y el resto se obtiene con
`GET RESPONSE`. **Ninguna respuesta supera los 258 bytes**, así que nunca se dispara
`SCARD_E_INSUFFICIENT_BUFFER`.

---

## El detalle del orden de bytes (crítico)

### Lo que devuelve la tarjeta (`GET DATA 7F49`)

Respuesta (258 bytes, big-endian, TLV 7F49):

```
7F 49 82 01 09 81 82 01 00
82 E2 17 BF 25 88 63 5F ...  (módulo, 256 bytes, big-endian)
... EC 3F 9C 11 4D            (últimos bytes del módulo)
82 03 01 00 01                (exponente público 0x010001)
```

- Módulo (big-endian) empieza por `82 E2 17 BF ...` y termina en `... EC 3F 9C 11 4D`.
- Se trata de la clave pública del **certificado** (firma). La tarjeta la sirve **sin selección ni PIN**.

### Lo que msclmd guarda en `ContainerInfo_00`

`WRITE_CACHE` → 308 bytes, cuyo campo de módulo empieza por:

```
4D 11 9C 3F EC 23 09 4E 65 EB 87 EA D9 7D 45 6B ...
```

Observa: `4D 11 9C 3F EC 23 09 4E 65` = **inverso byte a byte** de los últimos bytes del módulo
big-endian (`65 4E 09 23 EC 3F 9C 11 4D`). Es el **mismo módulo en little-endian**.

### Conclusión

- `4D11...` (OS cache / ContainerInfo_00) y `82E2...` (7F49 de la tarjeta) **NO son dos claves
  distintas** (keyexchange vs certificado): son la **misma clave RSA** del certificado en distinto
  orden de bytes.
- Por eso el OS cache servía "4D11" y funcionaba: era el módulo que msclmd ya había invertido.
- Por eso nuestra generación manual con el módulo `82E2` directo **fallaba la "prueba de
  coincidencia"**: lo estábamos insertando en big-endian, cuando Windows espera little-endian.

### Formato del blob de 308 bytes (`ContainerInfo_00`)

```
00 01 00 05 00 00 00 00 00 00 00 24 01 00 00 00 00 00 00 00 00 00 14 01 00 00
06 02 00 00 00 A4 00 00 52 53 41 31 00 08 00 00 01 00 01 00
<modulus little-endian, 256 bytes>
82 03 01 00 01   (exponente 0x010001)
```
(Estructura: header de CSP + `CONTAINER_INFO` con blob RSA: bits de clave 1, algoritmo "RSA1",
tamaño 0x00000800, clave pública en little-endian.)

---

## Por qué `SCARD_E_INSUFFICIENT_BUFFER` no es necesario

Hipótesis previa (errónea): "el pubkey viene en una sola respuesta >258 bytes y el wire no lo
entrega". Realidad:

- msclmd pide el pubkey con `Le=0x00` (256 bytes máx) → la tarjeta devuelve **258 bytes**
  (256 + SW) y marca `61 XX` → msclmd hace `GET RESPONSE` para el resto.
- Todas las respuestas individuales son ≤258. Nunca se supera el buffer del caller.
- Un addin correcto debe **devolver la respuesta tal cual** (sin concatenar GET RESPONSE en el
  canal — lo hicimos y rompía el flujo). msclmd gestiona el chaining por su cuenta.

`SCARD_E_INSUFFICIENT_BUFFER` puede dejarse como guarda defensiva (cubre el caso teórico de una
respuesta única > buffer del caller), pero en esta tarjeta/flujo **nunca se dispara**.

---

## Implicaciones

1. **El flujo es autónomo**: no depende del OS cache del host. En un sistema limpio, msclmd lee
   la tarjeta al conectar y rellena él mismo toda la caché (ContainerInfo incluido).
2. **No hace falta PIN para leer el contenedor**: la clave pública del certificado se lee sin
   autenticar. El PIN (VERIFY `00 20 00 80`) aparece solo en la fase de uso de la clave privada.
3. **`get_container_info` del backend native (lectura OS cache + fallback 7F49) quedó en desuso**:
   con el canal devolviendo `MISS`, msclmd lo hace todo vía TRANSMIT/WRITE_CACHE.
4. **Para la tarjeta emulada**: el diseño es el mismo. msclmd descubrirá la tarjeta emulada con la
   misma secuencia de APDUs (SELECT, GET DATA 7F49, GET RESPONSE...); el backend emulado las
   responde. ContainerInfo_00 lo construye msclmd (no nosotros).

---

## Tarjeta emulada (GIDS)

Emula la tarjeta GIDS de referencia (mismo ATR, AID, cardid y GUID de contenedor) con la clave
pública/certificado del usuario. El backend vive en `uds-client/crates/channels/src/smartcard/emulated/`
(`GidsEngine`).

### Activación

- **Opción** (`uds-client`): `smartcard_emulated` con un spec (JS/launcher):
  - `file:<path>` → fichero PEM local (puede contener cert + key).
  - `pem:<cert_pem>,<key_pem>` → cert y key como PEM directos.
  - `userdefined:` → reservado (futuro; usar un cert del navegador en el cliente HTML5).
  - Si el spec es inválido → warning y **sin smartcard** (el canal no se registra).
- **Entorno (dev/tests)**: `UDS_SMARTCARD_EMULATED=1` + `UDS_SMARTCARD_KEYS="cert.pem;key.pem"`.
- Si no hay smartcard real (spec inválido o PC/SC no disponible) → `SmartcardHandle::new()` devuelve
  `None` y la redirección no se activa (ya no hay backend dummy).

### Formato del certificado

El `Cached_GeneralFile/mscp/kxcXX` debe servirse **comprimido**: `01 00` + longitud sin comprimir
(2 bytes **little-endian**) + DER comprimido con **zlib**. El BaseCSP lo descomprime (el flag
`fCertificateCompression` de CARD_CAPABILITIES está a FALSE en la tarjeta de referencia).

### PIN = password de la clave privada

- El cert (público) se muestra **sin PIN**.
- Al cargar la key en `GidsEngine::new()`:
  - Si la key **no tiene password** → el PIN no es necesario (se firma sin pedirlo).
  - Si la key **está cifrada** → el PIN es su password: `VERIFY` intenta **descifrar la key** con el
    PIN introducido; si abre, correcto (se guarda la `d`); si no, `63 CX`. La parte pública (7F49)
    se sirve desde el **SPKI del certificado** (disponible sin PIN).
- El PIN solo se pide cuando msclmd necesita operar con la clave privada (firmar/descifrar).

### VERIFY como consulta de estado (2026-08-09)

msclmd envía `00 20 00 80` **sin datos** (Lc=0) como **consulta del estado del PIN** antes de
mostrar el diálogo. Debe responder con el contador de intentos **sin decrementar** (`63 CX` con el
valor actual). Un bug inicial lo trataba como intento fallido (password vacía) y bloqueaba la tarjeta
tras unas pocas consultas. Fijado: `data.is_empty()` → reporta `63 CX` actual (o `9000` si ya
verificado) sin tocar el contador.

Al agotar los intentos (`63 C0`), la tarjeta responde `69 83` (auth method blocked) a VERIFY y
PSO devuelve `69 82`; **solo se bloquea la clave privada** — lo público (cert DF24, pubkey 7F49,
propiedades) sigue con `90 00`.

---

## Soporte ECDSA (2026-08-08)

El emulador (`GidsEngine`) detecta automáticamente el tipo de clave al cargarla:

- **RSA** (PKCS#1 v1.5): `7F49` con tags `81` (módulo) + `82` (exponente); PSO devuelve la
  firma cruda de `key_size` bytes.
- **ECDSA P-256 / P-384 / P-521** (mech ref GIDS `0C`/`0D`/`0E`):
  - `7F49` = tag **`86`** con el punto **`04 ‖ X ‖ Y`** (uncompressed).
  - PSO (`2A 9E 9A`) firma el **hash precomputado** enviado por msclmd y devuelve
    `EC Signature ::= SEQUENCE { r INTEGER, s INTEGER }` (DER), tal como define GIDS v2.0.
  - El PIN sigue siendo la password de la key: si la key EC está cifrada, `VERIFY` la descifra
    (PBES2) y guarda el SigningKey; la parte pública se sirve desde el SPKI del certificado.

Detalle de implementación: p521 0.13.x tiene el wrapper `SigningKey` roto (sin
`DecodePrivateKey` ni feature `verifying`), por lo que P-521 parsea el PKCS#8 a mano y
descifra el PEM cifrado vía `pkcs8::EncryptedPrivateKeyInfo` (PBES2).

### BLOQUEADO: descubrimiento del contenedor ECDSA en Windows (2026-08-09)

La **firma** ECDSA del emulador es spec-correcta y está verificada (tests unitarios P-256/384/521,
cifrados y sin cifrar). Pero **msclmd no descubre el contenedor ECDSA**:

- msclmd lee el `7F49` y **solo construye contenedores RSA** (tags `81/82`). El tag `86` (punto ECC)
  lo **ignora** → no crea contenedor. Sirviendo `81=X, 82=Y` msclmd construye un blob **RSA-256
  roto** (trata X como módulo) → `CryptExportPublicKeyInfo` falla.
- Se probó servir `ContainerInfo_00` directamente por el hook `get_container_info`
  (handlers.rs:1080) con varios formatos (BCRYPT_ECCKEY_BLOB puro, X/Y big/little-endian, wrapper
  CAPI BLOBHEADER + CALG_ECDSA_P256, en cbSig/cbKeyEx): el mejor llegó a mostrar el GUID del
  contenedor y a llegar a `CryptExportPublicKeyInfo`, pero **ninguno consigue que el KSP importe la
  clave** (`NTE_BAD_KEYSET` / `RPC_X_NULL_REF_POINTER`).
- **El formato exacto del blob ECC del contenedor no está documentado** (msclmd es código cerrado
  de Microsoft; el spec GIDS no lo cubre). Sin una tarjeta GIDS real que soporte ECDSA para capturar
  la referencia, es adivinar.

**Qué necesitamos para desbloquearlo:** una **tarjeta GIDS/Javacard con soporte ECDSA** real
(parecen poco comunes; la mayoría, incluida la FNMT, usan RSA) para capturar el flujo de referencia
(7F49 ECC + ContainerInfo_XX que msclmd escribe).

**Estado actual:** el emulador cubre el caso **RSA completo** (descubrimiento + cert + firma con
PIN, verificado con certutil). ECDSA queda **parcial**: firma OK, descubrimiento en Windows
pendiente de tarjeta real.

---

## Evidencia

- Runs 17:11, 17:27 (certutil completo, funciona: "coincidencia de clave pública correcta") y
  17:33 (solo conexión, "a cero"). Todos con el canal devolviendo `MISS` en `READ_CACHE` de
  `ContainerInfo_00`.
- `%TEMP%\uds-launcher-tests.log` (+ `.log.1` por rotación de 16 MB).
- Log de referencia nativa: `smartcard-native-reference.log` (el nativo usaba el OS cache ya
  poblado; esta fase de descubrimiento no se ejercita en el nativo).

---

## Trabajo pendiente

- Revisar si el addin debe mantener la generación de `ContainerInfo_00` (código muerto hoy, con
  `get_container_info` devolviendo Err) o eliminarla del todo.
- Emulador: soportar más formatos de entrada (p.e. DER / PKCS#12) si se necesitan, y el caso
  `userdefined:` (cert del navegador en el cliente HTML5).
- Decidir el manejo del `smartcard_emulated` en el cliente HTML5 (cómo exponer un certificado del
  navegador como tarjeta emulada).
- Probar ECDSA de punta a punta con certutil en la VM (`ecdsa256/384/521.crt`, PIN `testpass`).
