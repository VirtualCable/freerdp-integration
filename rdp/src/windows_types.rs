// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors
//    may be used to endorse or promote products derived from this software
//    without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use bitflags::bitflags;

bitflags! {
    /// Win32 Window Styles (WS_* constants).
    ///
    /// See: <https://learn.microsoft.com/en-us/windows/win32/winmsg/window-styles>
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct WindowStyle: u32 {
        /// Window has a thin-line border.
        const BORDER        = 0x0080_0000;
        /// Window has a title bar (includes WS_BORDER).
        const CAPTION       = 0x00C0_0000;
        /// Window is a child window.
        const CHILD         = 0x4000_0000;
        /// Same as WS_CHILD.
        const CHILD_WINDOW  = 0x4000_0000;
        /// Excludes child window area when drawing in parent.
        const CLIP_CHILDREN = 0x0200_0000;
        /// Clips child windows relative to each other.
        const CLIP_SIBLINGS = 0x0400_0000;
        /// Window is initially disabled.
        const DISABLED      = 0x0800_0000;
        /// Window has a dialog border style.
        const DLG_FRAME     = 0x0040_0000;
        /// First control of a group.
        const GROUP         = 0x0002_0000;
        /// Window has a horizontal scroll bar.
        const HSCROLL       = 0x0010_0000;
        /// Window is initially minimized. Same as WS_MINIMIZE.
        const ICONIC        = 0x2000_0000;
        /// Window is initially maximized.
        const MAXIMIZE      = 0x0100_0000;
        /// Window has a maximize button.
        const MAXIMIZE_BOX  = 0x0001_0000;
        /// Window is initially minimized.
        const MINIMIZE      = 0x2000_0000;
        /// Window has a minimize button.
        const MINIMIZE_BOX  = 0x0002_0000;
        /// Window is an overlapped window.
        const OVERLAPPED    = 0x0000_0000;
        /// Window is a pop-up window.
        const POPUP         = 0x8000_0000;
        /// Window has a sizing border (same as WS_THICKFRAME).
        const SIZE_BOX      = 0x0004_0000;
        /// Window has a window menu on its title bar.
        const SYSMENU       = 0x0008_0000;
        /// Control that can receive TAB focus.
        const TAB_STOP      = 0x0001_0000;
        /// Window has a sizing border.
        const THICK_FRAME   = 0x0004_0000;
        /// Tiled window (same as WS_OVERLAPPED).
        const TILED         = 0x0000_0000;
        /// Window has a vertical scroll bar.
        const VSCROLL       = 0x0020_0000;
        /// Window is initially visible.
        const VISIBLE       = 0x1000_0000;

        // Composite styles
        /// Overlapped window: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX
        const OVERLAPPED_WINDOW = 0x00CF_0000;
        /// Popup window: WS_POPUP | WS_BORDER | WS_SYSMENU
        const POPUP_WINDOW      = 0x8088_0000;
        /// Tiled window (same as WS_OVERLAPPEDWINDOW).
        const TILED_WINDOW      = 0x00CF_0000;
    }
}

bitflags! {
    /// Win32 Extended Window Styles (WS_EX_* constants).
    ///
    /// See: <https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles>
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ExtendedWindowStyle: u32 {
        /// Window accepts drag-drop files.
        const ACCEPT_FILES          = 0x0000_0010;
        /// Forces a top-level window onto the taskbar.
        const APP_WINDOW            = 0x0004_0000;
        /// Window has a border with a sunken edge.
        const CLIENT_EDGE           = 0x0000_0200;
        /// Paints all descendants in bottom-to-top order using double-buffering.
        const COMPOSITED            = 0x0200_0000;
        /// Title bar includes a question mark.
        const CONTEXT_HELP          = 0x0000_0400;
        /// Window contains child windows that participate in dialog navigation.
        const CONTROL_PARENT        = 0x0001_0000;
        /// Window has a double border.
        const DLG_MODAL_FRAME       = 0x0000_0001;
        /// Window has a left-to-right reading order (default).
        const LTR_READING           = 0x0000_0000;
        /// Window is a layered window.
        const LAYERED               = 0x0008_0000;
        /// Horizontal origin on the right edge.
        const LAYOUT_RTL            = 0x0040_0000;
        /// Window has generic left-aligned properties (default).
        const LEFT                  = 0x0000_0000;
        /// Vertical scroll bar to the left of client area.
        const LEFT_SCROLLBAR        = 0x0000_4000;
        /// Window text has left-to-right reading order (default).
        const LEFT_READING          = 0x0000_0000;
        /// Window is an MDI child window.
        const MDI_CHILD             = 0x0000_0040;
        /// Top-level window does not become the foreground window when clicked.
        const NO_ACTIVATE           = 0x0800_0000;
        /// Window does not pass its window layout to child windows.
        const NO_INHERIT_LAYOUT     = 0x0010_0000;
        /// Child window does not send WM_PARENTNOTIFY.
        const NO_PARENT_NOTIFY      = 0x0000_0004;
        /// Window does not render to a redirection surface.
        const NO_REDIRECTION_BITMAP = 0x0020_0000;
        /// Window has generic right-aligned properties.
        const RIGHT                 = 0x0000_1000;
        /// Vertical scroll bar to the right of client area (default).
        const RIGHT_SCROLLBAR       = 0x0000_0000;
        /// Window text has right-to-left reading order.
        const RTL_READING           = 0x0000_2000;
        /// Three-dimensional border style for non-interactive items.
        const STATIC_EDGE           = 0x0002_0000;
        /// Window is a floating toolbar (not in taskbar nor Alt+Tab).
        const TOOL_WINDOW           = 0x0000_0080;
        /// Window should be placed above all non-topmost windows.
        const TOP_MOST              = 0x0000_0008;
        /// Window should not be painted until siblings beneath it are painted.
        const TRANSPARENT           = 0x0000_0020;
        /// Window has a border with a raised edge.
        const WINDOW_EDGE           = 0x0000_0100;

        // Composite styles
        /// Overlapped window: WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE
        const OVERLAPPED_WINDOW     = 0x0000_0300;
        /// Palette window: WS_EX_WINDOWEDGE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST
        const PALETTE_WINDOW        = 0x0000_0188;
    }
}

/// Win32 ShowWindow commands (SW_* constants).
///
/// See: <https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow>
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShowWindowCmd {
    Hide = 0,
    ShowNormal = 1,
    ShowMinimized = 2,
    ShowMaximized = 3,
    ShowNoActivate = 4,
    Show = 5,
    Minimize = 6,
    ShowMinNoActive = 7,
    ShowNa = 8,
    Restore = 9,
    ShowDefault = 10,
    ForceMinimize = 11,
}

impl TryFrom<u32> for ShowWindowCmd {
    type Error = u32;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hide),
            1 => Ok(Self::ShowNormal),
            2 => Ok(Self::ShowMinimized),
            3 => Ok(Self::ShowMaximized),
            4 => Ok(Self::ShowNoActivate),
            5 => Ok(Self::Show),
            6 => Ok(Self::Minimize),
            7 => Ok(Self::ShowMinNoActive),
            8 => Ok(Self::ShowNa),
            9 => Ok(Self::Restore),
            10 => Ok(Self::ShowDefault),
            11 => Ok(Self::ForceMinimize),
            _ => Err(value),
        }
    }
}

/// Win32 System Commands (SC_* constants, WM_SYSCOMMAND).
///
/// Values from FreeRDP's winpr bindings (`freerdp_sys::SC_*`).
/// See: <https://learn.microsoft.com/en-us/windows/win32/menurc/wm-syscommand>
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemCommand {
    Size = 0xF000,
    Move = 0xF010,
    Minimize = 0xF020,
    Maximize = 0xF030,
    Close = 0xF060,
    KeyMenu = 0xF100,
    Restore = 0xF120,
}

impl TryFrom<u32> for SystemCommand {
    type Error = u32;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0xF000 => Ok(Self::Size),
            0xF010 => Ok(Self::Move),
            0xF020 => Ok(Self::Minimize),
            0xF030 => Ok(Self::Maximize),
            0xF060 => Ok(Self::Close),
            0xF100 => Ok(Self::KeyMenu),
            0xF120 => Ok(Self::Restore),
            _ => Err(value),
        }
    }
}


