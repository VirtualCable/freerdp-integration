# Smartcard: Canal Nativo FreeRDP vs. Nuestro Addin Rust — Diferencias

> ⚠️ **ACTUALIZACIÓN (2026-08-05):** varias conclusiones de este documento quedaron obsoletas.
> Ver **`smartcard-connect-phase-discovery.md`** para los hallazgos que las corrigen:
> - el pubkey del contenedor SÍ se puede leer de la tarjeta por TRANSMIT (en trozos, sin PIN);
> - `4D11...` y `82E2...` son la misma clave (byte-order), no keyexchange vs certificado;
> - `INSUFFICIENT_BUFFER` no es necesario (msclmd trocea con GET RESPONSE);
> - el flujo es autónomo sin OS cache.
>
> Referencias (históricas):

> Documento de análisis comparativo. Referencias:
> - Nativo: `Z:\projects\FreeRDP\channels\smartcard\client\smartcard_main.c`, `libfreerdp\utils\smartcard_call.c`, `smartcard_pack.c`, `smartcard_operations.c`
> - Nuestro: `rdp/src/addins/smartcard/` (mod.rs, device.rs, handlers.rs, consts.rs) + `rdp/src/integrations/smartcard/` (trait)
> - Backend nativo en uds-client: `crates/channels/src/smartcard/native/`

---

## Resumen Ejecutivo

El canal nativo de FreeRDP **funciona** con la tarjeta física, pero NO porque haga nada especial con los APDUs — funciona porque usa el **cache del OS** (`SCardReadCacheW`/`SCardWriteCacheW`), que ya tenía los datos del contenedor de sesiones previas. En el run nativo comprobado, **nunca lee el cmapfile ni el pubkey de la tarjeta** (0 lecturas `DF23`/`7F49`).

Nuestro canal (cache en memoria, vacío al arrancar) fuerza a msclmd a leer el pubkey `7F49` (272 bytes) de la tarjeta. A través del wire RDPSC esto **falla**: el caller reserva un buffer de 258 bytes y msclmd **no reintenta** con uno mayor. Resultado: `CardGetContainerInfo` falla → `Contenedor de claves = (null)`.

**La divergencia fundamental no es de formato de respuesta: es que el nativo depende del OS cache para darle a msclmd la info del contenedor, y nosotros intentamos dársela por el camino de la tarjeta (que el wire RDPSC no soporta para respuestas >258 bytes).**

---

## Tabla de Divergencias

| # | Área | Nativo FreeRDP (C) | Nuestro Addin (Rust) | Impacto |
|---|------|---------------------|----------------------|---------|
| 1 | **Cache** | `SCardReadCacheW`/`SCardWriteCacheW` (OS cache, persistente por tarjeta, key = CardIdentifier+LookupName+Freshness) | Original: `HashMap` en memoria key = LookupName, READ siempre NOT_FOUND. Tras fixes: HIT + delegación al OS cache | **CRÍTICO** — msclmd obtiene ContainerInfo_00 del cache; sin él lee el 7F49 y falla |
| 2 | **Transmit respuesta > buffer** | Windows `SCardTransmit` trunca a `cbRecvLength` (258) y devuelve SUCCESS | Original: devolvía el dato completo (272) → el remoto no lo acepta. INSUFFICIENT_BUFFER → msclmd no reintenta. Truncar → pubkey roto | **CRÍTICO** — el wire RDPSC no entrega >258 al caller |
| 3 | **GSC (GET_STATUS_CHANGE)** | Async (hilo por contexto), pasos de 100ms (`MIN(dwTimeOut,100)`), en timeout **empaqueta los states** y devuelve `SCARD_E_TIMEOUT` | Original: **bloqueaba** el hilo único con timeout INFINITO (24h), en timeout devolvía body vacío. Tras fixes: paso 100ms + states empaquetados | Alto — estados mal empaquetados → SCardSvr re-enumera (flood) |
| 4 | **Bit 0x10000 en reader states** | Pasa los states **verbatim** de `SCardGetStatusChangeW` (incluye el bit `0x10000` del reader `\\?PnP?\Notification`) | El crate `pcsc` usa `State::from_bits_truncate` que **descarta** el bit 0x10000. Fix: leer `dwEventState` crudo vía transmute | Alto — sin el bit, el SCardSvr remoto ve cambios espurios |
| 5 | **pioRecvPci en Transmit_Return** | Devuelve **NULL** (verificado: `pioRecvPci: null` en 153 returns) | Original: eco del PCI del caller. Tras fixes: NULL | Medio |
| 6 | **GET_DEVICE_TYPE_ID** | Body = solo `dwDeviceId` (4 bytes); ReturnCode en el header. Llama `SCardGetDeviceTypeIdW` | Original: 8 bytes (ReturnCode + 0x0020) → el remoto leía `dwDeviceId=0`. Fix: 4 bytes | Medio — causaba flood de re-consulta |
| 7 | **GET_READERICON** | Body = `cbDataLen` + datos (o error `SCARD_E_UNSUPPORTED_FEATURE`) | Original: SUCCESS sin body. Fix: empaqueta el return | Bajo |
| 8 | **CONNECT protocolo** | `SCARD_PROTOCOL_UNDEFINED` (0) → mapea a `SCARD_PROTOCOL_Tx` (T0\|T1) | `u32_to_protocols(0)` → `Protocols::ANY` | Bajo |
| 9 | **Handles (hContext/hCard)** | SCARDCONTEXT/SCARDHANDLE reales del OS (con la codificación `0xCD`/`0xEA` de FreeRDP) | Contadores locales (1, 2, 3...) | Bajo — el remoto los trata como opacos |
| 10 | **Threading** | Multi-hilo: GSC y ops bloqueantes en hilos por contexto (asyncIrp); el dispatch de IRP no se bloquea | **Un único hilo de device** (`device_thread_main`), TODO síncrono; GSC bloquea | Alto — ver abajo |
| 11 | **CMAPFILE/cardid/cardcf** | Lee de la tarjeta vía SCardTransmit (lo mismo que nosotros) | Igual | Ninguna — los datos coinciden byte a byte |
| 12 | **Certificado** | Se muestra porque msclmd lo lee tras abrir el contenedor (vía cache) | No se llega a leer porque el contenedor no abre | Consecuencia de #1/#2 |

---

## Análisis Funcion por Funcion

### CardAcquireContext (fase de descubrimiento)

**Nativo**: `SCardEstablishContext` → el minidriver (msclmd) hace la secuencia de APDUs: SELECT PIV (6A82), SELECT MF, SELECT AID GIDS, GET DATA 2F01 (EF.ATR), GET DATA 7F73 (PUK → 6A88), GET DATA DF1F (cardapps), GET DATA DF20 A012 (cardid), GET DATA DF22 (cardcf). **NO lee DF23 (cmapfile) ni 7F49** — esos vienen del OS cache.

**Nuestro**: misma secuencia de descubrimiento, PERO además lee `DF23` (cmapfile, 91 bytes) y `7F49` (pubkey, 272 bytes) cuando `ContainerInfo_00` falla en el cache. El 7F49 es el que falla.

**Diferencia real**: el descubrimiento base es idéntico. La divergencia aparece cuando msclmd necesita la info del contenedor: nativo la saca del cache, nosotros intentamos leerla de la tarjeta.

### CardGetContainerInfo (el punto que falla)

**Nativo**: `ContainerInfo_00` → `SCardReadCacheW` → **HIT** (308 bytes: header CSP + CONTAINER_INFO con el blob RSA). msclmd construye el contenedor desde el cache. No se emite ningún APDU.

**Nuestro**: `ContainerInfo_00` → MISS (cache vacío) → msclmd emite `GET DATA DF20` (PIN id) + `GET DATA 7F49` (pubkey, 272 bytes). El 7F49 no puede entregarse correctamente por el wire (ver #2) → `CardGetContainerInfo` falla → contenedor `(null)`.

### GET_STATUS_CHANGE

**Nativo** (smartcard_call.c GetStatusChangeW_Call):
```c
for (UINT32 x = 0; x < MAX(1, dwTimeOut);) {
    ret.ReturnCode = SCardGetStatusChangeW(hContext, MIN(dwTimeOut, 100), rgReaderStates, cReaders);
    if (ret.ReturnCode != SCARD_E_TIMEOUT) break;
    if (aborted) break;
    if (dwTimeOut != INFINITE) x += 100;
}
// copia los states de vuelta (VERBATIM) y los empaqueta
```
- Async: el IOCTL se marca `asyncIrp = TRUE` y se procesa en el hilo del contexto, no bloquea el dispatch principal.
- En timeout devuelve `SCARD_E_TIMEOUT` con los states empaquetados.

**Nuestro** (handlers.rs handle_get_status_change + native/mod.rs):
- Síncrono en el único hilo de device.
- Original: pasaba el timeout completo a pcsc (INFINITE → 24h), bloqueando el hilo; en timeout devolvía `SCARD_E_TIMEOUT` con **body vacío**.
- Fixes aplicados: paso de 100ms (`timeout.min(100ms)`) y empaquetar los states con el return code.

**Diferencia pendiente**: el nativo es async (no bloquea); nosotros seguimos siendo síncronos (bloqueamos el hilo hasta 100ms por GSC). Esto puede degradar el flujo cuando el remoto envía GSC(INFINITE) durante el descubrimiento.

### Transmit

**Nativo** (Transmit_Call):
```c
if (call->cbRecvLength && !call->fpbRecvBufferIsNULL) {
    if (call->cbRecvLength >= 66560) call->cbRecvLength = 66560;
    ret.cbRecvLength = call->cbRecvLength;   // buffer del caller (258)
    ret.pbRecvBuffer = malloc(ret.cbRecvLength);
}
ret.pioRecvPci = call->pioRecvPci;
ret.ReturnCode = SCardTransmit(hCard, call->pioSendPci, ..., ret.pioRecvPci, ret.pbRecvBuffer, &ret.cbRecvLength);
```
- Usa el buffer del caller como capacidad; Windows trunca a `cbRecvLength` y devuelve SUCCESS con la longitud truncada.
- `pioRecvPci` de la respuesta = NULL (verificado empíricamente).

**Nuestro**: el backend pcsc usa un buffer de `SCARD_TRANSMIT_MAX` (66560) y devuelve el dato completo. En la respuesta pasamos `cbRecvLength` = longitud real y `pioRecvPci` = NULL.

**Diferencia**: el nativo respeta el buffer del caller (trunca); nosotros devolvemos el dato completo. Para respuestas ≤258 es idéntico. Para respuestas >258 (el 7F49), el nativo "trunca" — pero el nativo **nunca lee el 7F49** (cache), así que esa ruta no está probada en el nativo. Es un comportamiento desconocido.

### GET_DEVICE_TYPE_ID / GET_READERICON

**Nativo**: `GetDeviceTypeId_Return` body = solo `dwDeviceId` (4 bytes); ReturnCode en el header Result. `GetReaderIcon_Return` = `cbDataLen` + datos.

**Nuestro (original)**: GET_DEVICE_TYPE_ID escribía 8 bytes (ReturnCode + 0x0020) → el remoto leía `dwDeviceId=0` → flood de re-consulta. GET_READERICON devolvía SUCCESS sin body. Ambos corregidos para coincidir con el nativo.

### Cache

**Nativo**: `SCardReadCacheW(hContext, CardIdentifier, FreshnessCounter, LookupName, ...)`. El hContext es el SCARDCONTEXT real del OS (establecido en SCardEstablishContext). El cache es persistente por tarjeta y scope.

**Nuestro (original)**: `HashMap<Vec<u16>, Vec<u8>>` global, key = LookupName, READ siempre NOT_FOUND (no servía lo escrito). Fix: HIT + delegación al OS cache (estableciendo un contexto SYSTEM en cada llamada).

**Diferencia**: la delegación al OS cache replica el nativo. Pero depende de que el OS cache esté poblado (sesiones previas). En un sistema limpio, ambos fallarían igual al no poder entregar el 7F49.

---

## Sincrono vs Asíncrono

| IOCTL | Nativo | Nuestro | Nota |
|-------|--------|---------|------|
| GET_STATUS_CHANGE | **Async** (hilo por contexto, IrpQueue) | **Sync** (hilo único de device) | El nativo no bloquea el dispatch; nosotros sí (hasta 100ms/iteración) |
| GET_STATUS_CHANGE (INFINITE) | Se queda pendiente en el hilo del contexto, sin bloquear nada más | Bloquea el hilo de device (ahora con paso de 100ms) | Si el remoto envía INFINITE durante el descubrimiento, el nativo sigue atendiendo transmits; nosotros los encolamos |
| LIST_READERS / CONNECT / TRANSMIT | Procesados en el hilo del device service (RDPDR) | Procesados en el mismo hilo que GSC | — |
| CANCEL | Cancela el GSC pendiente en el hilo del contexto | Devuelve OK sin cancelar nada real | — |

**La diferencia de threading es la más sospechosa de degradar el flujo**: el nativo puede atender `GET_STATUS_CHANGE` (bloqueante) y `TRANSMIT` concurrentemente; nosotros serializamos todo en un hilo, y un GSC bloqueado retrasa los transmits.

---

## PDU-Level (para revisión posterior)

Pendiente: tras este análisis, revisar a nivel de PDU la secuencia exacta de IOCTLs y APDUs que cada canal emite para el flujo de contenedor, y comparar:

1. Secuencia de IOCTLs (ya verificada idéntica en los primeros 100).
2. Respuestas de cada IOCTL (formato de wire) — ya auditadas para transmit/cache/GSC/GDTID/GERI.
3. Los puntos donde el nativo "no hace nada" (no lee cmapfile/7F49) y nosotros sí.
4. La decisión de diseño: ¿populamos el cache del contenedor nosotros (leer el 7F49 internamente con buffer grande y construir ContainerInfo_00), o usamos el OS cache como el nativo?
