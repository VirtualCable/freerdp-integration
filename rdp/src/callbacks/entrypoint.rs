// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::utils::log;

pub trait EntrypointCallbacks {
    fn client_start(&mut self) -> bool {
        log::debug!("**** Client started");
        true
    }

    fn client_stop(&mut self) -> bool {
        log::debug!("**** Client stopped");
        true
    }
}
