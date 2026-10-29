// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Author: Andres Schumann, aschumann at virtualcable dot es

//! Storage for a decoded `SMARTCARD_OPERATION` that tolerates newer FreeRDP.

use freerdp_sys::SMARTCARD_OPERATION;

/// FreeRDP 3.28 appended `ret` and `returnCode` to `SMARTCARD_OPERATION`
/// (128 -> 192 bytes on x86_64), and `smartcard_operation_free` overwrites the
/// whole struct of the library it runs with. A binary built against older
/// headers and run on a newer libfreerdp would have the tail written past its
/// buffer, so the struct carries spare room for appended fields.
const ROOM_FOR_NEWER_FREERDP: usize = 256;

#[repr(C)]
pub(crate) struct Operation {
    op: SMARTCARD_OPERATION,
    _room_for_newer_freerdp: [u8; ROOM_FOR_NEWER_FREERDP],
}

impl Operation {
    pub(crate) fn zeroed() -> Self {
        // Newer fields freed by smartcard_operation_free (pointers in `ret`)
        // must read as NULL, so the spare room starts zeroed too.
        unsafe { std::mem::zeroed() }
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut SMARTCARD_OPERATION {
        &mut self.op
    }
}

impl std::ops::Deref for Operation {
    type Target = SMARTCARD_OPERATION;

    fn deref(&self) -> &SMARTCARD_OPERATION {
        &self.op
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILL: u8 = 0xAA;

    #[repr(C)]
    struct Guarded {
        operation: Operation,
        canary: [u8; 64],
    }

    /// Bytes `smartcard_operation_free` of the loaded libfreerdp cleared, which
    /// is the size of its own `SMARTCARD_OPERATION`.
    fn cleared_from_start(guarded: &Guarded) -> usize {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (guarded as *const Guarded).cast::<u8>(),
                size_of::<Guarded>(),
            )
        };
        bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len())
    }

    #[test]
    fn freeing_never_writes_past_the_operation() {
        let mut guarded = Guarded {
            operation: Operation::zeroed(),
            canary: [FILL; 64],
        };
        // An unknown ioctl (0xAAAAAAAA) makes free() skip every pointer and
        // only clear the struct.
        unsafe {
            std::ptr::write_bytes(&raw mut guarded.operation, FILL, 1);
            freerdp_sys::smartcard_operation_free(guarded.operation.as_mut_ptr(), 0);
        }

        let cleared = cleared_from_start(&guarded);
        assert!(cleared >= size_of::<SMARTCARD_OPERATION>());
        assert!(
            cleared <= size_of::<Operation>(),
            "libfreerdp cleared {cleared} bytes, Operation holds {}",
            size_of::<Operation>()
        );
        assert_eq!(guarded.canary, [FILL; 64]);
    }

    #[test]
    fn the_spare_room_starts_zeroed() {
        // Fields a newer libfreerdp frees (pointers in `ret`) live there.
        let operation = Operation::zeroed();
        assert!(
            operation
                ._room_for_newer_freerdp
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(operation.ioControlCode, 0);
        assert_eq!(operation.hContext, 0);
    }
}
