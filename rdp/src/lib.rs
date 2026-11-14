#![allow(clippy::chunks_exact_to_as_chunks)]
// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use std::sync::{Arc, RwLock};

type IconCache = std::collections::HashMap<(u32, u32), (Vec<u8>, u32, u32)>;

pub mod callbacks;

mod addins;
pub mod connection;
mod init;
pub mod utils;

pub mod events;
pub mod wlog;

pub mod geom;
pub mod settings;

pub mod context;
pub mod messaging;
pub mod windows_types;

// Re-export sys module
pub mod sys;

pub mod channels;
pub mod consts;
pub mod integrations;

#[derive(Debug)]
pub struct Config {
    pub settings: settings::RdpSettings,
    pub callbacks: callbacks::Callbacks,
    pub use_rgba: bool, // Whether to use RGBA pixel format or BGRA
    pub integrations: integrations::RdpIntegrations,
}

#[derive(Debug)]
pub struct Rdp {
    pub config: Config,
    instance: Option<utils::SafePtr<freerdp_sys::freerdp>>,
    update_tx: Option<messaging::Sender>,
    // GDI lock for thread safety
    gdi_lock: Arc<RwLock<()>>,
    // Note: We need a clonable safe struct for channels
    // because they are initialized after connection is created, on a later step
    channels: Arc<RwLock<channels::RdpChannels>>,
    /// RAIL icon cache, keyed by host-chosen `(cacheId, cacheEntry)`.
    ///
    /// # Contract
    /// Internal write-once / read-on-miss cache populated by `on_window_icon`
    /// and read by `on_window_cached_icon`. Bounded (see `MAX_ICON_CACHE_ENTRIES`);
    /// entries are evicted on overflow and the cache is per-`Rdp` (not shared
    /// across sessions). Consumers should treat it as opaque session state.
    pub icon_cache: Arc<RwLock<IconCache>>,
    stop_event: utils::SafeHandle,
    command_rx: messaging::CommandReceiver,
    command_event: utils::SafeHandle, // Win32 event to signal new commands
    _pin: std::marker::PhantomPinned, // Do not allow moving
}

impl Rdp {
    pub fn new(
        settings: settings::RdpSettings,
        update_tx: messaging::Sender,
        use_rgba: bool,
        channels: Option<Arc<RwLock<channels::RdpChannels>>>,
        integrations: integrations::RdpIntegrations,
    ) -> (Self, messaging::CommandSender) {
        let stop_event: freerdp_sys::HANDLE =
            unsafe { freerdp_sys::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        let command_event: freerdp_sys::HANDLE =
            unsafe { freerdp_sys::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()) }; // Auto-reset event

        let stop_event = utils::SafeHandle::new(stop_event).unwrap();
        let command_event = utils::SafeHandle::new(command_event).unwrap();
        let (command_tx, command_rx) = flume::unbounded();

        let is_rail = settings.rail.is_some();

        (
            Rdp {
                config: Config {
                    settings,
                    use_rgba,
                    callbacks: if is_rail {
                        callbacks::Callbacks {
                            window: callbacks::window_c::Callbacks::all(),
                            ..callbacks::Callbacks::default()
                        }
                    } else {
                        callbacks::Callbacks::default()
                    },
                    integrations,
                },
                instance: None,
                update_tx: Some(update_tx),
                gdi_lock: Arc::new(RwLock::new(())),
                channels: channels
                    .unwrap_or_else(|| Arc::new(RwLock::new(channels::RdpChannels::new()))),
                icon_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
                stop_event,
                command_rx,
                command_event,
                _pin: std::marker::PhantomPinned,
            },
            command_tx,
        )
    }

    /// Borrow the FreeRDP client context for this session.
    ///
    /// # Contract (read before use)
    /// - Returns `None` before the connection is built and after teardown
    ///   (`instance` is `None`); the context pointer is null in those windows.
    /// - The returned `&RdpContext` aliases the same `Rdp` that
    ///   [`owner_from_ctx`](context::owner_from_ctx) / [`OwnerFromCtx`](context::OwnerFromCtx) hand out
    ///   as `&mut Rdp`. Do NOT create an overlapping `&mut Rdp` while this
    ///   borrow (or any other borrow of the owner) is live — that is UB. The
    ///   crate serializes owner access through its callback dispatch; external
    ///   consumers must respect the same single-writer discipline.
    /// - The context (and its `gdi`, `input`, `settings` fields) is owned by
    ///   FreeRDP and freed on disconnect/deactivation; do not retain the
    ///   borrow past the session's lifetime.
    pub fn context(&self) -> Option<&context::RdpContext> {
        unsafe {
            if let Some(instance) = self.instance {
                let ctx = instance.context as *mut context::RdpContext;
                if ctx.is_null() { None } else { Some(&*ctx) }
            } else {
                None
            }
        }
    }

    pub fn get_stop_event(&self) -> crate::utils::SafeHandle {
        crate::utils::SafeHandle::new(self.stop_event.as_handle()).unwrap_or_else(|| {
            panic!("Failed to clone stop event handle");
        })
    }

    pub fn get_command_event(&self) -> crate::utils::SafeHandle {
        crate::utils::SafeHandle::new(self.command_event.as_handle()).unwrap_or_else(|| {
            panic!("Failed to clone command event handle");
        })
    }

    // Note: For conveinence only, does not has "self"
    pub fn set_stop_event(stop_event: &crate::utils::SafeHandle) {
        unsafe {
            freerdp_sys::SetEvent(stop_event.as_handle());
        }
    }

    pub fn set_command_event(command_event: &crate::utils::SafeHandle) {
        unsafe {
            freerdp_sys::SetEvent(command_event.as_handle());
        }
    }

    /// Raw `rdpInput` pointer for injecting input events.
    ///
    /// # Contract (read before use)
    /// - `None` before build / after teardown (see [`context`](Self::context)).
    /// - The pointer is owned by FreeRDP and freed on disconnect; do not use it
    ///   past the session lifetime or after the context is gone.
    /// - Input callbacks run on FreeRDP's threads; coordinate with the session
    ///   thread (the crate's input dispatch) rather than calling concurrently.
    pub fn input(&self) -> Option<*mut freerdp_sys::rdpInput> {
        if let Some(context) = self.context() {
            let input = context.context().input;
            if input.is_null() { None } else { Some(input) }
        } else {
            None
        }
    }

    pub fn channels(&self) -> Arc<RwLock<channels::RdpChannels>> {
        self.channels.clone()
    }

    /// Raw `rdpGdi` pointer for the session's primary framebuffer.
    ///
    /// # Contract (read before use)
    /// - `None` before build / after teardown, and **after FreeRDP frees the GDI
    ///   on deactivation/disconnect** (`gdi_free` in PostDisconnect). The pointer
    ///   becomes dangling in that window; re-check validity each use.
    /// - **You MUST hold [`gdi_lock`](Self::gdi_lock) across any `gdi_*` call.**
    ///   The crate takes the write side in `on_desktop_resize` / PostDisconnect
    ///   and the read side in consumers (e.g. `RectSender::encode`); the lock is
    ///   the two-sided serialization point that orders GDI use against resize
    ///   and free. Using `gdi()` without the lock races with teardown (UAF).
    pub fn gdi(&self) -> Option<*mut freerdp_sys::rdpGdi> {
        if let Some(context) = self.context() {
            let gdi = context.context().gdi;
            if gdi.is_null() { None } else { Some(gdi) }
        } else {
            None
        }
    }

    /// The GDI serialization lock (see [`gdi`](Self::gdi)).
    ///
    /// # Contract
    /// Hold the **read** side for GDI reads/encodes and the **write** side for
    /// resize/free. This is the same `Arc<RwLock<()>>` the crate's callbacks
    /// use; consumers MUST participate in it for `gdi()` access to be safe.
    pub fn gdi_lock(&self) -> Arc<RwLock<()>> {
        self.gdi_lock.clone()
    }

    /// Current desktop size read from the GDI.
    ///
    /// # Contract
    /// Reads `gdi()` (see its contract): the GDI may be freed on deactivation.
    /// Returns `Fixed(0, 0)` when there is no GDI. For race-free reads against
    /// resize/free, hold [`gdi_lock`](Self::gdi_lock) around the call.
    pub fn screen_size(&self) -> geom::ScreenSize {
        if let Some(gdi) = self.gdi() {
            let width = unsafe { (*gdi).width as u32 };
            let height = unsafe { (*gdi).height as u32 };
            geom::ScreenSize::Fixed(width, height)
        } else {
            geom::ScreenSize::Fixed(0, 0)
        }
    }

    pub fn use_rgba(&self) -> bool {
        self.config.use_rgba
    }

    pub fn update_tx(&self) -> messaging::Sender {
        self.update_tx.as_ref().unwrap().clone()
    }
}
