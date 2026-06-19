// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use freerdp_sys::*;

use crate::utils::log;

use crate::callbacks::update;

use super::{Rdp, RdpMessage};

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
            let gdi_lock = self.gdi_lock();
            let _gdi_guard = gdi_lock.write().unwrap();
            if let Some(gdi) = self.gdi() {
                unsafe { gdi_resize(gdi, width as u32, height as u32) };
            }
            // If update_tx is present, notify it of the resize
            // with try_send, so it doesn't block if the gui thread is not ready
            if let Some(tx) = &self.update_tx {
                let _ = tx.try_send(RdpMessage::DesktopResize(width as u32, height as u32));
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
