// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use crate::integrations::WebcamIntegration;
use crate::utils::log;
use freerdp_sys::{
    BOOL, BYTE, CHANNEL_RC_OK, IWTSListener, IWTSListenerCallback, IWTSVirtualChannel,
    IWTSVirtualChannelCallback, IWTSVirtualChannelManager, UINT,
};
use std::sync::Arc;

use super::channel::{self, ChannelCtx};
use super::pdu::write_to_channel;

// --- Control Channel (Device Enumerator) Listener ---

#[repr(C)]
pub struct ControlListenerCtx {
    pub listener_cb: IWTSListenerCallback,
    pub webcam: Arc<dyn WebcamIntegration>,
    pub channel_mgr: *mut IWTSVirtualChannelManager,
    pub rdpcontext: *mut freerdp_sys::rdpContext,
}

#[repr(C)]
pub struct ControlChannelCtx {
    pub channel_cb: IWTSVirtualChannelCallback,
    pub channel: *mut IWTSVirtualChannel,
    pub webcam: Arc<dyn WebcamIntegration>,
    pub channel_mgr: *mut IWTSVirtualChannelManager,
    pub dev_listener: Option<*mut IWTSListener>,
    pub dev_listener_ctx: Option<*mut DeviceListenerCtx>,
    pub rdpcontext: *mut freerdp_sys::rdpContext,
}

pub unsafe extern "C" fn on_new_control_channel(
    listener_cb: *mut IWTSListenerCallback,
    p_channel: *mut IWTSVirtualChannel,
    _data: *mut BYTE,
    pb_accept: *mut BOOL,
    pp_callback: *mut *mut IWTSVirtualChannelCallback,
) -> UINT {
    let lctx = listener_cb as *mut ControlListenerCtx;
    let webcam = unsafe { (*lctx).webcam.clone() };
    let channel_mgr = unsafe { (*lctx).channel_mgr };
    let rdpcontext = unsafe { (*lctx).rdpcontext };

    log::info!("Webcam Control: OnNewChannelConnection — channel={p_channel:?}, accepting");

    unsafe {
        *pb_accept = true.into();
    }

    let mut channel_ctx = Box::new(ControlChannelCtx {
        channel_cb: IWTSVirtualChannelCallback {
            OnDataReceived: Some(on_control_data),
            OnOpen: Some(on_control_open),
            OnClose: Some(on_control_close),
            ..unsafe { std::mem::zeroed() }
        },
        channel: p_channel,
        webcam,
        channel_mgr,
        dev_listener: None,
        dev_listener_ctx: None,
        rdpcontext,
    });

    unsafe {
        *pp_callback = &mut channel_ctx.channel_cb;
    }

    let _ = Box::into_raw(channel_ctx);
    CHANNEL_RC_OK
}

pub unsafe extern "C" fn on_control_open(cb: *mut IWTSVirtualChannelCallback) -> UINT {
    let ctx = cb as *mut ControlChannelCtx;
    log::info!("Webcam Control: Channel opened. Sending SelectVersionRequest...");

    // SelectVersionRequest: version = 1, msg_id = 3
    let pdu = &[1u8, 3u8];
    unsafe { write_to_channel((*ctx).channel, pdu) };
    CHANNEL_RC_OK
}

pub unsafe extern "C" fn on_control_data(
    cb: *mut IWTSVirtualChannelCallback,
    stream: *mut freerdp_sys::wStream,
) -> UINT {
    let ctx = unsafe { &mut *(cb as *mut ControlChannelCtx) };
    if stream.is_null() {
        return CHANNEL_RC_OK;
    }

    let s = unsafe { &*stream };
    let bytes = unsafe { std::slice::from_raw_parts(s.pointer, s.length) };

    if bytes.len() < 2 {
        log::error!("Webcam Control PDU too short: {}", bytes.len());
        return CHANNEL_RC_OK;
    }
    let version = bytes[0];
    let msg_id = bytes[1];
    log::info!(
        "Webcam Control PDU received: version={version} msg_id={msg_id} len={}",
        bytes.len()
    );

    if msg_id == 0x04 {
        // CAM_MSG_ID_SelectVersionResponse
        log::info!("Webcam Control: Version response received. Sending DeviceAddedNotification...");
        // Send DeviceAddedNotification: version = 1, msg_id = 5
        let mut pdu = Vec::new();
        pdu.push(1u8); // version
        pdu.push(5u8); // msg_id (CAM_MSG_ID_DeviceAddedNotification)

        // DeviceName (UTF-16 LE null-terminated): e.g. "UDS Camera\0" or "Mock Camera\0"
        let name = ctx.webcam.get_device_name();
        let mut utf16: Vec<u16> = name.encode_utf16().collect();
        utf16.push(0); // Null terminator
        for &val in &utf16 {
            pdu.extend_from_slice(&val.to_le_bytes());
        }

        // VirtualChannelName (ASCII null-terminated): "RDCamera_Capture_Device_0\0"
        let channel_name = "RDCamera_Capture_Device_0\0";
        pdu.extend_from_slice(channel_name.as_bytes());

        unsafe { write_to_channel(ctx.channel, &pdu) };

        log::info!("Webcam Control: Creating dynamic listener for RDCamera_Capture_Device_0...");
        let (raw_dev_ctx, dev_listener, err) = unsafe {
            create_device_listener(
                ctx.webcam.clone(),
                ctx.channel_mgr,
                "RDCamera_Capture_Device_0",
                ctx.rdpcontext,
            )
        };
        if err == CHANNEL_RC_OK {
            ctx.dev_listener = Some(dev_listener);
            ctx.dev_listener_ctx = Some(raw_dev_ctx);
            log::info!("Webcam Control: Dynamic listener created successfully");
        } else {
            log::error!("Webcam Control: Failed to create dynamic listener: {err}");
        }
    }

    CHANNEL_RC_OK
}

pub unsafe extern "C" fn on_control_close(cb: *mut IWTSVirtualChannelCallback) -> UINT {
    let ctx = cb as *mut ControlChannelCtx;
    log::info!("Webcam Control: Channel closed. Cleaning up listeners...");

    unsafe {
        if let Some(l) = (*ctx).dev_listener.take() {
            let mgr = (*ctx).channel_mgr;
            if let Some(destroy_fn) = (*mgr).DestroyListener {
                destroy_fn(mgr, l);
            }
        }
        if let Some(c) = (*ctx).dev_listener_ctx.take() {
            let _ = Box::from_raw(c);
        }
        let _ = Box::from_raw(ctx);
    }
    CHANNEL_RC_OK
}

// --- Device Channel Listener ---

#[repr(C)]
pub struct DeviceListenerCtx {
    pub listener_cb: IWTSListenerCallback,
    pub webcam: Arc<dyn WebcamIntegration>,
    pub rdpcontext: *mut freerdp_sys::rdpContext,
}

pub unsafe extern "C" fn on_new_device_channel(
    listener_cb: *mut IWTSListenerCallback,
    p_channel: *mut IWTSVirtualChannel,
    _data: *mut BYTE,
    pb_accept: *mut BOOL,
    pp_callback: *mut *mut IWTSVirtualChannelCallback,
) -> UINT {
    let lctx = listener_cb as *mut DeviceListenerCtx;
    let webcam = unsafe { (*lctx).webcam.clone() };
    let rdpcontext = unsafe { (*lctx).rdpcontext };

    log::info!("Webcam Device: OnNewChannelConnection — channel={p_channel:?}, accepting");

    unsafe {
        *pb_accept = true.into();
    }

    let mut channel_ctx = Box::new(ChannelCtx {
        channel_cb: IWTSVirtualChannelCallback {
            OnDataReceived: Some(channel::on_data),
            OnOpen: Some(channel::on_open),
            OnClose: Some(channel::on_close),
            ..unsafe { std::mem::zeroed() }
        },
        channel: p_channel,
        webcam,
        stream_index: 0,
        rdpcontext,
    });

    unsafe {
        *pp_callback = &mut channel_ctx.channel_cb;
    }

    let _ = Box::into_raw(channel_ctx);
    CHANNEL_RC_OK
}

// --- Public Initialization Functions ---

pub(super) fn create_listener(
    webcam: Arc<dyn WebcamIntegration>,
    channel_mgr: *mut IWTSVirtualChannelManager,
    rdpcontext: *mut freerdp_sys::rdpContext,
) -> (*mut ControlListenerCtx, *mut IWTSListener, UINT) {
    let mut listener_ctx = Box::new(ControlListenerCtx {
        listener_cb: IWTSListenerCallback {
            OnNewChannelConnection: Some(on_new_control_channel),
            pInterface: std::ptr::null_mut(), // unused
        },
        webcam,
        channel_mgr,
        rdpcontext,
    });

    let mut listener_handle: *mut IWTSListener = std::ptr::null_mut();
    let error = unsafe {
        (*channel_mgr).CreateListener.unwrap_unchecked()(
            channel_mgr,
            c"RDCamera_Device_Enumerator".as_ptr(),
            0,
            &mut listener_ctx.listener_cb,
            &mut listener_handle,
        )
    };

    let raw_ctx = Box::into_raw(listener_ctx);
    (raw_ctx, listener_handle, error)
}

pub(super) unsafe fn create_device_listener(
    webcam: Arc<dyn WebcamIntegration>,
    channel_mgr: *mut IWTSVirtualChannelManager,
    device_id: &str,
    rdpcontext: *mut freerdp_sys::rdpContext,
) -> (*mut DeviceListenerCtx, *mut IWTSListener, UINT) {
    let mut listener_ctx = Box::new(DeviceListenerCtx {
        listener_cb: IWTSListenerCallback {
            OnNewChannelConnection: Some(on_new_device_channel),
            pInterface: std::ptr::null_mut(),
        },
        webcam,
        rdpcontext,
    });

    let mut listener_handle: *mut IWTSListener = std::ptr::null_mut();
    let c_device_id = std::ffi::CString::new(device_id).unwrap();
    let error = unsafe {
        (*channel_mgr).CreateListener.unwrap_unchecked()(
            channel_mgr,
            c_device_id.as_ptr(),
            0,
            &mut listener_ctx.listener_cb,
            &mut listener_handle,
        )
    };

    let raw_ctx = Box::into_raw(listener_ctx);
    (raw_ctx, listener_handle, error)
}

#[cfg(test)]
mod tests {
    //! Characterization tests for the webcam CONTROL-channel contract (the
    //! host-facing enumerator handshake): SelectVersion, DeviceAdded
    //! notification bytes, dynamic device-listener creation/destruction, and
    //! the device-channel acceptance wiring. Test-only: pins the CURRENT
    //! observable behavior so the lifetime fixes cannot silently alter it.
    use super::*;
    use crate::integrations::{WebcamFrame, WebcamMode};
    use std::ffi::CStr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    type WriteSink = Arc<Mutex<Vec<Vec<u8>>>>;

    /// Fake host DVC channel: `Write` captures exact PDU bytes into a
    /// per-channel sink recovered from the `pChannel` pointer (repr(C) with
    /// the C struct first). No globals; safe under parallel tests.
    #[repr(C)]
    struct FakeChannel {
        inner: IWTSVirtualChannel,
        sink: WriteSink,
    }
    unsafe impl Send for FakeChannel {}
    unsafe impl Sync for FakeChannel {}

    unsafe extern "C" fn fake_write(
        channel: *mut IWTSVirtualChannel,
        size: freerdp_sys::ULONG,
        buffer: *const BYTE,
        _reserved: *mut std::ffi::c_void,
    ) -> UINT {
        let bytes = unsafe { std::slice::from_raw_parts(buffer, size as usize) }.to_vec();
        let fake = unsafe { &*(channel as *const FakeChannel) };
        fake.sink.lock().unwrap().push(bytes);
        CHANNEL_RC_OK
    }

    impl FakeChannel {
        fn new(sink: WriteSink) -> Box<Self> {
            Box::new(FakeChannel {
                inner: IWTSVirtualChannel {
                    Write: Some(fake_write),
                    Close: None,
                },
                sink,
            })
        }
        fn ptr(&mut self) -> *mut IWTSVirtualChannel {
            &mut self.inner
        }
    }

    /// Fake DVC manager: records CreateListener/DestroyListener calls.
    /// Created listener handles are heap allocations reclaimed on destroy.
    #[repr(C)]
    struct FakeMgr {
        inner: IWTSVirtualChannelManager,
        created: Arc<Mutex<Vec<(String, usize, usize)>>>, // (name, listener, cb fn)
        destroyed: Arc<Mutex<Vec<usize>>>,
    }
    unsafe impl Send for FakeMgr {}
    unsafe impl Sync for FakeMgr {}

    unsafe extern "C" fn fake_create_listener(
        mgr: *mut IWTSVirtualChannelManager,
        name: *const std::ffi::c_char,
        _flags: freerdp_sys::ULONG,
        cb: *mut IWTSListenerCallback,
        pp_listener: *mut *mut IWTSListener,
    ) -> UINT {
        let fake = unsafe { &*(mgr as *const FakeMgr) };
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        let cb_fn = unsafe { (*cb).OnNewChannelConnection }.map(|f| f as usize).unwrap_or(0);
        let listener = Box::into_raw(Box::new(IWTSListener {
            GetConfiguration: None,
            pInterface: std::ptr::null_mut(),
        }));
        fake.created.lock().unwrap().push((name, listener as usize, cb_fn));
        unsafe { *pp_listener = listener };
        CHANNEL_RC_OK
    }

    unsafe extern "C" fn fake_destroy_listener(
        mgr: *mut IWTSVirtualChannelManager,
        listener: *mut IWTSListener,
    ) -> UINT {
        let fake = unsafe { &*(mgr as *const FakeMgr) };
        fake.destroyed.lock().unwrap().push(listener as usize);
        let _ = unsafe { Box::from_raw(listener) };
        CHANNEL_RC_OK
    }

    impl FakeMgr {
        fn new() -> Box<Self> {
            Box::new(FakeMgr {
                inner: IWTSVirtualChannelManager {
                    CreateListener: Some(fake_create_listener),
                    GetChannelId: None,
                    FindChannelById: None,
                    GetChannelName: None,
                    DestroyListener: Some(fake_destroy_listener),
                },
                created: Arc::new(Mutex::new(Vec::new())),
                destroyed: Arc::new(Mutex::new(Vec::new())),
            })
        }
        fn ptr(&mut self) -> *mut IWTSVirtualChannelManager {
            &mut self.inner
        }
    }

    /// Minimal integration for control-path tests: only get_device_name and
    /// stop_stream are observable here; the rest are inert.
    #[derive(Debug)]
    struct ControlMock {
        name: String,
        stops: AtomicU32,
    }
    impl ControlMock {
        fn new(name: &str) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                stops: AtomicU32::new(0),
            })
        }
    }
    impl WebcamIntegration for ControlMock {
        fn is_h264_available(&self) -> bool {
            false
        }
        fn get_camera_dimensions(&self) -> (u32, u32) {
            (640, 480)
        }
        fn get_max_dimensions(&self) -> (u32, u32) {
            (640, 480)
        }
        fn get_fps(&self) -> u32 {
            15
        }
        fn set_mode(&self, _mode: WebcamMode) {}
        fn set_format(&self, _format: u32, _width: u32, _height: u32, _fps: u32) {}
        fn start_stream(&self, _w: u32, _h: u32, _fps: u32) -> flume::Receiver<WebcamFrame> {
            let (_tx, rx) = flume::unbounded();
            rx
        }
        fn stop_stream(&self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        fn request_sample(&self, _channel_ptr: usize) {}
        fn push_frame(&self, _data: Vec<u8>) {}
        fn get_device_name(&self) -> String {
            self.name.clone()
        }
    }

    fn send_control_pdu(ctx: *mut ControlChannelCtx, bytes: &[u8]) -> UINT {
        let mut stream = freerdp_sys::wStream {
            buffer: bytes.as_ptr() as *mut _,
            pointer: bytes.as_ptr() as *mut _,
            length: bytes.len(),
            capacity: bytes.len(),
            count: 1,
            pool: std::ptr::null_mut(),
            isAllocatedStream: 0,
            isOwner: 0,
        };
        unsafe { on_control_data(ctx as *mut _, &mut stream) }
    }

    /// Expected DeviceAddedNotification body for a device name.
    fn device_added_pdu(name: &str) -> Vec<u8> {
        let mut pdu = vec![1u8, 5u8];
        let mut utf16: Vec<u16> = name.encode_utf16().collect();
        utf16.push(0);
        for v in &utf16 {
            pdu.extend_from_slice(&v.to_le_bytes());
        }
        pdu.extend_from_slice(b"RDCamera_Capture_Device_0\0");
        pdu
    }

    struct ControlHarness {
        mock: Arc<ControlMock>,
        writes: WriteSink,
        created: Arc<Mutex<Vec<(String, usize, usize)>>>,
        destroyed: Arc<Mutex<Vec<usize>>>,
        lctx: *mut ControlListenerCtx,
        cctx: *mut ControlChannelCtx,
        // Must outlive any callback use; dropped after close().
        _channel: Box<FakeChannel>,
        _mgr: Box<FakeMgr>,
        closed: bool,
    }

    impl ControlHarness {
        fn new(name: &str) -> Self {
            let mock = ControlMock::new(name);
            let writes: WriteSink = Arc::new(Mutex::new(Vec::new()));
            let mut channel = FakeChannel::new(writes.clone());
            let mut mgr = FakeMgr::new();
            let created = mgr.created.clone();
            let destroyed = mgr.destroyed.clone();

            let lctx = Box::into_raw(Box::new(ControlListenerCtx {
                listener_cb: IWTSListenerCallback {
                    OnNewChannelConnection: Some(on_new_control_channel),
                    pInterface: std::ptr::null_mut(),
                },
                webcam: mock.clone(),
                channel_mgr: mgr.ptr(),
                rdpcontext: std::ptr::null_mut(),
            }));

            let mut accept: BOOL = 0;
            let mut cb: *mut IWTSVirtualChannelCallback = std::ptr::null_mut();
            let rc = unsafe {
                on_new_control_channel(
                    lctx as *mut _,
                    channel.ptr(),
                    std::ptr::null_mut(),
                    &mut accept,
                    &mut cb,
                )
            };
            assert_eq!(rc, CHANNEL_RC_OK);
            assert_ne!(accept, 0, "control channel must be accepted");
            assert!(!cb.is_null());

            Self {
                mock,
                writes,
                created,
                destroyed,
                lctx,
                cctx: cb as *mut ControlChannelCtx,
                _channel: channel,
                _mgr: mgr,
                closed: false,
            }
        }
        fn written(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }
        fn close(&mut self) -> UINT {
            self.closed = true;
            let rc = unsafe { on_control_close(self.cctx as *mut _) };
            unsafe {
                let _ = Box::from_raw(self.lctx);
            }
            rc
        }
    }

    impl Drop for ControlHarness {
        fn drop(&mut self) {
            if !self.closed {
                unsafe {
                    let _ = on_control_close(self.cctx as *mut _);
                    let _ = Box::from_raw(self.lctx);
                }
            }
        }
    }

    #[test]
    fn control_open_sends_select_version_request() {
        let mut h = ControlHarness::new("Test Cam");
        let rc = unsafe { on_control_open(h.cctx as *mut _) };
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_eq!(h.written(), vec![vec![1u8, 3u8]]);
        h.close();
    }

    #[test]
    fn version_response_sends_device_added_and_creates_device_listener() {
        let mut h = ControlHarness::new("Test Cam");
        let rc = send_control_pdu(h.cctx, &[1, 4]); // SelectVersionResponse
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_eq!(h.written(), vec![device_added_pdu("Test Cam")]);

        let created = h.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "RDCamera_Capture_Device_0");
        assert_eq!(created[0].2, on_new_device_channel as usize);
        unsafe {
            assert!((*h.cctx).dev_listener.is_some());
            assert!((*h.cctx).dev_listener_ctx.is_some());
        }
        drop(created);
        h.close();
    }

    #[test]
    fn default_device_name_is_uds_camera() {
        let mut h = ControlHarness::new("UDS Camera");
        send_control_pdu(h.cctx, &[1, 4]);
        assert_eq!(h.written(), vec![device_added_pdu("UDS Camera")]);
        h.close();
    }

    #[test]
    fn unicode_device_name_encoded_utf16le() {
        let mut h = ControlHarness::new("Cámara Ñ");
        send_control_pdu(h.cctx, &[1, 4]);
        assert_eq!(h.written(), vec![device_added_pdu("Cámara Ñ")]);
        h.close();
    }

    #[test]
    fn other_control_msgs_are_noops() {
        let mut h = ControlHarness::new("Test Cam");
        assert_eq!(send_control_pdu(h.cctx, &[1, 99]), CHANNEL_RC_OK);
        assert_eq!(send_control_pdu(h.cctx, &[1, 1]), CHANNEL_RC_OK);
        assert!(h.written().is_empty());
        assert!(h.created.lock().unwrap().is_empty());
        h.close();
    }

    #[test]
    fn short_or_null_control_pdus_never_panic() {
        let mut h = ControlHarness::new("Test Cam");
        assert_eq!(send_control_pdu(h.cctx, &[]), CHANNEL_RC_OK);
        assert_eq!(send_control_pdu(h.cctx, &[1]), CHANNEL_RC_OK);
        let rc = unsafe { on_control_data(h.cctx as *mut _, std::ptr::null_mut()) };
        assert_eq!(rc, CHANNEL_RC_OK);
        assert!(h.written().is_empty());
        h.close();
    }

    #[test]
    fn control_close_destroys_device_listener() {
        let mut h = ControlHarness::new("Test Cam");
        send_control_pdu(h.cctx, &[1, 4]);
        let listener_ptr = h.created.lock().unwrap()[0].1;
        let rc = h.close();
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_eq!(*h.destroyed.lock().unwrap(), vec![listener_ptr]);
    }

    #[test]
    fn control_close_without_listener_destroys_nothing() {
        let mut h = ControlHarness::new("Test Cam");
        let rc = h.close();
        assert_eq!(rc, CHANNEL_RC_OK);
        assert!(h.destroyed.lock().unwrap().is_empty());
    }

    #[test]
    fn create_listener_registers_enumerator_with_control_callbacks() {
        let mock = ControlMock::new("Test Cam");
        let mut mgr = FakeMgr::new();
        let (raw_ctx, handle, error) =
            create_listener(mock.clone(), mgr.ptr(), std::ptr::null_mut());
        assert_eq!(error, CHANNEL_RC_OK);
        assert!(!handle.is_null());
        let created = mgr.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "RDCamera_Device_Enumerator");
        assert_eq!(created[0].2, on_new_control_channel as usize);
        drop(created);
        // Reclaim like the plugin teardown does.
        unsafe {
            let _ = Box::from_raw(raw_ctx);
            if let Some(destroy) = mgr.inner.DestroyListener {
                destroy(mgr.ptr(), handle);
            }
        }
    }

    #[test]
    fn device_channel_accept_wires_device_callbacks() {
        let mock = ControlMock::new("Test Cam");
        let writes: WriteSink = Arc::new(Mutex::new(Vec::new()));
        let mut channel = FakeChannel::new(writes);
        let dev_ctx = Box::into_raw(Box::new(DeviceListenerCtx {
            listener_cb: IWTSListenerCallback {
                OnNewChannelConnection: Some(on_new_device_channel),
                pInterface: std::ptr::null_mut(),
            },
            webcam: mock.clone(),
            rdpcontext: std::ptr::null_mut(),
        }));
        let mut accept: BOOL = 0;
        let mut cb: *mut IWTSVirtualChannelCallback = std::ptr::null_mut();
        let rc = unsafe {
            on_new_device_channel(
                dev_ctx as *mut _,
                channel.ptr(),
                std::ptr::null_mut(),
                &mut accept,
                &mut cb,
            )
        };
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_ne!(accept, 0);
        unsafe {
            assert_eq!(
                (*cb).OnDataReceived.map(|f| f as usize),
                Some(channel::on_data as usize)
            );
            assert_eq!(
                (*cb).OnOpen.map(|f| f as usize),
                Some(channel::on_open as usize)
            );
            assert_eq!(
                (*cb).OnClose.map(|f| f as usize),
                Some(channel::on_close as usize)
            );
            // Free the device channel ctx through its real close path.
            let rc = channel::on_close(cb);
            assert_eq!(rc, CHANNEL_RC_OK);
            let _ = Box::from_raw(dev_ctx);
        }
        assert_eq!(mock.stops.load(Ordering::SeqCst), 1);
    }
}
