// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use anyhow::Result;

use freerdp_sys::*;

use crate::utils::log;

use super::{Rdp, addins::addin};
use crate::callbacks::entrypoint_c;

#[derive(Debug)]
#[repr(C)]
pub struct RdpContext {
    pub common: rdpClientContext,
    /// Back-pointer to the owning `Rdp` (set once at context creation).
    ///
    /// # Contract
    /// Valid only while the session is alive; the owner is dropped on teardown.
    /// Dereferencing it aliases the same `Rdp` exposed by
    /// [`Rdp::context`](crate::Rdp::context) — see [`OwnerFromCtx`] for the
    /// single-writer / no-overlapping-borrow requirement.
    pub owner: *mut Rdp,
}

impl RdpContext {
    pub fn context(&self) -> &rdpContext {
        &self.common.context
    }

    pub fn create(owner: &mut Rdp) -> Result<*mut Self> {
        const FREERDP_CLIENT_INTERFACE_VERSION: u32 = 1;

        debug_assert!(
            std::mem::size_of::<rdpClientContext>() + std::mem::size_of::<*mut Rdp>()
                == std::mem::size_of::<RdpContext>(),
            "Size mismatch between rdpClientContext and RdpContext"
        );

        let entry_points = rdp_client_entry_points_v1 {
            Size: std::mem::size_of::<rdp_client_entry_points_v1>() as u32,
            Version: FREERDP_CLIENT_INTERFACE_VERSION,
            settings: std::ptr::null_mut(),
            GlobalInit: Some(entrypoint_c::client_global_init),
            GlobalUninit: Some(entrypoint_c::client_global_uninit),
            ContextSize: std::mem::size_of::<RdpContext>() as u32,
            ClientNew: Some(entrypoint_c::client_new),
            ClientFree: Some(entrypoint_c::client_free),
            ClientStart: Some(entrypoint_c::client_start),
            ClientStop: Some(entrypoint_c::client_stop),
        };

        unsafe {
            // Hold the provider-install lock across context_new + re-assert:
            // freerdp_client_context_new re-registers the stock static loader
            // into the process-global slot, and we must re-install our custom
            // provider before any other session can observe the stock state.
            let _install_guard = addin::lock_provider_install();
            let ctx_ptr = freerdp_client_context_new(&entry_points);
            if ctx_ptr.is_null() {
                return Err(anyhow::anyhow!("Failed to create client context"));
            }
            // Override the addin provider to our custom one (in fact, "hook" the existing one)
            addin::register_channel_addin_loader();

            let ctx = ctx_ptr as *mut RdpContext;
            (*ctx).owner = owner as *mut Rdp;

            Ok(ctx)
        }
    }
}

impl Drop for RdpContext {
    fn drop(&mut self) {
        log::debug!("****** Dropping RdpContext, cleaning up resources... !!!!!!");
    }
}

/// Recover the owning `Rdp` from a FreeRDP context pointer.
///
/// # Safety / aliasing contract
/// This is a **safe** signature that performs an unsafe aliasing operation: it
/// returns `&'a mut Rdp` with a caller-chosen, unconstrained lifetime from the
/// `#[repr(C)]` owner back-pointer. The caller MUST guarantee:
/// - `ctx` is a live `RdpContext` whose `owner` is a valid, pinned `Rdp` (null
///   after teardown — returns `None` only for a null `ctx`/`owner`, not for a
///   freed one).
/// - **No other borrow of the same `Rdp` is live.** In particular
///   [`Rdp::context`](crate::Rdp::context) hands out `&RdpContext` aliasing the
///   same owner; minting a second overlapping `&mut Rdp` is UB. The crate
///   confines this to its single-threaded callback dispatch; external callers
///   must uphold the same single-writer discipline.
///
/// Prefer the crate's callback paths over calling this directly.
pub trait OwnerFromCtx<'a> {
    /// See the trait-level aliasing contract before calling.
    fn owner(&self) -> Option<&'a mut Rdp>;
}

impl<'a> OwnerFromCtx<'a> for *mut rdpContext {
    fn owner(&self) -> Option<&'a mut Rdp> {
        owner_from_ctx(*self)
    }
}

impl<'a> OwnerFromCtx<'a> for *mut freerdp {
    fn owner(&self) -> Option<&'a mut Rdp> {
        owner_from_ctx(unsafe { (*(*self)).context })
    }
}

impl<'a> OwnerFromCtx<'a> for *mut rdpInput {
    fn owner(&self) -> Option<&'a mut Rdp> {
        unsafe {
            if self.is_null() {
                return None;
            }
            let ctx = (*(*self)).context;
            owner_from_ctx(ctx)
        }
    }
}

/// Recover the owning `Rdp` from a raw `rdpContext` pointer.
///
/// # Safety / aliasing contract
/// See [`OwnerFromCtx`]: the returned `&mut Rdp` must not overlap any other
/// live borrow of the same owner, and `ctx` must be a live context (the owner
/// is freed on teardown). This safe wrapper exists for the crate's internal
/// callback dispatch, which upholds the single-writer discipline.
pub fn owner_from_ctx<'a>(ctx: *mut rdpContext) -> Option<&'a mut crate::Rdp> {
    unsafe {
        if ctx.is_null() {
            return None;
        }
        let ctx = ctx as *mut RdpContext;
        (*ctx).owner.as_mut()
    }
}
