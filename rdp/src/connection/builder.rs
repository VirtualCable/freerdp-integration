// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use std::ffi::CString;

use anyhow::Result;
use freerdp_sys::*;

use crate::utils::log;
use crate::{callbacks::instance_c, utils::SafePtr};

use super::{Rdp, context::RdpContext};

impl Rdp {
    pub fn build(self: std::pin::Pin<&mut Self>) -> Result<()> {
        log::debug!("Building RDP connection... {:p}", self);
        let mut_self = unsafe { self.get_unchecked_mut() };

        unsafe {
            let ctx = RdpContext::create(mut_self)?;
            let instance = (*ctx).common.context.instance;

            mut_self.instance = Some(SafePtr::new(instance).ok_or_else(|| {
                anyhow::anyhow!(
                    "Failed to create SafePtr for freerdp instance: {:?}",
                    instance
                )
            })?);

            instance_c::set_instance_callbacks(instance);

            let settings_ptr = (*ctx).common.context.settings;

            let host = CString::new(mut_self.config.settings.server.as_str())?;
            let user = CString::new(mut_self.config.settings.user.as_str())?;
            let mut pass = CString::new(mut_self.config.settings.password.as_str())?;
            let domain = CString::new(mut_self.config.settings.domain.as_str())?;

            freerdp_settings_set_string(
                settings_ptr,
                FreeRDP_Settings_Keys_String_FreeRDP_ServerHostname,
                host.as_ptr(),
            );
            freerdp_settings_set_string(
                settings_ptr,
                FreeRDP_Settings_Keys_String_FreeRDP_Username,
                user.as_ptr(),
            );
            freerdp_settings_set_string(
                settings_ptr,
                FreeRDP_Settings_Keys_String_FreeRDP_Password,
                pass.as_ptr(),
            );
            freerdp_settings_set_string(
                settings_ptr,
                FreeRDP_Settings_Keys_String_FreeRDP_Domain,
                domain.as_ptr(),
            );

            // Zeroize sensitive data as soon as it is passed to FreeRDP
            crate::utils::zeroize_cstring(&mut pass);

            freerdp_settings_set_uint32(
                settings_ptr,
                FreeRDP_Settings_Keys_UInt32_FreeRDP_ServerPort,
                mut_self.config.settings.port,
            );
            Ok(())
        }
    }
}
