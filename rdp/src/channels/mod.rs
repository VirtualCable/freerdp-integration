// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

pub mod cliprdr;
pub mod disp;
pub mod gfx;
pub mod rail;

#[derive(Clone, Debug)]
pub struct RdpChannels {
    disp: Option<disp::DispChannel>,
    cliprdr: Option<cliprdr::RdpClipboard>,
    rail: Option<rail::RailChannel>,
    gfx: Option<gfx::GfxChannel>,
}

impl RdpChannels {
    pub fn new() -> Self {
        RdpChannels {
            disp: None,
            cliprdr: None,
            rail: None,
            gfx: None,
        }
    }

    pub fn set_disp_ptr(&mut self, disp: *mut freerdp_sys::DispClientContext) {
        self.disp = Some(disp::DispChannel::new(disp));
    }

    pub fn clear_disp(&mut self) {
        self.disp = None;
    }

    pub fn disp(&self) -> Option<disp::DispChannel> {
        self.disp.clone()
    }

    pub fn set_cliprdr_ptr(&mut self, cliprdr: *mut freerdp_sys::CliprdrClientContext) {
        let clipboard = cliprdr::RdpClipboard::new(cliprdr);
        self.cliprdr = Some(clipboard);
    }

    pub fn clear_cliprdr(&mut self) {
        self.cliprdr = None;
    }

    pub fn cliprdr(&self) -> Option<cliprdr::RdpClipboard> {
        self.cliprdr.clone()
    }

    pub fn set_rail_ptr(&mut self, rail: *mut freerdp_sys::RailClientContext) {
        self.rail = Some(rail::RailChannel::new(rail));
    }

    pub fn clear_rail(&mut self) {
        self.rail = None;
    }

    pub fn rail(&self) -> Option<rail::RailChannel> {
        self.rail.clone()
    }

    pub fn set_gfx_ptr(&mut self, gfx: *mut freerdp_sys::RdpgfxClientContext) {
        self.gfx = Some(gfx::GfxChannel::new(gfx));
    }

    pub fn clear_gfx(&mut self) {
        self.gfx = None;
    }

    pub fn gfx(&self) -> Option<gfx::GfxChannel> {
        self.gfx.clone()
    }
}

impl Default for RdpChannels {
    fn default() -> Self {
        Self::new()
    }
}
