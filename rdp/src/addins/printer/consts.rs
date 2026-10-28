// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! Constants for RDPDR Printer Redirection (MS-RDPEFS and MS-RDPEPC).

pub(crate) const RDPDR_DTYP_PRINT: u32 = 0x0000_0004;
pub(crate) const RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER: u32 = 0x0000_0002;

pub(crate) const IRP_MJ_CREATE: u32 = 0x00;
pub(crate) const IRP_MJ_CLOSE: u32 = 0x02;
pub(crate) const IRP_MJ_WRITE: u32 = 0x04;
pub(crate) const IRP_MJ_DEVICE_CONTROL: u32 = 0x0E;

pub(crate) const STATUS_SUCCESS: i32 = 0x0000_0000;
pub(crate) const STATUS_UNSUCCESSFUL: i32 = 0xC000_0001_u32 as i32;
pub(crate) const STATUS_INVALID_DATA: i32 = 0xC000_0022_u32 as i32;
pub(crate) const STATUS_NOT_SUPPORTED: i32 = 0xC000_00BB_u32 as i32;
pub(crate) const STATUS_PRINT_QUEUE_FULL: i32 = 0xC000_00C6_u32 as i32;

#[cfg(test)]
mod tests {
    use super::*;

    // MS-RDPEFS 2.2.2.1.1: 0x1 is RDPDR_PRINTER_ANNOUNCE_FLAG_ASCII. Sending it
    // makes the server read the UTF-16 driver and printer names as ASCII, so
    // they arrive cut at the first byte.
    #[test]
    fn default_printer_flag_is_not_the_ascii_flag() {
        assert_eq!(RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER, 0x0000_0002);
    }
}
