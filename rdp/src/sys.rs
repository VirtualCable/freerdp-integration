// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

// Re exports from freerdp-sys

pub use freerdp_sys::{
    PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3, PTR_FLAGS_DOWN, PTR_FLAGS_MOVE,
    PTR_FLAGS_WHEEL, PTR_FLAGS_WHEEL_NEGATIVE, PTR_XFLAGS_BUTTON1, PTR_XFLAGS_BUTTON2, SetEvent,
    freerdp_input_send_extended_mouse_event, freerdp_input_send_keyboard_event_ex,
    freerdp_input_send_mouse_event, rdpGdi, rdpInput,
};
