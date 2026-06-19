// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

mod channels;
mod graphics; // implements graphics callbacks
mod instance; // implements instance callbacks
mod update; // implements update callbacks // implements channel callbacks

// Clipboard is set on channel connection. Callbacks will be registered then
// and will invoke us
mod clipboard; // implements clipboard callbacks
mod window; // implements window callbacks

use crate::callbacks::{altsec, entrypoint, input, pointer_update, primary, secondary};

use super::{Rdp, RdpMessage};

impl input::InputCallbacks for Rdp {}
impl pointer_update::PointerCallbacks for Rdp {}
impl primary::PrimaryCallbacks for Rdp {}
impl secondary::SecondaryCallbacks for Rdp {}
impl altsec::AltSecCallbacks for Rdp {}
impl entrypoint::EntrypointCallbacks for Rdp {}
