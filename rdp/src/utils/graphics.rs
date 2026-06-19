// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

pub fn pixel_format(bpp: u8, pixel_type: u8, a: u8, r: u8, g: u8, b: u8) -> u32 {
    ((bpp as u32) << 24)
        | ((pixel_type as u32) << 16)
        | ((a as u32) << 12)
        | ((r as u32) << 8)
        | ((g as u32) << 4)
        | (b as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_rgba32() {
        let pf = pixel_format(32, 3, 8, 8, 8, 8);
        assert_eq!(pf, 0x20038888);
    }

    #[test]
    fn pixel_format_bgra32() {
        let pf = pixel_format(32, 4, 8, 8, 8, 8);
        assert_eq!(pf, 0x20048888);
    }

    #[test]
    fn pixel_format_all_zero() {
        assert_eq!(pixel_format(0, 0, 0, 0, 0, 0), 0);
    }
}
