// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use freerdp_sys::{AccessTokenType, SmartcardCertInfo};

use crate::utils::log;

pub trait InstanceCallbacks {
    fn on_pre_connect(&mut self) -> bool {
        log::debug!(" **** Preparing connection...");
        true
    }

    fn on_post_connect(&mut self) -> bool {
        log::debug!(" **** Connected successfully!");
        true
    }

    fn on_context_new(&mut self) -> bool {
        log::debug!(" **** Context new...");
        true
    }

    fn on_context_free(&mut self) {
        log::debug!(" **** Context free...");
    }

    #[allow(unused_variables)]
    fn on_authenticate(
        &mut self,
        username: *mut *mut ::std::os::raw::c_char,
        password: *mut *mut ::std::os::raw::c_char,
        domain: *mut *mut ::std::os::raw::c_char,
    ) -> bool {
        log::debug!(" **** Authenticating...");
        true
    }

    #[allow(unused_variables)]
    fn on_authenticate_ex(
        &mut self,
        _username: *mut *mut ::std::os::raw::c_char,
        _password: *mut *mut ::std::os::raw::c_char,
        _domain: *mut *mut ::std::os::raw::c_char,
        _reason: i32,
    ) -> bool {
        log::debug!(" **** Authenticating (extended)...");
        true
    }

    #[allow(unused_variables)]
    fn on_gateway_authenticate(
        &mut self,
        username: *mut *mut ::std::os::raw::c_char,
        password: *mut *mut ::std::os::raw::c_char,
        domain: *mut *mut ::std::os::raw::c_char,
    ) -> bool {
        log::debug!(" **** Authenticating...");
        true
    }

    #[allow(unused_variables)]
    fn on_choose_smartcard(
        &mut self,
        cert_list: *mut *mut SmartcardCertInfo,
        count: u32,
        choice: *mut u32,
        gateway: bool,
    ) -> bool {
        log::debug!(" **** Choosing smartcard certificate...");
        true
    }

    #[allow(unused_variables)]
    fn on_get_access_token(
        &mut self,
        token_type: AccessTokenType,
        token: *mut *mut ::std::os::raw::c_char,
        count: usize,
        data: *const *const ::std::os::raw::c_char,
    ) -> bool {
        log::debug!(" **** Getting access token...");
        true
    }

    fn on_verify_x509_certificate(
        &mut self,
        data: *const u8,
        length: usize,
        hostname: &str,
        port: u16,
        flags: u32,
    ) -> bool {
        log::debug!(
            " **** Verifying certificate: Hostname: {}, Port: {}, Flags: {}, Data: {:?} Data Length: {}",
            hostname,
            port,
            flags,
            data,
            length
        );
        // Fail closed by default. This callback is only reached when FreeRDP's
        // own verification did NOT fully succeed (chain not trusted by the
        // system CA store, or hostname/IP mismatch). Accepting here would
        // override that rejection and give false security. A real policy
        // (pin / fingerprint allowlist / custom CA) must override this.
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn on_verify_certificate(
        &mut self,
        host: &str,
        port: u16,
        common_name: &str,
        subject: &str,
        issuer: &str,
        fingerprint: &str,
        flags: u32,
    ) -> u32 {
        log::debug!(
            " **** Verifying certificate: Host: {:?}, Port: {}, Common Name: {:?}, Subject: {:?}, Issuer: {:?}, Fingerprint: {:?}, Flags: {}",
            host,
            port,
            common_name,
            subject,
            issuer,
            fingerprint,
            flags
        );
        // Fail closed by default (0 = reject, no store). See
        // on_verify_x509_certificate: this is only reached when FreeRDP's
        // verification did not fully succeed; never accept-and-store (1).
        0
    }

    fn on_logon_error_info(&mut self, data_str: &str, type_str: &str) -> bool {
        log::debug!(
            " **** Logon error info received... Data: {}, Type: {}",
            data_str,
            type_str
        );
        true
    }

    fn on_post_disconnect(&mut self) {
        log::debug!(" ****post_disconnected.");
    }

    fn on_present_gateway_message(
        &mut self,
        msg_type: u32,
        is_display_mandatory: bool,
        is_consent_mandatory: bool,
        length: usize,
        message: String,
    ) -> bool {
        log::debug!(
            " **** Gateway message received. Type: {}, Display Mandatory: {}, Consent Mandatory: {}, Length: {}, Message: {}",
            msg_type,
            is_display_mandatory,
            is_consent_mandatory,
            length,
            message
        );
        true
    }

    fn on_redirect(&mut self) -> bool {
        log::debug!(" **** Redirecting...");
        true
    }

    fn on_load_channels(&mut self) -> bool {
        log::debug!(" **** Loading channels...");
        true
    }

    // fn on_send_channel_data(&mut self, channel_id: u16, data: *const u8, size: usize) -> bool {
    //     log::debug!(
    //         " **** Sending channel data... Channel ID: {}, Data: {:?}, Size: {}",
    //         channel_id, data, size
    //     );
    //     true
    // }

    // fn on_receive_channel_data(
    //     &mut self,
    //     channel_id: u16,
    //     data: *const u8,
    //     size: usize,
    //     flags: u32,
    //     total_size: usize,
    // ) -> bool {
    //     log::debug!(
    //         " **** Receiving channel data... Channel ID: {}, Data: {:?}, Size: {}, Flags: {}, Total Size: {}",
    //         channel_id, data, size, flags, total_size
    //     );
    //     true
    // }

    // fn on_send_channel_packet(
    //     &mut self,
    //     channel_id: u16,
    //     total_size: usize,
    //     flags: u32,
    //     data: *const u8,
    //     chunk_size: usize,
    // ) -> bool {
    //     log::debug!(
    //         " **** Sending channel packet... Channel ID: {}, Total Size: {}, Flags: {}, Data: {:?}, Chunk Size: {}",
    //         channel_id, total_size, flags, data, chunk_size
    //     );
    //     true
    // }

    fn on_post_final_disconnect(&mut self) {
        log::debug!(" **** final disconnected.");
    }

    fn on_retry_dialog(
        &mut self,
        what: &str,
        current: usize,
        userarg: *mut ::std::os::raw::c_void,
    ) -> i64 {
        log::debug!(
            " **** Retry dialog invoked. What: {}, Current: {}, UserArg: {:?}",
            what,
            current,
            userarg
        );
        -1 // Indicate no retry by default
    }
}

#[cfg(test)]
mod tests {
    //! Certificate-verification contract (Fix J): the trait defaults must
    //! fail closed. FreeRDP performs real verification (system CA store +
    //! hostname/IP SAN match) when IgnoreCertificate=false (verify_cert=true);
    //! these callbacks are only reached when that verification did NOT fully
    //! succeed, so accepting here would override the rejection and give false
    //! security. verify_cert=false sets IgnoreCertificate=true and FreeRDP
    //! skips verification entirely (these callbacks are not invoked).
    use super::*;

    struct Dummy;
    impl InstanceCallbacks for Dummy {}

    #[test]
    fn x509_default_rejects() {
        let mut d = Dummy;
        assert!(!d.on_verify_x509_certificate(std::ptr::null(), 0, "host.example", 3389, 0));
    }

    #[test]
    fn x509_default_rejects_changed_cert() {
        let mut d = Dummy;
        // VERIFY_CERT_FLAG_CHANGED = 64: MitM / regenerated-cert signal.
        assert!(!d.on_verify_x509_certificate(std::ptr::null(), 0, "host.example", 3389, 64));
    }

    #[test]
    fn certificate_default_rejects_no_store() {
        let mut d = Dummy;
        // 0 = reject (not 1 = accept-and-store, not 2 = accept-temporary).
        assert_eq!(
            d.on_verify_certificate("host", 3389, "cn", "subj", "issuer", "fp", 0),
            0
        );
    }

    #[test]
    fn certificate_default_rejects_changed() {
        let mut d = Dummy;
        assert_eq!(
            d.on_verify_certificate("host", 3389, "cn", "subj", "issuer", "fp", 64),
            0
        );
        assert_eq!(
            d.on_verify_certificate("host", 3389, "cn", "subj", "issuer", "fp", 128),
            0
        );
    }
}
