# ISO: Upgrade de FreeRDP a 3.3x (trabajo futuro, NO urgente)

> Fecha: 2026-10-25. Contexto: m4 (macOS, Apple Silicon).

## Estado actual (lo que funciona HOY)

- macOS construye contra **FreeRDP 3.26.0** de brew:
  - tap local `/opt/homebrew/Library/Taps/dkmaster/homebrew-local` (fórmula del
    histórico de homebrew-core @ `081e067eb6b1`)
  - instalado **con `-s` (build from source)** contra el ffmpeg 9 actual
    (enlaza `libswscale.10` / `libavcodec.63`) y con `brew pin freerdp`
  - reponer en m4: `brew install -s dkmaster/local/freerdp && brew pin freerdp`
- Este repo (`freerdp-integration`) en `master` @ `9f14213`.
- `uds-client` (m4) con `Cargo.lock` actualizado a `9f14213` (sin commit).

## Por qué NO subir todavía

Con freerdp ≥ 3.27 (brew da 3.32.2 hoy) el crate `rdp` de ESTE repo no compila:

1. `WLOG_FATAL/ERROR/WARN/INFO/DEBUG/TRACE/WLOG_APPENDER_CALLBACK` eliminados
   de los headers → `rdp/src/wlog.rs`.
2. `RAIL_EXEC_FLAG_EXPAND_ARGUMENTS` eliminado → `rdp/src/channels/rail.rs`.
3. `SaveSessionInfo` ahora es `*const c_void` (antes `*mut`) →
   `rdp/src/callbacks/update_c.rs`.

(Recuerdo del cliente: "por encima de la 3.26 ya dio problemas". Confirmado.)

## Plan cuando toque (rama futura, alinear las 3 plataformas)

1. Adaptar el crate `rdp` a la API 3.3x (los 3 puntos + lo que surja).
2. Subir la versión de freerdp en TODAS las plataformas a la vez
   (macOS tap/formula, Linux CI/paquetes, Windows).
3. En macOS: `brew unpin freerdp` + fórmula nueva del histórico de
   homebrew-core (brew va en modo API: no hay tap de core; bajar el `.rb`
   del commit correspondiente a un tap local, ej. `dkmaster/local`) e
   instalar SIEMPRE con `-s`: sin `-s` brew cuela la botella vieja
   (enlazada a un ffmpeg antiguo) y el empaquetado muere con
   `libswscale.9 not found`.
4. En uds-client: `cargo update -p rdp` + commit del `Cargo.lock`.
5. Verificar el flujo: `build-pkg.py` con hooks `UDS_PROCESS_BINARY/APP/PACKAGE`
   (nombres actuales; los `UDSACTOR_*` son de copias antiguas).

## Responsable y modalidad

- Lo ejecuta **dkmaster personalmente**, en **rama aparte y como prueba
  independiente** — NO los padawans, para que ellos lleven producción sin
  sobresaltos. El contrato público del crate `rdp` (v5.0.0) aísla a
  uds-client: un cambio de freerdp debajo no se nota fuera de este repo.
