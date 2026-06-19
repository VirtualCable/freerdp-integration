// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::utils::log;

// RDP needs WinSock to be initialized befere, at least, open connection
fn init_socks() {
    log::debug!("Initializing WinSock...");

    #[cfg(windows)]
    unsafe {
        use windows::Win32::Networking::WinSock::{WSADATA, WSAStartup};

        let mut wsa_data = std::mem::zeroed::<WSADATA>();
        let version: u16 = 0x0202;

        // 0x101 = MAKEWORD(1, 1), MAKEWORD(2, 2) for WinSock 2.2
        let ret = WSAStartup(version, &mut wsa_data);
        if ret != 0 {
            panic!("WSAStartup failed: {}", ret);
        }
    }
}

fn uninit_socks() {
    #[cfg(windows)]
    unsafe {
        windows::Win32::Networking::WinSock::WSACleanup();
    }
}

fn init_callbacks() {
    log::debug!("Initializing RDP Callbacks...");
    // Ensure that the callback is set to our wrapper function
    // We will have only that function with varargs disabled
    use super::callbacks::instance_c::get_access_token_no_varargs;

    unsafe { freerdp_sys::set_rust_get_access_token_cb(get_access_token_no_varargs) };
}

pub fn initialize() {
    init_socks();
    init_callbacks();
}

pub fn uninitialize() {
    // Currently, we do not need any special handling here.
    uninit_socks();
}
