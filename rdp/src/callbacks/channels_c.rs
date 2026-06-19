// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

// Note that channels callbacks must be always implemented, we cannot disable them.

use freerdp_sys::{
    ChannelConnectedEventArgs, freerdp_client_OnChannelConnectedEventHandler,
    freerdp_client_OnChannelDisconnectedEventHandler, rdpContext,
};

use crate::utils::log;

use super::{super::context::OwnerFromCtx, channels::ChannelsCallbacks};
use crate::utils::ToStringLossy;

/// # Safety
/// This function is called from the FreeRDP C library when a channel is connected.
pub unsafe extern "C" fn on_channel_connected(
    context: *mut ::std::os::raw::c_void,
    e: *const ChannelConnectedEventArgs,
) {
    let context = context as *mut rdpContext;
    let size = unsafe { (*e).e.Size as usize };
    let sender = unsafe { (*e).e.Sender }.to_string_lossy();
    let name = unsafe { (*e).name }.to_string_lossy();
    let p_interface = unsafe { (*e).pInterface };

    log::debug!(
        "**** ChannelConnected Event: size={}, sender={}, name={}, pInterface={:?} (context={:?})",
        size,
        sender,
        name,
        p_interface,
        context
    );

    // Here we get for example the DISP_DVC_CHANNEL_NAME when the display virtual channel is connected.
    if let Some(rdp) = context.owner() {
        // Here we could notify the Rdp instance if needed.
        if rdp.on_channel_connected(size, &sender, &name, p_interface) {
            log::debug!("++++  {name} Channel connection accepted by Rdp instance.");
            return;
        } else {
            log::debug!("----  {name} Channel connection not processed by Rdp instance.");
        }
    } else {
        log::debug!("----  No Rdp instance found for channel connection event.");
    }

    unsafe {
        freerdp_client_OnChannelConnectedEventHandler(context as *mut _, e);
    }
}

/// # Safety
/// This function is called from the FreeRDP C library when a channel is disconnected.
pub unsafe extern "C" fn on_channel_disconnected(
    context: *mut ::std::os::raw::c_void,
    e: *const freerdp_sys::ChannelDisconnectedEventArgs,
) {
    let context: *mut freerdp_sys::rdpContext = context as *mut rdpContext;
    let size = unsafe { (*e).e.Size as usize };
    let sender = unsafe { (*e).e.Sender }.to_string_lossy();
    let name = unsafe { (*e).name }.to_string_lossy();
    let p_interface = unsafe { (*e).pInterface };

    log::debug!(
        "**** ChannelDisconnected Event: size={}, sender={}, name={}, pInterface={:?} (context={:?})",
        size,
        sender,
        name,
        p_interface,
        context
    );

    if let Some(rdp) = context.owner() {
        // Here we could notify the Rdp instance if needed.
        if rdp.on_channel_disconnected(size, &sender, &name, p_interface) {
            log::debug!("**** Channel disconnection accepted by Rdp instance.");
            // Do not return here, we still want to clean up if it's the graphics channel
        } else {
            log::debug!("**** Channel disconnection not accepted by Rdp instance.");
        }
    }

    unsafe {
        freerdp_client_OnChannelDisconnectedEventHandler(context as *mut _, e);
    }
}
