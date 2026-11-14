// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use freerdp_sys::*;

use crate::utils::log;

use crate::callbacks::update;

use super::{Rdp, RdpMessage};

/// Hard upper bound for the host-negotiated desktop geometry. The host
/// advertises u16 width/height (up to 65535) in the Bitmap Capability Set;
/// upstream copies them into settings unvalidated and gdi_resize would
/// allocate w*h*4 bytes (~17 GB at 65535²) before any consumer gate. 4096²
/// (~64 MB primary buffer) is a sane ceiling for today's sessions; anything
/// larger is rejected so upstream aborts the activation path. Not
/// configurable by design — if a consumer genuinely needs more, raise this.
pub(crate) const MAX_DESKTOP_DIM: u32 = 4096;

/// Validate host-negotiated desktop dimensions against the hard bound.
/// Returns `Some((w, h))` when both are in `1..=MAX_DESKTOP_DIM`, else `None`
/// (the caller must abort the resize / activation). Pure: unit-testable
/// without a FreeRDP connection.
pub(crate) fn clamp_desktop_dims(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || width > MAX_DESKTOP_DIM || height > MAX_DESKTOP_DIM {
        None
    } else {
        Some((width, height))
    }
}

impl update::UpdateCallbacks for Rdp {
    fn on_begin_paint(&mut self) -> bool {
        true
    }

    fn on_end_paint(&mut self) -> bool {
        log::trace!("on_end_paint called");
        self.send_update()
    }

    fn on_desktop_resize(&mut self) -> bool {
        log::debug!(" **** Desktop resized");
        if let Some(settings) = self.settings() {
            let width = unsafe {
                freerdp_settings_get_uint32(
                    settings,
                    FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopWidth,
                )
            };
            let height = unsafe {
                freerdp_settings_get_uint32(
                    settings,
                    FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopHeight,
                )
            };
            // Reject out-of-bounds host geometry BEFORE any GDI reallocation:
            // returning false makes upstream abort the activation path
            // instead of allocating a multi-GB primary buffer.
            let (width, height) = match clamp_desktop_dims(width, height) {
                Some(dims) => dims,
                None => {
                    log::error!(
                        " **** Desktop resize rejected: host geometry {}x{} exceeds \
                         the {}x{} bound",
                        width,
                        height,
                        MAX_DESKTOP_DIM,
                        MAX_DESKTOP_DIM
                    );
                    return false;
                }
            };
            let gdi_lock = self.gdi_lock();
            let _gdi_guard = gdi_lock.write().unwrap();
            if let Some(gdi) = self.gdi() {
                let ok = unsafe { gdi_resize(gdi, width, height) };
                if ok == 0 {
                    log::error!(" **** gdi_resize failed for {}x{}", width, height);
                    return false;
                }
            }
            // If update_tx is present, notify it of the resize
            // with try_send, so it doesn't block if the gui thread is not ready
            if let Some(tx) = &self.update_tx {
                let _ = tx.try_send(RdpMessage::DesktopResize(width, height));
            }
            true
        } else {
            log::debug!("No settings found");
            false
        }
    }
}

impl Rdp {
    fn send_update(&self) -> bool {
        log::trace!("send_update called");

        let is_individual_windows = self.config.settings.rail.as_ref().map(|r| r.behavior)
            == Some(crate::settings::RailBehavior::IndividualWindows);
        if is_individual_windows {
            // Skip GDI updates when in IndividualWindows mode (Mode B)
            return true;
        }

        if let Some(tx) = &self.update_tx
            && let Some(gdi) = self.gdi()
        {
            unsafe {
                // CRITICAL: Use gdi->primary->hdc->hwnd->invalid,
                // NOT gdi->drawing. The GFX pipeline writes to primary and sets
                // its invalidation region, but 'drawing' may point elsewhere.
                let primary = (*gdi).primary;
                if primary.is_null() {
                    return true;
                }
                let hdc = (*primary).hdc;
                if hdc.is_null() || (*hdc).hwnd.is_null() {
                    return true;
                }

                let hwnd = (*hdc).hwnd;
                let rgn = (*hwnd).invalid;
                let mut ninvalid = (*hwnd).ninvalid;

                // Sanity check: limit the number of invalid rectangles to prevent DoS/OOB
                if ninvalid > 256 {
                    log::warn!(
                        "RAIL: Too many invalid rectangles: {}, capping to 256",
                        ninvalid
                    );
                    ninvalid = 256;
                }

                #[allow(clippy::unnecessary_cast)]
                // Needed beceuse windows/linux differ in the expected type of the flags parameter
                if !rgn.is_null() && ((*rgn).null == 0 || ninvalid > 0) {
                    let mut rects = vec![];
                    if (*rgn).null == 0 {
                        let r = &*rgn;
                        // Basic dimension check
                        if r.w > 0 && r.h > 0 && r.w < 16384 && r.h < 16384 {
                            rects.push(crate::geom::Rect::new(
                                r.x as i32, r.y as i32, r.w as u32, r.h as u32,
                            ));
                        }
                    }
                    if ninvalid > 0 {
                        let cinvalid = (*hwnd).cinvalid;
                        if !cinvalid.is_null() {
                            let slice = std::slice::from_raw_parts(cinvalid, ninvalid as usize);
                            for crgn in slice.iter() {
                                if crgn.null == 0
                                    && crgn.w > 0
                                    && crgn.h > 0
                                    && crgn.w < 16384
                                    && crgn.h < 16384
                                {
                                    rects.push(crate::geom::Rect::new(
                                        crgn.x as i32,
                                        crgn.y as i32,
                                        crgn.w as u32,
                                        crgn.h as u32,
                                    ));
                                }
                            }
                        }
                    }

                    if !rects.is_empty() {
                        // Use trace instead of debug
                        log::trace!("Sending UpdateRects: block, items: {}", rects.len());
                        let _ = tx.try_send(RdpMessage::UpdateRects(rects));
                    }

                    // Reset invalidation after sending, following Guacamole's pattern
                    (*rgn).null = 1;
                    (*hwnd).ninvalid = 0;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Characterization: the clamp decision (pure, no FreeRDP needed) ───

    #[test]
    fn accepts_typical_desktop_sizes() {
        for (w, h) in [(1024, 768), (1920, 1080), (3840, 2160), (4096, 4096)] {
            assert_eq!(clamp_desktop_dims(w, h), Some((w, h)));
        }
    }

    #[test]
    fn rejects_zero_dimensions() {
        assert_eq!(clamp_desktop_dims(0, 768), None);
        assert_eq!(clamp_desktop_dims(1024, 0), None);
        assert_eq!(clamp_desktop_dims(0, 0), None);
    }

    #[test]
    fn rejects_host_advertised_oversize() {
        // u16 max the host can advertise -> ~17 GB allocation upstream.
        assert_eq!(clamp_desktop_dims(65535, 65535), None);
        assert_eq!(clamp_desktop_dims(4097, 2160), None);
        assert_eq!(clamp_desktop_dims(3840, 4097), None);
        // 8k is beyond the sane ceiling today.
        assert_eq!(clamp_desktop_dims(7680, 4320), None);
    }

    #[test]
    fn boundary_is_inclusive() {
        assert_eq!(
            clamp_desktop_dims(MAX_DESKTOP_DIM, MAX_DESKTOP_DIM),
            Some((4096, 4096))
        );
        assert_eq!(clamp_desktop_dims(MAX_DESKTOP_DIM + 1, 1), None);
        assert_eq!(clamp_desktop_dims(1, MAX_DESKTOP_DIM + 1), None);
    }
}
