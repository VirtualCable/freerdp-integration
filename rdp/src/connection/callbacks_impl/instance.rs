// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::utils::log;
use std::ffi::CString;

use crate::callbacks::instance;
use freerdp_sys::{
    FreeRDP_Settings_Keys_String_FreeRDP_ServerHostname, freerdp_settings_set_string,
};

use super::Rdp;

// Sombra de S_H264_CONTEXT de FreeRDP para acceder a NumberOfThreads
// ya que el header h264.h no es público.
#[repr(C)]
struct H264ContextShadow {
    _compressor: i32,
    _width: u32,
    _height: u32,
    _rate_control_mode: u32,
    _bit_rate: u32,
    _frame_rate: u32,
    _qp: u32,
    _usage_type: u32,
    _hw_accel: u32,
    pub num_threads: u32,
}

impl instance::InstanceCallbacks for Rdp {
    fn on_post_connect(&mut self) -> bool {
        log::debug!(" **** Connected successfully!");

        // Limit FFmpeg threads to avoid the thread-per-core explosion
        if let Some(instance) = &self.instance {
            unsafe {
                let context = instance.context;
                if !context.is_null() && !(*context).codecs.is_null() {
                    let codecs = (*context).codecs;
                    let h264 = (*codecs).h264;
                    if !h264.is_null() {
                        log::debug!("Limiting FFmpeg decoder threads to 2 (via Shadow Struct)");
                        let h264_shadow = h264 as *mut H264ContextShadow;
                        (*h264_shadow).num_threads = 2;
                    }
                }
            }
        }
        true
    }

    fn on_redirect(&mut self) -> bool {
        log::debug!(" **** Redirecting!");
        // Override FreeRDP_ServerHostname with original hostname if tunnel flag is set
        if self.config.settings.options.use_tunnel
            && let Some(settings) = self.settings()
            && let Ok(host) = CString::new(self.config.settings.server.as_str())
        {
            log::debug!("Override FreeRDP_ServerHostname with original");
            unsafe {
                freerdp_settings_set_string(
                    settings,
                    FreeRDP_Settings_Keys_String_FreeRDP_ServerHostname,
                    host.as_ptr(),
                );
            }
        };
        true
    }
}
