// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use freerdp_sys::{BOOL, freerdp, rdpContext};

use super::{super::context::OwnerFromCtx, entrypoint::EntrypointCallbacks};
use crate::context::RdpContext;
use crate::utils::log;

pub extern "C" fn client_global_init() -> BOOL {
    // We could do the WSA initialization here if needed
    log::debug!(" **** RDP client GLOBAL init called");
    super::super::init::initialize();
    true.into()
}

pub extern "C" fn client_global_uninit() {
    // Currently, we do not need any special handling here.
    log::debug!(" **** RDP client GLOBAL uninit called");
    super::super::init::uninitialize();
}

pub extern "C" fn client_new(instance: *mut freerdp, context: *mut rdpContext) -> BOOL {
    // Currently, we do not need any special handling here.
    // Note, here we do not have the owner initialized, just for future reference.
    let ctx = context as *mut RdpContext;
    log::debug!(
        " **** RDP client new instance created: {:?} -- {:?} ({:?})",
        instance,
        ctx,
        unsafe { (*ctx).owner }
    );
    true.into()
}

/// # Safety
///
/// This function is called by FreeRDP
/// when the client instance is being freed.
/// It should clean up any resources associated with the instance.
pub unsafe extern "C" fn client_free(_instance: *mut freerdp, _context: *mut rdpContext) {
    log::debug!(" **** RDP client free called");
}

pub extern "C" fn client_start(context: *mut rdpContext) -> ::std::os::raw::c_int {
    log::debug!(" **** RDP client start called with context: {:?}", context);
    if let Some(owner) = context.owner() {
        owner.client_start().into()
    } else {
        true.into()
    }
}

pub extern "C" fn client_stop(context: *mut rdpContext) -> ::std::os::raw::c_int {
    log::debug!(" **** RDP client stop called with context: {:?}", context);
    if let Some(owner) = context.owner() {
        owner.client_stop().into()
    } else {
        true.into()
    }
}
