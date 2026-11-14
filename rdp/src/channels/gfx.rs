// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::context::OwnerFromCtx;
use crate::utils;
use crate::utils::log;
use freerdp_sys::*;

#[derive(Clone, Debug)]
pub struct GfxChannel {
    ptr: Option<utils::SafePtr<freerdp_sys::RdpgfxClientContext>>,
}

extern "C" fn update_window_from_surface(
    context: *mut RdpgfxClientContext,
    surface: *mut gdiGfxSurface,
) -> UINT {
    unsafe {
        let gdi = (*context).custom as *mut rdpGdi;
        let rdp_context = (*gdi).context;
        if let Some(owner) = rdp_context.owner() {
            let mut nb_rects = 0;
            let _rects = region16_rects(&(*surface).invalidRegion, &mut nb_rects);
            if nb_rects == 0 {
                return CHANNEL_RC_OK;
            }

            let width = (*surface).width;
            let height = (*surface).height;
            let window_id = (*surface).windowId as u32;

            let mut mapped_width = (*surface).mappedWidth;
            let mut mapped_height = (*surface).mappedHeight;

            if mapped_width == 0 {
                mapped_width = width;
            }
            if mapped_height == 0 {
                mapped_height = height;
            }

            if mapped_width == 0 || mapped_height == 0 || (*surface).data.is_null() {
                return CHANNEL_RC_OK;
            }

            // Validate host-supplied geometry before any allocation/copy:
            // mapped dims must not exceed the created surface dims, and all
            // size arithmetic must be widening/checked (host u32s can wrap a
            // u32 multiply to 0 or a small value, then freerdp_image_copy
            // writes past the undersized buffer). Reject the frame otherwise.
            if mapped_width > width || mapped_height > height {
                log::warn!(
                    "GFX: mapped dims {}x{} exceed surface {}x{} for window {}, dropping frame",
                    mapped_width,
                    mapped_height,
                    width,
                    height,
                    window_id
                );
                return CHANNEL_RC_OK;
            }
            let copy_w = mapped_width.min(width) as usize;
            let copy_h = mapped_height.min(height) as usize;
            let dst_step = match copy_w.checked_mul(4) {
                Some(s) if s <= u32::MAX as usize => s as u32,
                _ => {
                    log::warn!(
                        "GFX: dst stride overflow for window {}, dropping frame",
                        window_id
                    );
                    return CHANNEL_RC_OK;
                }
            };
            let buf_size = match (copy_w as u64)
                .checked_mul(copy_h as u64)
                .and_then(|v| v.checked_mul(4))
            {
                Some(s) if s <= usize::MAX as u64 => s as usize,
                _ => {
                    log::warn!(
                        "GFX: buffer size overflow for window {}, dropping frame",
                        window_id
                    );
                    return CHANNEL_RC_OK;
                }
            };

            if mapped_width != width || mapped_height != height {
                log::trace!(
                    "GFX Surface size mismatch for window {}: surface={}x{}, mapped={}x{}",
                    window_id,
                    width,
                    height,
                    mapped_width,
                    mapped_height
                );
            }

            // Ensure we don't try to copy more than what we have in the surface
            // freerdp_image_copy doesn't scale, it just copies pixels.
            // If mapped size is different, it usually means scaling is handled elsewhere
            // or we should be using surface size for the copy.
            let copy_width = mapped_width.min(width);
            let copy_height = mapped_height.min(height);

            let mut data = vec![0u8; buf_size];
            let format = if owner.use_rgba() {
                utils::pixel_format(32, 3, 8, 8, 8, 8) // RGBA32 (macOS)
            } else {
                utils::pixel_format(32, 4, 8, 8, 8, 8) // BGRA32 (Windows/Linux)
            };

            #[allow(clippy::unnecessary_cast)]
            // Needed beceuse windows/linux differ in the expected type of the flags parameter
            let _res = freerdp_image_copy(
                data.as_mut_ptr(),
                format,
                dst_step,
                0,
                0,
                copy_width,
                copy_height,
                (*surface).data,
                (*surface).format,
                (*surface).scanline,
                0,
                0,
                &(*gdi).palette,
                FREERDP_IMAGE_FLAGS_FREERDP_FLIP_NONE as u32,
            );

            // No swapping loop needed as we copy directly to the platform's native format

            if let Some(tx) = &owner.update_tx {
                // log::debug!("GFX sending WindowPixels for id={}, {}x{}", window_id, mapped_width, mapped_height);
                let _ = tx.try_send(crate::messaging::RdpMessage::WindowPixels {
                    window_id,
                    width: copy_w as u32,
                    height: copy_h as u32,
                    data,
                });
            }

            // Clear invalid region
            region16_clear(&mut (*surface).invalidRegion);
        }
        CHANNEL_RC_OK
    }
}

impl GfxChannel {
    pub fn new(ptr: *mut freerdp_sys::RdpgfxClientContext) -> Self {
        Self {
            ptr: utils::SafePtr::new(ptr),
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that `gdi` is a valid pointer to an `rdpGdi` structure.
    /// The caller must also ensure that the `GfxChannel` is valid and that the
    /// `rdpGdi` structure is properly initialized.
    ///
    /// Hooks the Graphics Pipeline into the GDI drawing engine.
    /// This handles drawing GFX frames into the GDI surface and
    /// automatically sends frame acknowledgments for flow control.
    pub unsafe fn hook_gdi(&self, gdi: *mut rdpGdi) -> bool {
        if let Some(ptr) = &self.ptr {
            log::debug!("GFX: Hooking GDI pipeline");
            let context = ptr.as_mut_ptr();
            unsafe {
                if gdi_graphics_pipeline_init(gdi, context) != 0 {
                    log::info!("GFX: Graphics pipeline integrated with GDI.");
                    let rdp_context = (*gdi).context;
                    let use_individual_windows = if let Some(owner) = rdp_context.owner() {
                        owner.config.settings.rail.as_ref().map(|r| r.behavior)
                            == Some(crate::settings::RailBehavior::IndividualWindows)
                    } else {
                        false
                    };
                    if use_individual_windows {
                        (*context).UpdateWindowFromSurface = Some(update_window_from_surface);
                    } else {
                        (*context).UpdateWindowFromSurface = None;
                    }
                    return true;
                }
            }
        }
        log::error!("GFX: Failed to hook GDI pipeline.");
        false
    }
}

#[cfg(test)]
mod tests {
    //! Characterization + overflow guarantee (Fix I) for
    //! update_window_from_surface. The host controls mappedWidth/mappedHeight
    //! (u32); a wrapping u32 multiply previously sized the copy buffer to 0 or
    //! a small value while freerdp_image_copy wrote the full surface -> heap
    //! overflow. These tests drive the real callback with a fake surface and
    //! a real Rdp owner, observing the WindowPixels message it emits.
    use super::*;
    use crate::context::RdpContext;
    use crate::messaging::RdpMessage;

    struct Fixture {
        rdp: *mut crate::Rdp,
        ctx: *mut RdpContext,
        gdi: *mut rdpGdi,
        gfx_ctx: *mut RdpgfxClientContext,
        update_rx: flume::Receiver<RdpMessage>,
    }

    impl Fixture {
        fn new() -> Self {
            let integrations = crate::integrations::RdpIntegrations {
                audio_output: None,
                audio_input: None,
                webcam: None,
                clipboard: None,
                smartcard: None,
                printer: None,
            };
            let (update_tx, update_rx) = flume::unbounded();
            let (rdp, _cmd) = crate::Rdp::new(
                crate::settings::RdpSettings::default(),
                update_tx,
                true,
                None,
                integrations,
            );
            let rdp = Box::into_raw(Box::new(rdp));
            let ctx = Box::into_raw(Box::new(RdpContext {
                common: unsafe { std::mem::zeroed() },
                owner: rdp,
            }));
            // gdi.context -> RdpContext.common.context (rdpContext)
            let gdi = Box::into_raw(Box::new(unsafe { std::mem::zeroed::<rdpGdi>() }));
            unsafe {
                (*gdi).context = ctx as *mut freerdp_sys::rdpContext;
            }
            let gfx_ctx = Box::into_raw(Box::new(unsafe {
                std::mem::zeroed::<RdpgfxClientContext>()
            }));
            unsafe {
                (*gfx_ctx).custom = gdi as *mut std::ffi::c_void;
            }
            Self {
                rdp,
                ctx,
                gdi,
                gfx_ctx,
                update_rx,
            }
        }

        /// Build a fake surface with a real pixel buffer and one invalid rect.
        fn surface(
            &self,
            width: u32,
            height: u32,
            mapped_w: u32,
            mapped_h: u32,
        ) -> (*mut gdiGfxSurface, Vec<u8>) {
            let scanline = width * 4;
            let data = vec![0xABu8; (scanline as usize) * (height as usize).max(1)];
            let surf = Box::into_raw(Box::new(unsafe { std::mem::zeroed::<gdiGfxSurface>() }));
            unsafe {
                (*surf).width = width;
                (*surf).height = height;
                (*surf).mappedWidth = mapped_w;
                (*surf).mappedHeight = mapped_h;
                (*surf).scanline = scanline;
                (*surf).format = utils::pixel_format(32, 4, 8, 8, 8, 8); // BGRA32
                (*surf).data = data.as_ptr() as *mut BYTE;
                (*surf).windowId = 42;
                region16_init(&mut (*surf).invalidRegion);
                let rect = RECTANGLE_16 {
                    left: 0,
                    top: 0,
                    right: width.min(u16::MAX as u32) as u16,
                    bottom: height.min(u16::MAX as u32) as u16,
                };
                region16_union_rect(&mut (*surf).invalidRegion, &(*surf).invalidRegion, &rect);
            }
            (surf, data)
        }

        fn drain_pixels(&self) -> Vec<(u32, u32, usize)> {
            let mut out = Vec::new();
            while let Ok(msg) = self.update_rx.try_recv() {
                if let RdpMessage::WindowPixels {
                    width,
                    height,
                    data,
                    ..
                } = msg
                {
                    out.push((width, height, data.len()));
                }
            }
            out
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                let _ = Box::from_raw(self.gfx_ctx);
                let _ = Box::from_raw(self.gdi);
                let _ = Box::from_raw(self.ctx);
                let _ = Box::from_raw(self.rdp);
            }
        }
    }

    #[test]
    fn normal_surface_emits_correctly_sized_pixels() {
        let f = Fixture::new();
        let (surf, _data) = f.surface(64, 48, 64, 48);
        let rc = update_window_from_surface(f.gfx_ctx, surf);
        assert_eq!(rc, CHANNEL_RC_OK);
        let msgs = f.drain_pixels();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, 64);
        assert_eq!(msgs[0].1, 48);
        assert_eq!(msgs[0].2, 64 * 48 * 4);
        unsafe {
            region16_uninit(&mut (*surf).invalidRegion);
            let _ = Box::from_raw(surf);
        }
    }

    #[test]
    fn mapped_exceeding_surface_is_dropped() {
        let f = Fixture::new();
        // Host advertises mapped dims larger than the created surface.
        let (surf, _data) = f.surface(64, 48, 1000, 1000);
        let rc = update_window_from_surface(f.gfx_ctx, surf);
        assert_eq!(rc, CHANNEL_RC_OK);
        assert!(
            f.drain_pixels().is_empty(),
            "oversize mapped dims must be dropped"
        );
        unsafe {
            region16_uninit(&mut (*surf).invalidRegion);
            let _ = Box::from_raw(surf);
        }
    }

    #[test]
    fn wrapping_mapped_dims_do_not_overflow() {
        let f = Fixture::new();
        // mappedWidth * mappedHeight * 4 wraps u32 to a small/zero value.
        // 0x20000 * 0x20000 * 4 = 0x4_0000_0000 -> wraps to 0 in u32.
        let (surf, _data) = f.surface(64, 48, 0x20000, 0x20000);
        let rc = update_window_from_surface(f.gfx_ctx, surf);
        assert_eq!(rc, CHANNEL_RC_OK);
        // Must be dropped (mapped > surface), NOT a 0-byte buffer overflow.
        assert!(f.drain_pixels().is_empty());
        unsafe {
            region16_uninit(&mut (*surf).invalidRegion);
            let _ = Box::from_raw(surf);
        }
    }

    #[test]
    fn zero_mapped_falls_back_to_surface_dims() {
        let f = Fixture::new();
        let (surf, _data) = f.surface(32, 16, 0, 0);
        let rc = update_window_from_surface(f.gfx_ctx, surf);
        assert_eq!(rc, CHANNEL_RC_OK);
        let msgs = f.drain_pixels();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, 32);
        assert_eq!(msgs[0].1, 16);
        assert_eq!(msgs[0].2, 32 * 16 * 4);
        unsafe {
            region16_uninit(&mut (*surf).invalidRegion);
            let _ = Box::from_raw(surf);
        }
    }
}
