# Análisis de Arquitectura: Windows Smart Card & Calais Database

> **Fecha:** 2026-06-25  
> **Objetivo:** Analizar sistemáticamente cómo Windows interactúa con la Smart Card redirigida a nivel de sistema operativo (Base CSP, KSP, Calais Database) para entender y resolver el error de `SCardGetCardTypeProviderName` y la enumeración de certificados.

---

## 1. La Arquitectura de Smart Cards en Windows

Windows gestiona las tarjetas inteligentes a través de una arquitectura de tres capas:

```
┌────────────────────────────────────────────────────────┐
│ Aplicación (certutil, Firefox, Edge, etc.)             │
└────────────────────────────────────────────────────────┘
                           │ CryptoAPI / CNG
                           ▼
┌────────────────────────────────────────────────────────┐
│ Base CSP (scardcsp.dll) / Smart Card KSP (scksp.dll)   │
└────────────────────────────────────────────────────────┘
                           │ Minidriver Interface
                           ▼
┌────────────────────────────────────────────────────────┐
│ Smart Card Minidriver (msclmd.dll para GIDS/PIV)       │
└────────────────────────────────────────────────────────┘
                           │ PC/SC API (winscard.dll)
                           ▼
┌────────────────────────────────────────────────────────┐
│ Smart Card Resource Manager (Calais / SCardSvr)        │
└────────────────────────────────────────────────────────┘
                           │ RDP Redirect
                           ▼
┌────────────────────────────────────────────────────────┐
│ Tarjeta Emulada (uds-client / freerdp-integration)     │
└────────────────────────────────────────────────────────┘
```

### El rol de la Calais Database (El registro de Windows)

El Resource Manager de Windows (`SCardSvr`) utiliza una base de datos interna llamada **Calais**, almacenada en el registro bajo:
`HKLM\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\`

Cuando una tarjeta es insertada, el Resource Manager lee su ATR y busca una coincidencia en esta base de datos para identificar el **Card Type** y asociarle un **Crypto Provider** (Minidriver/CSP).

---

## 2. Análisis del error `SCardGetCardTypeProviderName`

El error `SCardGetCardTypeProviderName: El sistema no puede encontrar el archivo especificado. 0x2 (ERROR_FILE_NOT_FOUND)` ocurre antes de que Windows intente interactuar con la tarjeta a nivel de APDUs para abrir las claves.

Este error significa que **Windows encuentra el ATR en la base de datos Calais, pero la configuración del proveedor asociada a esa tarjeta en el registro está rota o apunta a un archivo que no existe.**

### El conflicto del ATR GIDS / PIV

El ATR de FreeRDP GIDS (`3B F7 18 00 00 80 31 FE 45 73 66 74 65 2D 6E 66 C4`) coincide con dos entradas nativas de Windows:

1. **`Identity Device (Microsoft Generic Profile)`** — Perfil GIDS (Microsoft Smart Card Minidriver).
2. **`Identity Device (NIST SP 800-73 [PIV])`** — Perfil PIV (NIST PIV Minidriver).

Windows está asociando la tarjeta al perfil **PIV** (como muestra el output de `certutil`: `Tarjeta: Identity Device (NIST SP 800-73 [PIV])`).

Si la entrada de registro para el perfil PIV está rota o le falta el valor `80080001` (que apunta al minidriver), `SCardGetCardTypeProviderName` fallará inmediatamente con `ERROR_FILE_NOT_FOUND`, impidiendo que el CSP intente abrir las claves.

### Qué significa el valor `80080001`

En la Calais Database, cada tarjeta tiene valores binarios especiales que identifican sus proveedores:
* **`80080001`**: Corresponde al GUID `SCARD_GUID_PRIMARY_PROVIDER`. Identifica el tipo de minidriver y apunta a una clave en `HKLM\SOFTWARE\Microsoft\Cryptography\Defaults\Smart Card Minidriver\`.
* Si este valor está en `0` o está ausente, o apunta a un minidriver que no está instalado en el Windows, `certutil` fallará con `ERROR_FILE_NOT_FOUND`.

---

## 3. Plan de Acción y Diagnóstico

Para resolver esto sin dar "palos de ciego", debemos verificar exactamente el estado del registro en el Windows 10 de pruebas.

### Paso 1: Inspeccionar los perfiles nativos de Windows

Ejecuta estos comandos en una PowerShell de Windows como Administrador para ver qué tienen configurados los perfiles que coinciden con nuestro ATR:

```powershell
# 1. Ver propiedades del perfil GIDS nativo
Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\Identity Device (Microsoft Generic Profile)" -ErrorAction SilentlyContinue | Format-List *

# 2. Ver propiedades del perfil PIV nativo (el que Windows está seleccionando)
Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\Identity Device (NIST SP 800-73 [PIV])" -ErrorAction SilentlyContinue | Format-List *
```

**Qué buscar en el output:**
* El valor de `Crypto Provider` (debe ser `Microsoft Base Smart Card Crypto Provider`).
* El valor de `Smart Card Key Store Provider` (debe ser `Microsoft Smart Card Key Storage Provider`).
* El valor de `80080001` (debe ser un array de bytes largo, no `0`).

---

## 4. Estrategia de Solución: Forzar un ATR Custom de GIDS

Si el perfil PIV de Windows está roto o tiene prioridad sobre GIDS, la forma más limpia y robusta de probar es **definir un ATR único para nuestra tarjeta GIDS que no entre en conflicto con el perfil PIV nativo.**

### El plan de bypass:

1. **Definir un ATR Custom en nuestro código** (ej: `3B 04 00 00 00 00` — súper simple, no coincide con ninguna tarjeta del mundo).
2. **Registrar este ATR explícitamente como GIDS en el Windows remoto** usando un archivo `.reg` bien formado.
3. Al no coincidir con PIV, Windows se verá obligado a usar nuestra definición de GIDS y cargar el minidriver oficial (`msclmd.dll`).

### Valores del registro para el ATR Custom:

Creamos una entrada llamada `UDS GIDS Card` con el ATR custom:

```reg
Windows Registry Editor Version 5.00

[HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Cryptography\Calais\SmartCards\UDS GIDS Card]
"ATR"=hex:3b,04,00,00,00,00
"ATRMask"=hex:ff,ff,ff,ff,ff,ff
"Crypto Provider"="Microsoft Base Smart Card Crypto Provider"
"Smart Card Key Store Provider"="Microsoft Smart Card Key Storage Provider"
"80080001"=hex:53,00,6f,00,66,00,74,00,77,00,61,00,72,00,65,00,5c,00,4d,00,69,00,63,00,72,00,6f,00,73,00,6f,00,66,00,74,00,5c,00,43,00,72,00,79,00,70,00,74,00,6f,00,67,00,72,00,61,00,70,00,68,00,79,00,5c,00,44,00,65,00,66,00,61,00,75,00,6c,00,74,00,73,00,5c,00,53,00,6d,00,61,00,72,00,74,00,20,00,43,00,61,00,72,00,64,00,20,00,4d,00,69,00,6e,00,69,00,64,00,72,00,69,00,76,00,65,00,72,00,00,00
```

*(El valor de `80080001` es la cadena Unicode `Software\Microsoft\Cryptography\Defaults\Smart Card Minidriver` terminada en null, que es lo que el minidriver de Microsoft espera para ubicarse).*

---

## 5. Resumen del Flujo que esperamos ver en los Logs

Una vez que Windows resuelva el card type provider correctamente, el flujo de APDUs debe avanzar:

1. **SELECT GIDS AID** (`00 A4 04 00 ...`) → devuelve FCI `90 00`.
2. **GET DATA Filesystem** (`00 CB 2F 01 02 5C 00`) → devuelve la tabla de archivos `90 00`.
3. **GET DATA CMAPFile** (`00 CB 2F 01 02 5C 02 DF 23`) → Windows lee el mapa de contenedores para buscar `"Private Key 00"`.
4. **GET DATA KXC00** (`00 CB 2F 01 02 5C 02 DF 24`) → Windows lee el certificado en formato zlib.
5. **GET DATA Public Key** (`00 CB 3F FF ... 7F 49`) → Windows lee el módulo y exponente de la clave pública para verificar la consistencia con el certificado.
6. **MSE SET** (`00 22 41 B6 ...`) → configura el contexto de firma.
7. **VERIFY** (`00 20 00 80 ...`) → Windows pide el PIN si es necesario.
8. **PSO SIGN** (`00 2A 9E 9A ...`) → Windows realiza la firma digital.

Si el flujo se detiene en el paso 2, es porque el minidriver no puede continuar debido a que no interpreta bien la respuesta, o el registro local de Windows lo ha bloqueado.
