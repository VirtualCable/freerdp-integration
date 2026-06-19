// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use freerdp_sys::rdpPointer;

use crate::utils::log;

pub trait GraphicsCallbacks {
    /// # Safety
    /// This function is unsafe because it dereferences a raw pointer to rdpPointer.
    unsafe fn on_pointer_new(&self, _pointer: *mut rdpPointer) -> bool {
        log::debug!("Pointer New callback not implemented");
        true
    }

    /// # Safety
    /// This function is unsafe because it dereferences a raw pointer to rdpPointer.
    unsafe fn on_pointer_free(&self, _pointer: *mut rdpPointer) {
        log::debug!("Pointer Free callback not implemented");
    }

    /// # Safety
    /// This function is unsafe because it dereferences a raw pointer to rdpPointer.
    unsafe fn on_pointer_set(&self, _pointer: *mut rdpPointer) -> bool {
        log::debug!("Pointer Set callback not implemented");
        true
    }

    fn on_pointer_set_null(&self) -> bool {
        log::debug!("Pointer SetNull callback not implemented");
        true
    }

    fn on_pointer_set_default(&self) -> bool {
        log::debug!("Pointer SetDefault callback not implemented");
        true
    }

    fn on_pointer_position(&self, _x: u32, _y: u32) -> bool {
        log::debug!("Pointer Position callback not implemented");
        true
    }
}
