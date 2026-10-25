# Printer Redirection Architecture & Plan — freerdp-integration

> **Date:** 2026-10-25  
> **Status:** Architecture Proposal / Technical Roadmap  
> **Target Module:** `freerdp-integration` (shared generic crate between `uds-client` and `html5-rdp`)  
> **Companion Document in uds-client:** `doc/printer-redirection-plan.md`  
> **Incident Context:** `informe-impresion-rdp-integrado-windows.md`

---

## 1. Motivation: Full Control vs. Fragile Upstream Patching

Technical diagnostics in `informe-impresion-rdp-integrado-windows.md` confirmed fatal regressions in FreeRDP's native printer redirection (`channels/printer/client`):
* **Windows (`printer_win.c`)**: `printer_win_get_printjob_name` calls `swprintf_s(NULL, 0, ...)`, triggering a Microsoft CRT `__fastfail` (exception `0xC0000409`), instantly killing the client process.
* **Linux (`printer_cups.c`)**: CUPS print jobs stall indefinitely in `pending-held / job-incoming` with 0 KB received.

**Why Patching Upstream FreeRDP Is Insufficient:**
1. In Linux, distributions package `libfreerdp3` (e.g. Debian 13 / Ubuntu). Client packages dynamically link against system libraries where **patching is impossible**.
2. By intercepting the channel and moving it into **our own Rust domain**, we achieve **complete ownership**:
   * Works consistently regardless of the underlying FreeRDP system version.
   * Completely eliminates Windows CRT crashes.
   * Provides clean, non-blocking asynchronous job delivery on Linux and macOS.
   * Allows `html5-rdp` to intercept print streams and deliver PDF downloads to web browsers without touching C code.

---

## 2. Interception Mechanism: Dynamic Channels vs. Static Subsystem Hooks

In FreeRDP, channel subsystems fall into two distinct loading categories:

1. **Dynamic Virtual Channels (DVC)**:
   * Explicitly registered at connection configuration time via `freerdp_client_add_dynamic_channel(settings, ...)`.
   * Examples: `rdpecam` (webcam), `audin` (audio input), `disp`, `gfx`.
   * FreeRDP queries addin providers dynamically by channel name and subsystem tag (e.g. `"sys:custom"`).

2. **Static Channels and Built-in Subsystems ("Hooks")**:
   * Printer redirection is a sub-device under the **RDPDR** static virtual channel (MS-RDPEFS and MS-RDPEPC protocols).
   * It is compiled into FreeRDP and is **not** loaded as an external dynamic plugin.
   * When RDPDR's device manager (`devman`) initializes, FreeRDP internally calls:
     `freerdp_load_channel_addin_entry("printer", NULL, "DeviceServiceEntry", ...)`
   * **Our Hook Strategy:** In `context.rs`, we replace FreeRDP's global addin provider using `freerdp_register_addin_provider(Some(custom_addin_provider), 0)`.
     When FreeRDP requests `name == "printer"` and `psz_type == "DeviceServiceEntry"`, we intercept the query and return our own entry point:
     ```rust
     } else if name == "printer"
         && secure_cstr_from_lpcstr(psz_type) == "DeviceServiceEntry"
     {
         log::info!("printer DeviceServiceEntry intercepted via custom_addin_provider.");
         Some(std::mem::transmute::<
             unsafe extern "C" fn(PDEVICE_SERVICE_ENTRY_POINTS) -> UINT,
             unsafe extern "C" fn(*mut freerdp_sys::tagCHANNEL_ENTRY_POINTS) -> BOOL,
         >(printer::device_service_entry))
     }
     ```
   * By supplying our own `device_service_entry`, FreeRDP **never executes** `printer_main.c`, `printer_win.c`, or `printer_cups.c`, bypassing 100% of the buggy C implementation.

---

## 3. Architecture in `freerdp-integration`

```text
FreeRDP (RDPDR devman)
  │
  ▼
custom_addin_provider ("printer" / "DeviceServiceEntry")
  │
  ▼
addins/printer/mod.rs (device_service_entry)
  ├── 1. Recovers Rdp owner from rdpContext.
  ├── 2. Checks rdp.config.integrations.printer; exits gracefully if None or unavailable.
  ├── 3. Spawns decoupled worker thread backed by an unbounded flume channel.
  ├── 4. Registers device in devman:
  │      pEntryPoints->RegisterDevice(pEntryPoints->devman, &device)
  │      with type_ = RDPDR_DTYP_PRINT (0x00000004) and MS-RDPEPC announcement payload.
  │
  ▼
addins/printer/handlers.rs (IRPs dispatched on the worker thread)
  ├── IRP_MJ_CREATE           ──► integration.create_job(printer_name) -> FileId
  ├── IRP_MJ_WRITE            ──► integration.write_data(file_id, data) -> Length
  ├── IRP_MJ_DEVICE_CONTROL   ──► Printer IOCTLs (GetPrinterData, etc.)
  └── IRP_MJ_CLOSE            ──► integration.close_job(file_id)
  │
  ▼
rdp::integrations::PrinterIntegration (Agnostic Trait)
```

---

## 4. Proposed File Hierarchy

```text
freerdp-integration/
├── rdp/src/
│   ├── addins/
│   │   ├── addin.rs                   // Hook in custom_addin_provider for "printer"
│   │   └── printer/                   // Printer addin module
│   │       ├── mod.rs                 // device_service_entry, devman device registration
│   │       ├── consts.rs              // RDPDR_DTYP_PRINT and MS-RDPEPC constants
│   │       ├── device.rs              // Dedicated worker thread and IRP tracking
│   │       └── handlers.rs            // IRP_MJ_CREATE / WRITE / CLOSE handlers
│   └── integrations/
│       ├── mod.rs                     // pub printer: Option<Arc<dyn PrinterIntegration>>
│       └── printer.rs                 // PrinterIntegration trait & PrinterDeviceInfo
```

---

## 5. Platform-Agnostic Trait (`rdp::integrations::printer`)

The trait contains zero OS-specific dependencies, ensuring clean interoperability:

```rust
#[derive(Clone, Debug)]
pub struct PrinterDeviceInfo {
    pub id: u32,
    pub name: String,
    pub driver_name: String,      // e.g. "MS Publisher Imagesetter" or "Remote Desktop Easy Print"
    pub is_default: bool,
    pub port_name: Option<String>,
}

pub trait PrinterIntegration: Send + Sync + std::fmt::Debug {
    /// Returns whether the printer subsystem is active and available
    fn is_available(&self) -> bool;

    /// List of printers to announce over RDPDR (DEVICE_ANNOUNCE)
    fn list_printers(&self) -> Vec<PrinterDeviceInfo>;

    /// Initiates a new print job (IRP_MJ_CREATE)
    fn create_job(&self, printer_name: &str) -> Result<u32, u32>;

    /// Streams incoming document data (IRP_MJ_WRITE)
    fn write_data(&self, job_id: u32, data: &[u8]) -> Result<usize, u32>;

    /// Completes the print job successfully (IRP_MJ_CLOSE)
    fn close_job(&self, job_id: u32) -> Result<(), u32>;

    /// Aborts an in-flight print job on error or disconnection
    fn abort_job(&self, job_id: u32);
}
```

---

## 6. Compatibility with `html5-rdp`

In adherence to the core design principle: *"as little as possible, strict data hiding"*:
* `html5-rdp` can leave `printer: None`. When unconfigured, the intercepted addin simply exits with `CHANNEL_RC_OK` without registering devices into RDPDR.
* To support browser-based printing:
  * `html5-rdp` provides a web-oriented implementation of `PrinterIntegration`.
  * Document data is collected in memory or temporary storage during `write_data`.
  * Upon `close_job`, the server pushes the generated document (as PDF) via WebSocket to the browser client, triggering client-side download or `window.print()`.
