# Diseño futuro: verificación de certificado RDP con ancla en el broker

> Nota de diseño (NO implementado). Registrada para no perderla durante el
> ciclo de fixes de seguridad de `freerdp-integration` (run-1, 2026-11).
> Relacionada con el Fix 1-J (callbacks de certificado fail-closed).

## Contexto

Hoy la semántica de `verify_cert` es honesta pero limitada:

- `verify_cert=false` (default): `IgnoreCertificate=true`, FreeRDP salta la
  verificación. Sin garantías criptográficas — la seguridad recae en la red
  privada (realidad de los despliegues VDI actuales).
- `verify_cert=true`: `IgnoreCertificate=false`, FreeRDP verifica de verdad
  contra la **CA del sistema** (`X509_STORE_set_default_paths`, p.ej. el bundle
  Debian `/etc/ssl/certs`) **+ match de nombre/IP** contra CN/DNS/IP SANs. Si la
  verificación no tiene éxito completo, el callback fail-closed rechaza.

Los certs self-signed de VDI (nombres/IPs generados dinámicamente) no pasan la
CA del sistema, así que `verify_cert=true` los rechazaría hasta que exista una
política real. Esta nota describe esa política.

## Idea

Sustituir "confiar en la CA pública" por **"confiar en el broker como anchor de
confianza"** (trust-on-broker-auth, no TOFU):

1. El broker ya emite un certificado al actor que corre en la máquina (canal
   autenticado con ticket/token).
2. Cuando un VDI pasa a *ready*, el servidor RDP usa **ese mismo cert**.
3. El cliente (p.ej. un instalable Windows que conecta al broker) le pide al
   broker "¿qué cert debe presentar este VDI ahora?" y lo recibe por el canal
   autenticado. El broker lo recuerda y lo entrega.
4. El callback de verificación del cliente **compara el cert presentado por el
   servidor RDP contra el que entregó el broker** (pin) y acepta solo si coincide.

La rotación la resuelve el broker: re-emite y el cliente re-consulta por sesión.
Es un PKI cerrado y limpio, no "aceptar sin más".

## Puntos de diseño

- **Qué pinear**: para VDI con rotación, o bien *fingerprint/cert completo
  consultado por sesión* (el broker es la fuente de verdad → rotación
  transparente), o *SPKI (clave pública)* si se quiere que sobreviva a
  re-emisiones con la misma clave. Dado que el broker recuerda y entrega el cert
  vigente, el pin por sesión es el más simple y correcto.
- **Flag `VERIFY_CERT_FLAG_CHANGED`**: con pin, un cert cambiado que no coincide
  con el del broker se rechaza (MitM detectado). Una rotación legítima requiere
  que el broker actualice el pin — comportamiento deseado.
- **IP vs nombre**: ya se emite con SAN cubriendo todas las IPs, así que el match
  de nombre/IP funciona. Con pin, el pin es la aserción fuerte; mantener además
  el match de SAN es defensa en profundidad.
- **Dónde vive**: trabajo de **consumidor + broker** (el instalable y la API del
  broker), NO de `freerdp-integration`. La librería solo expone el hook.

## Habilitador en la librería (cuando toque)

Ahora `Rdp` implementa `InstanceCallbacks` internamente, así que el consumidor no
puede inyectar su política de pin. El habilitador limpio sería un **verificador
opcional por setter**, p.ej.:

```rust
// trait público nuevo (nombre orientativo)
pub trait CertificateVerifier: Send + Sync {
    /// Decide si aceptar el cert presentado. `expected` es el pin que el
    /// consumidor obtuvo del broker (fingerprint/SPKI/PEM).
    fn verify(&self, presented_fingerprint: &str, host: &str, port: u16, flags: u32) -> bool;
}

impl Rdp {
    pub fn set_certificate_verifier(&self, verifier: Arc<dyn CertificateVerifier>);
}
```

- **NO** un campo nuevo en `RdpOptions`/`RdpIntegrations`: el consumidor
  (`rdphtml5`) los construye con literal sin `..Default::default()`, así que un
  campo rompería el principio de cero-cambios-en-consumidor seguido en los fixes
  A–J. Un setter evita tocar el literal.
- `on_verify_x509_certificate` / `on_verify_certificate` de `Rdp` consultarían el
  verificador si está presente; si no, mantienen el fail-closed actual (Fix 1-J).
- El verificador decide accept/reject; nunca accept-and-store (rc=1) por defecto.

## Estado

Pendiente. No bloquea ningún fix actual. El Fix 1-J ya deja la base segura
(fail-closed) sobre la que construir esto.