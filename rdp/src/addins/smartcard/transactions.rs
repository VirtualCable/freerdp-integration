// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Andres Schumann, aschumann at virtualcable dot es

//! Card transactions across handles.
//!
//! The remote side opens the same card from several handles and wraps each
//! multi-APDU exchange in BEGIN/END_TRANSACTION. Requests from other handles
//! are parked until the transaction ends, so their APDUs never interleave.

use std::collections::VecDeque;

use crate::integrations::smartcard::{SCARD_E_INVALID_HANDLE, SCARD_S_SUCCESS};

use super::consts::{
    SCARD_IOCTL_BEGINTRANSACTION, SCARD_IOCTL_CONTROL, SCARD_IOCTL_DISCONNECT,
    SCARD_IOCTL_ENDTRANSACTION, SCARD_IOCTL_RECONNECT, SCARD_IOCTL_RELEASECONTEXT,
    SCARD_IOCTL_TRANSMIT,
};

struct Owner {
    card: u64,
    context: u64,
    depth: u32,
}

struct Parked<T> {
    ioctl: u32,
    card: u64,
    context: u64,
    item: T,
}

pub(crate) struct TransactionGate<T> {
    owner: Option<Owner>,
    parked: VecDeque<Parked<T>>,
}

fn needs_card_exclusively(ioctl: u32) -> bool {
    matches!(
        ioctl,
        SCARD_IOCTL_BEGINTRANSACTION
            | SCARD_IOCTL_TRANSMIT
            | SCARD_IOCTL_CONTROL
            | SCARD_IOCTL_RECONNECT
    )
}

fn handle_is_gone(result: u32) -> bool {
    result == SCARD_S_SUCCESS || result == SCARD_E_INVALID_HANDLE
}

impl<T> TransactionGate<T> {
    pub(crate) fn new() -> Self {
        TransactionGate {
            owner: None,
            parked: VecDeque::new(),
        }
    }

    pub(crate) fn owner_card(&self) -> Option<u64> {
        self.owner.as_ref().map(|o| o.card)
    }

    pub(crate) fn must_wait(&self, ioctl: u32, card: u64) -> bool {
        needs_card_exclusively(ioctl) && self.owner.as_ref().is_some_and(|o| o.card != card)
    }

    pub(crate) fn park(&mut self, ioctl: u32, card: u64, context: u64, item: T) {
        self.parked.push_back(Parked {
            ioctl,
            card,
            context,
            item,
        });
    }

    /// Oldest parked request that can run now, if any.
    pub(crate) fn next_runnable(&mut self) -> Option<(u32, u64, u64, T)> {
        let index = self
            .parked
            .iter()
            .position(|p| !self.must_wait(p.ioctl, p.card))?;
        self.parked
            .remove(index)
            .map(|p| (p.ioctl, p.card, p.context, p.item))
    }

    /// Updates the owner after a request has run.
    pub(crate) fn on_completed(&mut self, ioctl: u32, card: u64, context: u64, result: u32) {
        match ioctl {
            SCARD_IOCTL_BEGINTRANSACTION if result == SCARD_S_SUCCESS => match &mut self.owner {
                Some(owner) if owner.card == card => owner.depth += 1,
                Some(_) => {}
                None => {
                    self.owner = Some(Owner {
                        card,
                        context,
                        depth: 1,
                    })
                }
            },
            SCARD_IOCTL_ENDTRANSACTION if self.owner_card() == Some(card) => {
                if result == SCARD_E_INVALID_HANDLE {
                    // The handle no longer exists, so nobody can end its transaction.
                    self.owner = None;
                } else if result == SCARD_S_SUCCESS
                    && let Some(owner) = &mut self.owner
                {
                    owner.depth -= 1;
                    if owner.depth == 0 {
                        self.owner = None;
                    }
                }
            }
            SCARD_IOCTL_DISCONNECT if self.owner_card() == Some(card) && handle_is_gone(result) => {
                self.owner = None
            }
            SCARD_IOCTL_RELEASECONTEXT
                if result == SCARD_S_SUCCESS
                    && self.owner.as_ref().is_some_and(|o| o.context == context) =>
            {
                self.owner = None
            }
            _ => {}
        }
    }

    /// Removes the parked requests of a released context.
    pub(crate) fn take_context(&mut self, context: u64) -> Vec<T> {
        let (taken, kept): (VecDeque<_>, VecDeque<_>) =
            self.parked.drain(..).partition(|p| p.context == context);
        self.parked = kept;
        taken.into_iter().map(|p| p.item).collect()
    }

    pub(crate) fn take_all(&mut self) -> Vec<T> {
        self.parked.drain(..).map(|p| p.item).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addins::smartcard::consts::{
        SCARD_IOCTL_CANCEL, SCARD_IOCTL_GETATTRIB, SCARD_IOCTL_GETSTATUSCHANGEW,
        SCARD_IOCTL_STATUSW,
    };

    const OK: u32 = SCARD_S_SUCCESS;
    const CARD_A: u64 = 0xA;
    const CARD_B: u64 = 0xB;
    const CTX_A: u64 = 0x1;
    const CTX_B: u64 = 0x2;

    fn begin(gate: &mut TransactionGate<u32>, card: u64, context: u64) {
        assert!(!gate.must_wait(SCARD_IOCTL_BEGINTRANSACTION, card));
        gate.on_completed(SCARD_IOCTL_BEGINTRANSACTION, card, context, OK);
    }

    #[test]
    fn other_handle_waits_until_end_transaction() {
        let mut gate = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);

        assert!(gate.must_wait(SCARD_IOCTL_BEGINTRANSACTION, CARD_B));
        assert!(gate.must_wait(SCARD_IOCTL_TRANSMIT, CARD_B));
        assert!(gate.must_wait(SCARD_IOCTL_CONTROL, CARD_B));
        assert!(gate.must_wait(SCARD_IOCTL_RECONNECT, CARD_B));
        assert!(!gate.must_wait(SCARD_IOCTL_TRANSMIT, CARD_A));
        gate.park(SCARD_IOCTL_BEGINTRANSACTION, CARD_B, CTX_B, 1);
        assert!(gate.next_runnable().is_none());

        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_A, CTX_A, OK);

        let (ioctl, card, _, item) = gate.next_runnable().unwrap();
        assert_eq!(
            (ioctl, card, item),
            (SCARD_IOCTL_BEGINTRANSACTION, CARD_B, 1)
        );
    }

    #[test]
    fn nested_begin_needs_matching_ends() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        begin(&mut gate, CARD_A, CTX_A);

        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_A, CTX_A, OK);
        assert_eq!(gate.owner_card(), Some(CARD_A));
        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_A, CTX_A, OK);
        assert_eq!(gate.owner_card(), None);
    }

    #[test]
    fn end_from_another_handle_keeps_owner() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_B, CTX_B, OK);
        assert_eq!(gate.owner_card(), Some(CARD_A));
    }

    #[test]
    fn failed_end_keeps_owner() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_A, CTX_A, 0x8010_0001);
        assert_eq!(gate.owner_card(), Some(CARD_A));
    }

    #[test]
    fn end_on_invalid_handle_releases() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        begin(&mut gate, CARD_A, CTX_A);
        gate.on_completed(
            SCARD_IOCTL_ENDTRANSACTION,
            CARD_A,
            CTX_A,
            SCARD_E_INVALID_HANDLE,
        );
        assert_eq!(gate.owner_card(), None);
    }

    #[test]
    fn disconnect_of_owner_releases() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.on_completed(SCARD_IOCTL_DISCONNECT, CARD_A, CTX_A, OK);
        assert_eq!(gate.owner_card(), None);
    }

    #[test]
    fn release_context_of_owner_releases() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.on_completed(SCARD_IOCTL_RELEASECONTEXT, 0, CTX_B, OK);
        assert_eq!(gate.owner_card(), Some(CARD_A));
        gate.on_completed(SCARD_IOCTL_RELEASECONTEXT, 0, CTX_A, 0x8010_0001);
        assert_eq!(gate.owner_card(), Some(CARD_A));
        gate.on_completed(SCARD_IOCTL_RELEASECONTEXT, 0, CTX_A, OK);
        assert_eq!(gate.owner_card(), None);
    }

    #[test]
    fn failed_disconnect_keeps_owner() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.on_completed(SCARD_IOCTL_DISCONNECT, CARD_A, CTX_A, 0x8010_0001);
        assert_eq!(gate.owner_card(), Some(CARD_A));
        gate.on_completed(
            SCARD_IOCTL_DISCONNECT,
            CARD_A,
            CTX_A,
            SCARD_E_INVALID_HANDLE,
        );
        assert_eq!(gate.owner_card(), None);
    }

    #[test]
    fn failed_begin_does_not_take_the_card() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        gate.on_completed(SCARD_IOCTL_BEGINTRANSACTION, CARD_A, CTX_A, 0x8010_0002);
        assert_eq!(gate.owner_card(), None);
    }

    #[test]
    fn status_requests_never_wait() {
        let mut gate: TransactionGate<u32> = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        assert!(!gate.must_wait(SCARD_IOCTL_GETSTATUSCHANGEW, CARD_B));
        assert!(!gate.must_wait(SCARD_IOCTL_STATUSW, CARD_B));
        assert!(!gate.must_wait(SCARD_IOCTL_GETATTRIB, CARD_B));
        assert!(!gate.must_wait(SCARD_IOCTL_CANCEL, CARD_B));
        assert!(!gate.must_wait(SCARD_IOCTL_ENDTRANSACTION, CARD_B));
        assert!(!gate.must_wait(SCARD_IOCTL_DISCONNECT, CARD_B));
    }

    #[test]
    fn parked_requests_keep_arrival_order() {
        let mut gate = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.park(SCARD_IOCTL_BEGINTRANSACTION, CARD_B, CTX_B, 1);
        gate.park(SCARD_IOCTL_BEGINTRANSACTION, 0xC, 0x3, 2);
        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_A, CTX_A, OK);

        let (ioctl, card, context, item) = gate.next_runnable().unwrap();
        assert_eq!(item, 1);
        gate.on_completed(ioctl, card, context, OK);
        assert!(gate.next_runnable().is_none());

        gate.on_completed(SCARD_IOCTL_ENDTRANSACTION, CARD_B, CTX_B, OK);
        assert_eq!(gate.next_runnable().map(|r| r.3), Some(2));
    }

    #[test]
    fn take_context_removes_only_that_context() {
        let mut gate = TransactionGate::new();
        begin(&mut gate, CARD_A, CTX_A);
        gate.park(SCARD_IOCTL_TRANSMIT, CARD_B, CTX_B, 1);
        gate.park(SCARD_IOCTL_BEGINTRANSACTION, 0xC, 0x3, 2);
        gate.park(SCARD_IOCTL_BEGINTRANSACTION, CARD_B, CTX_B, 3);

        assert_eq!(gate.take_context(CTX_B), vec![1, 3]);
        assert_eq!(gate.take_all(), vec![2]);
    }
}
