// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

use std::sync::Arc;

use crate::integrations::{WebcamIntegration, WebcamMode};
use crate::utils::log;
use freerdp_sys::{CHANNEL_RC_OK, IWTSVirtualChannel, IWTSVirtualChannelCallback, UINT};

use super::pdu::{self, parse_pdu_header, write_to_channel};

#[repr(C)]
pub struct ChannelCtx {
    pub channel_cb: IWTSVirtualChannelCallback,
    pub channel: *mut IWTSVirtualChannel,
    pub webcam: Arc<dyn WebcamIntegration>,
    pub stream_index: u8,
    pub rdpcontext: *mut freerdp_sys::rdpContext,
}

pub unsafe extern "C" fn on_data(
    cb: *mut IWTSVirtualChannelCallback,
    stream: *mut freerdp_sys::wStream,
) -> UINT {
    let ctx = unsafe { &*(cb as *const ChannelCtx) };
    if stream.is_null() {
        return CHANNEL_RC_OK;
    }

    let s = unsafe { &*stream };
    let bytes = unsafe { std::slice::from_raw_parts(s.pointer, s.length) };

    let Ok((_version, msg_id, payload)) = parse_pdu_header(bytes) else {
        log::error!("Webcam: failed to parse PDU header!");
        return CHANNEL_RC_OK;
    };

    log::trace!("Webcam received msg_id: {}", msg_id);

    #[allow(non_upper_case_globals)]
    match msg_id as freerdp_sys::CAM_MSG_ID {
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_SampleRequest => {
            ctx.webcam.request_sample(ctx.channel as usize);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_StartStreamsRequest => {
            handle_start_streams(ctx, payload);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_StopStreamsRequest => {
            log::info!("Webcam: StopStreamsRequest");
            ctx.webcam.stop_stream();
            send_success(ctx);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_ActivateDeviceRequest => {
            log::info!("Webcam: ActivateDeviceRequest");
            send_success(ctx);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_DeactivateDeviceRequest => {
            log::info!("Webcam: DeactivateDeviceRequest");
            ctx.webcam.stop_stream();
            send_success(ctx);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_StreamListRequest => {
            handle_stream_list(ctx);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_MediaTypeListRequest => {
            handle_media_type_list(ctx);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_CurrentMediaTypeRequest => {
            handle_current_media_type(ctx);
        }
        freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_PropertyListRequest => {
            handle_property_list(ctx);
        }
        _ => {
            log::warn!("Webcam: unknown msg_id={msg_id}, sending error");
            send_error(ctx);
        }
    }

    CHANNEL_RC_OK
}

fn get_overridden_format() -> Option<freerdp_sys::CAM_MEDIA_FORMAT> {
    if let Ok(val) = std::env::var("UDSLAUNCHER_CAM_FORMAT") {
        let val_lower = val.to_lowercase();
        if val_lower == "h264" {
            Some(freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264)
        } else if val_lower == "mjpeg" || val_lower == "mjpg" {
            Some(freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_MJPG)
        } else if val_lower == "yuy2" {
            Some(freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_YUY2)
        } else {
            None
        }
    } else {
        None
    }
}

fn handle_start_streams(ctx: &ChannelCtx, payload: &[u8]) {
    if payload.len() < 27 {
        log::error!("Webcam: StartStreams payload too short: {}", payload.len());
        send_error(ctx);
        return;
    }
    let stream_idx = payload[0];
    let media_type = match pdu::parse_media_type(&payload[1..]) {
        Ok(mt) => mt,
        Err(e) => {
            log::error!("Webcam: failed to parse media type: {e}");
            send_error(ctx);
            return;
        }
    };
    let mut format = media_type.Format;
    let width = media_type.Width;
    let height = media_type.Height;
    let requested_fps = media_type
        .FrameRateNumerator
        .checked_div(media_type.FrameRateDenominator)
        .unwrap_or(15);
    let configured_fps = ctx.webcam.get_fps();
    let fps = requested_fps.min(configured_fps);

    if let Some(override_fmt) = get_overridden_format() {
        log::info!("Webcam: Overriding format {} to {}", format, override_fmt);
        format = override_fmt as freerdp_sys::CAM_MEDIA_FORMAT;
    }

    log::info!(
        "Webcam: StartStreams — stream={stream_idx} format={format} {}x{} @ {fps}fps (requested: {requested_fps}fps, configured: {configured_fps}fps)",
        width,
        height
    );

    if format == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264
        && ctx.webcam.is_h264_available()
    {
        ctx.webcam.set_mode(WebcamMode::H264);
    } else if format == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_MJPG {
        ctx.webcam.set_mode(WebcamMode::MJPEG);
    } else if format == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_YUY2 {
        ctx.webcam.set_mode(WebcamMode::YUY2);
    } else {
        if format == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264 {
            log::warn!(
                "Webcam: Server requested H264 but OpenH264 is unavailable. Falling back to MJPEG."
            );
            ctx.webcam.set_mode(WebcamMode::MJPEG);
        } else {
            ctx.webcam.set_mode(WebcamMode::Raw);
        }
    }

    let rx = ctx.webcam.start_stream(width, height, fps);
    ctx.webcam.set_format(format as u32, width, height, fps);

    std::thread::spawn(move || {
        log::debug!("Webcam: Frame writer thread started for stream {stream_idx}");
        while let Ok(frame) = rx.recv() {
            log::trace!(
                "Webcam: Sending sample response of {} bytes, stream_idx={stream_idx}",
                frame.data.len()
            );
            let pdu = pdu::build_sample_response(stream_idx, &frame.data);
            unsafe { pdu::write_to_channel(frame.channel_ptr as *mut _, &pdu) };
        }
        log::debug!("Webcam: Frame writer thread stopped for stream {stream_idx}");
    });

    send_success(ctx);
}

fn handle_stream_list(ctx: &ChannelCtx) {
    log::info!("Webcam: StreamListRequest — reporting 1 stream");
    let pdu = pdu::build_stream_list_response(1, 1, true, false);
    unsafe { write_to_channel(ctx.channel, &pdu) };
}

fn handle_media_type_list(ctx: &ChannelCtx) {
    let h264_supported = ctx.webcam.is_h264_available();
    log::info!(
        "Webcam: MediaTypeListRequest — H264 supported: {}",
        h264_supported
    );

    let (mut width, mut height) = ctx.webcam.get_camera_dimensions();
    let (max_w, max_h) = ctx.webcam.get_max_dimensions();
    if max_w > 0 && width > max_w {
        width = max_w;
    }
    if max_h > 0 && height > max_h {
        height = max_h;
    }

    let fps = ctx.webcam.get_fps().max(1);

    let mjpeg_mt = freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION {
        Format: freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_MJPG,
        Width: width,
        Height: height,
        FrameRateNumerator: fps,
        FrameRateDenominator: 1,
        PixelAspectRatioNumerator: 1,
        PixelAspectRatioDenominator: 1,
        Flags: freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION_FLAGS_CAM_MEDIA_TYPE_DESCRIPTION_FLAG_DecodingRequired,
    };

    let h264_mt = freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION {
        Format: freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264,
        Width: width,
        Height: height,
        FrameRateNumerator: fps,
        FrameRateDenominator: 1,
        PixelAspectRatioNumerator: 1,
        PixelAspectRatioDenominator: 1,
        Flags: freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION_FLAGS_CAM_MEDIA_TYPE_DESCRIPTION_FLAG_DecodingRequired,
    };

    let pdu = if let Some(override_fmt) = get_overridden_format() {
        log::info!(
            "Webcam: Forcing media format list override: {}",
            override_fmt
        );
        if override_fmt == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264 && h264_supported {
            pdu::build_media_type_list_response(&[h264_mt])
        } else {
            pdu::build_media_type_list_response(&[mjpeg_mt])
        }
    } else if h264_supported {
        pdu::build_media_type_list_response(&[h264_mt, mjpeg_mt])
    } else {
        pdu::build_media_type_list_response(&[mjpeg_mt])
    };

    unsafe { write_to_channel(ctx.channel, &pdu) };
}

fn handle_current_media_type(ctx: &ChannelCtx) {
    let h264_supported = ctx.webcam.is_h264_available();
    log::info!(
        "Webcam: CurrentMediaTypeRequest — H264 supported: {}",
        h264_supported
    );

    let (mut width, mut height) = ctx.webcam.get_camera_dimensions();
    let (max_w, max_h) = ctx.webcam.get_max_dimensions();
    if max_w > 0 && width > max_w {
        width = max_w;
    }
    if max_h > 0 && height > max_h {
        height = max_h;
    }

    let fps = ctx.webcam.get_fps().max(1);

    let mut selected_fmt = if h264_supported {
        freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264
    } else {
        freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_MJPG
    };

    if let Some(override_fmt) = get_overridden_format() {
        log::info!(
            "Webcam: Forcing current media type override: {}",
            override_fmt
        );
        if override_fmt == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264 && h264_supported {
            selected_fmt = override_fmt;
        } else {
            selected_fmt = freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_MJPG;
        }
    }

    let mt = if selected_fmt == freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264 {
        freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION {
            Format: freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_H264,
            Width: width,
            Height: height,
            FrameRateNumerator: fps,
            FrameRateDenominator: 1,
            PixelAspectRatioNumerator: 1,
            PixelAspectRatioDenominator: 1,
            Flags: freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION_FLAGS_CAM_MEDIA_TYPE_DESCRIPTION_FLAG_DecodingRequired,
        }
    } else {
        freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION {
            Format: freerdp_sys::CAM_MEDIA_FORMAT_CAM_MEDIA_FORMAT_MJPG,
            Width: width,
            Height: height,
            FrameRateNumerator: fps,
            FrameRateDenominator: 1,
            PixelAspectRatioNumerator: 1,
            PixelAspectRatioDenominator: 1,
            Flags: freerdp_sys::CAM_MEDIA_TYPE_DESCRIPTION_FLAGS_CAM_MEDIA_TYPE_DESCRIPTION_FLAG_DecodingRequired,
        }
    };
    let pdu = pdu::build_current_media_type_response(&mt);
    unsafe { write_to_channel(ctx.channel, &pdu) };
}

fn handle_property_list(ctx: &ChannelCtx) {
    log::info!("Webcam: PropertyListRequest");
    let pdu = pdu::build_property_list_response();
    unsafe { write_to_channel(ctx.channel, &pdu) };
}

fn send_success(ctx: &ChannelCtx) {
    log::info!("Webcam: Sending SuccessResponse");
    let ok = pdu::build_response_header(freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_SuccessResponse as u8);
    unsafe { write_to_channel(ctx.channel, &ok) };
}

fn send_error(ctx: &ChannelCtx) {
    log::error!("Webcam: Sending ErrorResponse");
    let err = pdu::build_response_header(freerdp_sys::CAM_MSG_ID_CAM_MSG_ID_ErrorResponse as u8);
    unsafe { write_to_channel(ctx.channel, &err) };
}

pub unsafe extern "C" fn on_open(cb: *mut IWTSVirtualChannelCallback) -> UINT {
    let _ctx = cb as *mut ChannelCtx;
    log::debug!("Webcam Device: Channel opened");
    CHANNEL_RC_OK
}

pub unsafe extern "C" fn on_close(cb: *mut IWTSVirtualChannelCallback) -> UINT {
    let ctx = cb as *mut ChannelCtx;
    log::info!("Webcam Device: Channel closed");

    unsafe {
        (*ctx).webcam.stop_stream();
        let _ = Box::from_raw(ctx);
    }
    CHANNEL_RC_OK
}

#[cfg(test)]
mod tests {
    //! Characterization tests for the webcam device-channel contract as the
    //! external consumer (rdphtml5 WebcamHandle) relies on it. They pin the
    //! CURRENT observable behavior — exact wire PDUs, integration call
    //! sequence, return codes — so the lifetime fix (WriterGate quiesce in
    //! on_close) can be proven not to alter it. Test-only: no production
    //! change is made here.
    use super::*;
    use crate::integrations::{WebcamFrame, WebcamIntegration, WebcamMode};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    // Wire message ids (MS-RDPECAM), asserted numerically so a binding
    // constant rename cannot silently change what these tests pin.
    const SUCCESS: u8 = 1;
    const ERROR: u8 = 2;
    const ACTIVATE: u8 = 7;
    const DEACTIVATE: u8 = 8;
    const STREAM_LIST_REQ: u8 = 9;
    const STREAM_LIST_RESP: u8 = 10;
    const MEDIA_TYPE_LIST_REQ: u8 = 11;
    const MEDIA_TYPE_LIST_RESP: u8 = 12;
    const CURRENT_MEDIA_TYPE_REQ: u8 = 13;
    const CURRENT_MEDIA_TYPE_RESP: u8 = 14;
    const START_STREAMS: u8 = 15;
    const STOP_STREAMS: u8 = 16;
    const SAMPLE_REQ: u8 = 17;
    const SAMPLE_RESP: u8 = 18;
    const PROPERTY_LIST_REQ: u8 = 20;
    const PROPERTY_LIST_RESP: u8 = 21;

    const FMT_H264: u8 = 1;
    const FMT_MJPG: u8 = 2;
    const FMT_YUY2: u8 = 3;

    /// Fake host DVC channel. `Write` captures the exact PDU bytes into a
    /// per-channel sink recovered from the `pChannel` pointer itself (the
    /// fake is `#[repr(C)]` with the C struct first), so writes from ANY
    /// thread — including the detached writer thread — land in the right
    /// test's sink. No globals, tests run safely in parallel.
    ///
    /// `hold` (optional) lets a test deterministically park the writer
    /// thread INSIDE a frame `Write`: on entry the thread increments
    /// `inflight`, signals `entered`, then blocks until the test sets
    /// `release`. Only PDUs larger than `min_size` are held — the 2-byte
    /// Success/Error acks written synchronously by on_data must pass
    /// through. This models an in-flight `(*channel).Write` at the moment
    /// `on_close` runs — the exact UAF window — without any real freed
    /// memory.
    #[derive(Default)]
    struct WriteHold {
        enabled: std::sync::atomic::AtomicBool,
        min_size: usize,
        entered: Arc<(Mutex<bool>, std::sync::Condvar)>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        inflight: Arc<AtomicU32>,
    }

    #[repr(C)]
    struct FakeChannel {
        inner: freerdp_sys::IWTSVirtualChannel,
        sink: WriteSink,
        hold: Option<Arc<WriteHold>>,
    }

    // The fake is shared with the writer thread in these tests.
    unsafe impl Send for FakeChannel {}
    unsafe impl Sync for FakeChannel {}

    type WriteSink = Arc<Mutex<Vec<(usize, Vec<u8>)>>>;

    unsafe extern "C" fn fake_write(
        channel: *mut freerdp_sys::IWTSVirtualChannel,
        size: freerdp_sys::ULONG,
        buffer: *const freerdp_sys::BYTE,
        _reserved: *mut std::ffi::c_void,
    ) -> freerdp_sys::UINT {
        // Clone the Arcs out FIRST: pre-fix the harness may drop the fake
        // while this call is still running (that is the UAF under test).
        // Holding our own refs keeps sink/hold alive for the rest of the
        // call, so the RED run fails on assertions, not on freed memory.
        let (sink, hold) = unsafe {
            let fake = &*(channel as *const FakeChannel);
            (fake.sink.clone(), fake.hold.clone())
        };
        let bytes = unsafe { std::slice::from_raw_parts(buffer, size as usize) }.to_vec();
        if let Some(hold) = &hold {
            if hold.enabled.load(Ordering::SeqCst) && size as usize >= hold.min_size.max(1) {
                hold.inflight.fetch_add(1, Ordering::SeqCst);
                let (lock, cvar) = &*hold.entered;
                *lock.lock().unwrap() = true;
                cvar.notify_all();
                let (rlock, rcvar) = &*hold.release;
                let mut released = rlock.lock().unwrap();
                while !*released {
                    released = rcvar.wait(released).unwrap();
                }
                hold.inflight.fetch_sub(1, Ordering::SeqCst);
            }
        }
        sink.lock().unwrap().push((channel as usize, bytes));
        freerdp_sys::CHANNEL_RC_OK
    }

    impl FakeChannel {
        fn new(sink: WriteSink) -> Box<Self> {
            Self::with_hold(sink, None)
        }
        fn with_hold(sink: WriteSink, hold: Option<Arc<WriteHold>>) -> Box<Self> {
            Box::new(FakeChannel {
                inner: freerdp_sys::IWTSVirtualChannel {
                    Write: Some(fake_write),
                    Close: None,
                },
                sink,
                hold,
            })
        }
        fn ptr(&mut self) -> *mut freerdp_sys::IWTSVirtualChannel {
            &mut self.inner
        }
    }

    /// Records every integration call in order; `start_stream` hands back a
    /// real flume receiver so the writer-thread path is exercised end to end.
    #[derive(Debug)]
    struct MockWebcam {
        calls: Mutex<Vec<String>>,
        h264: bool,
        dims: (u32, u32),
        max_dims: (u32, u32),
        fps: u32,
        frame_tx: Mutex<Option<flume::Sender<WebcamFrame>>>,
        stop_count: AtomicU32,
    }

    impl MockWebcam {
        fn new(h264: bool, dims: (u32, u32), max_dims: (u32, u32), fps: u32) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                h264,
                dims,
                max_dims,
                fps,
                frame_tx: Mutex::new(None),
                stop_count: AtomicU32::new(0),
            })
        }
        fn record(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn send_frame(&self, data: Vec<u8>, channel_ptr: usize) {
            let guard = self.frame_tx.lock().unwrap();
            guard
                .as_ref()
                .expect("start_stream must have been called")
                .send(WebcamFrame { data, channel_ptr })
                .expect("writer thread must still be receiving");
        }
    }

    impl WebcamIntegration for MockWebcam {
        fn is_h264_available(&self) -> bool {
            self.h264
        }
        fn get_camera_dimensions(&self) -> (u32, u32) {
            self.dims
        }
        fn get_max_dimensions(&self) -> (u32, u32) {
            self.max_dims
        }
        fn get_fps(&self) -> u32 {
            self.fps
        }
        fn set_mode(&self, mode: WebcamMode) {
            self.record(format!("set_mode:{mode:?}"));
        }
        fn set_format(&self, format: u32, width: u32, height: u32, fps: u32) {
            self.record(format!("set_format:{format},{width},{height},{fps}"));
        }
        fn start_stream(&self, width: u32, height: u32, fps: u32) -> flume::Receiver<WebcamFrame> {
            self.record(format!("start_stream:{width},{height},{fps}"));
            let (tx, rx) = flume::unbounded();
            *self.frame_tx.lock().unwrap() = Some(tx);
            rx
        }
        fn stop_stream(&self) {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            *self.frame_tx.lock().unwrap() = None;
            self.record("stop_stream".to_string());
        }
        fn request_sample(&self, channel_ptr: usize) {
            self.record(format!("request_sample:{channel_ptr}"));
        }
        fn push_frame(&self, data: Vec<u8>) {
            self.record(format!("push_frame:{}", data.len()));
        }
    }

    /// Heap-allocated ChannelCtx wired like `on_new_device_channel` does.
    /// Returns the raw callback pointer; tests own it and must call
    /// `on_close` (or free manually) exactly once.
    fn make_ctx(
        webcam: Arc<MockWebcam>,
        channel: *mut freerdp_sys::IWTSVirtualChannel,
    ) -> *mut ChannelCtx {
        let ctx = Box::new(ChannelCtx {
            channel_cb: freerdp_sys::IWTSVirtualChannelCallback {
                OnDataReceived: Some(on_data),
                OnOpen: Some(on_open),
                OnClose: Some(on_close),
                pInterface: std::ptr::null_mut(),
            },
            channel,
            webcam,
            stream_index: 0,
            rdpcontext: std::ptr::null_mut(),
        });
        Box::into_raw(ctx)
    }

    fn send_pdu(ctx: *mut ChannelCtx, bytes: &[u8]) -> freerdp_sys::UINT {
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
        unsafe { on_data(ctx as *mut _, &mut stream) }
    }

    /// StartStreams payload: version + msg id + stream index + 26-byte media
    /// type description (29 bytes on the wire; handler requires payload >= 27).
    fn start_streams_payload(idx: u8, fmt: u8, w: u32, h: u32, num: u32, den: u32) -> Vec<u8> {
        let mut p = vec![1, START_STREAMS, idx, fmt];
        p.extend_from_slice(&w.to_le_bytes());
        p.extend_from_slice(&h.to_le_bytes());
        p.extend_from_slice(&num.to_le_bytes());
        p.extend_from_slice(&den.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes()); // aspect num
        p.extend_from_slice(&1u32.to_le_bytes()); // aspect den
        p.push(1); // DecodingRequired
        p
    }

    fn header(msg: u8) -> Vec<u8> {
        vec![1, msg]
    }

    /// Serialize the expected 26-byte media type entry for response PDUs.
    fn mt_bytes(fmt: u8, w: u32, h: u32, fps: u32) -> Vec<u8> {
        let mut b = vec![fmt];
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&fps.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(1);
        b
    }

    struct Harness {
        ctx: *mut ChannelCtx,
        mock: Arc<MockWebcam>,
        writes: WriteSink,
        channel_ptr: usize,
        // MUST outlive the writer thread: dropped only after close() has
        // quiesced it (post-fix). Option so guarantee tests can leak the
        // fake (pre-fix the writer may still touch it after close).
        _channel: Option<Box<FakeChannel>>,
        closed: bool,
    }

    impl Harness {
        fn new(mock: Arc<MockWebcam>) -> Self {
            Self::with_hold(mock, None)
        }
        fn with_hold(mock: Arc<MockWebcam>, hold: Option<Arc<WriteHold>>) -> Self {
            let writes: WriteSink = Arc::new(Mutex::new(Vec::new()));
            let mut channel = FakeChannel::with_hold(writes.clone(), hold);
            let channel_ptr = channel.ptr() as usize;
            let ctx = make_ctx(mock.clone(), channel.ptr());
            Self {
                ctx,
                mock,
                writes,
                channel_ptr,
                _channel: Some(channel),
                closed: false,
            }
        }
        fn send(&self, bytes: &[u8]) -> freerdp_sys::UINT {
            send_pdu(self.ctx, bytes)
        }
        fn written(&self) -> Vec<Vec<u8>> {
            self.writes
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| *p == self.channel_ptr)
                .map(|(_, b)| b.clone())
                .collect()
        }
        fn close(&mut self) -> freerdp_sys::UINT {
            self.closed = true;
            unsafe { on_close(self.ctx as *mut _) }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            if !self.closed {
                unsafe {
                    let _ = Box::from_raw(self.ctx);
                }
            }
        }
    }

    /// Wait until the writer thread has delivered `n` PDUs (bounded).
    fn wait_writes(h: &Harness, n: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while h.written().len() < n {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {n} writes, got {}",
                h.written().len()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Pre-fix, on_close frees the ctx while the writer thread may still be
    /// running (the very UAF under test). Give the detached writer a moment
    /// to exit (rx disconnects on stop_stream) before the harness drops the
    /// fake channel, so the RED run fails on assertions, not on allocator
    /// corruption. Post-fix this is a no-op wait: the writer is already
    /// quiesced when close returns.
    fn settle_writer() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    /// Leak the fake channel instead of freeing it. Pre-fix the writer
    /// thread can still be inside `(*channel).Write` when the test ends —
    /// freeing the fake then corrupts the allocator and masks the intended
    /// assertion failure with SIGABRT. Leaking one small struct per
    /// guarantee test keeps the RED run a clean FAILED. Post-fix nothing
    /// references the channel after close, so this is merely conservative.
    fn leak_fake(h: &mut Harness) {
        if let Some(fake) = h._channel.take() {
            let _ = Box::into_raw(fake);
        }
    }

    #[test]
    fn start_streams_mjpeg_full_contract() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        // host asks 30fps; configured 15 -> min = 15
        let p = start_streams_payload(0, FMT_MJPG, 320, 240, 30, 1);
        assert_eq!(h.send(&p), freerdp_sys::CHANNEL_RC_OK);
        assert_eq!(
            h.mock.calls(),
            vec![
                "set_mode:MJPEG",
                "start_stream:320,240,15",
                "set_format:2,320,240,15",
            ]
        );
        assert_eq!(h.written(), vec![header(SUCCESS)]);
        h.close();
    }

    #[test]
    fn start_streams_fps_min_and_den_zero_default() {
        // requested below configured -> requested wins
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 30);
        let mut h = Harness::new(mock.clone());
        let p = start_streams_payload(1, FMT_MJPG, 160, 120, 10, 1);
        h.send(&p);
        assert!(h.mock.calls().contains(&"start_stream:160,120,10".to_string()));
        h.close();

        // denominator 0 -> requested falls back to 15, min(15, configured 30) = 15
        let mock2 = MockWebcam::new(false, (640, 480), (640, 480), 30);
        let mut h2 = Harness::new(mock2.clone());
        let p2 = start_streams_payload(0, FMT_MJPG, 160, 120, 99, 0);
        h2.send(&p2);
        assert!(h2.mock.calls().contains(&"start_stream:160,120,15".to_string()));
        h2.close();
    }

    #[test]
    fn start_streams_h264_when_available_and_fallback_when_not() {
        let mock = MockWebcam::new(true, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&start_streams_payload(0, FMT_H264, 320, 240, 15, 1));
        assert!(h.mock.calls().contains(&"set_mode:H264".to_string()));
        assert!(h.mock.calls().contains(&"set_format:1,320,240,15".to_string()));
        h.close();

        let mock2 = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h2 = Harness::new(mock2.clone());
        h2.send(&start_streams_payload(0, FMT_H264, 320, 240, 15, 1));
        assert!(h2.mock.calls().contains(&"set_mode:MJPEG".to_string()));
        // format stays H264 on the wire contract (only mode falls back)
        assert!(h2.mock.calls().contains(&"set_format:1,320,240,15".to_string()));
        h2.close();
    }

    #[test]
    fn start_streams_yuy2_and_unknown_format() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&start_streams_payload(0, FMT_YUY2, 320, 240, 15, 1));
        assert!(h.mock.calls().contains(&"set_mode:YUY2".to_string()));
        h.close();

        let mock2 = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h2 = Harness::new(mock2.clone());
        h2.send(&start_streams_payload(0, 99, 320, 240, 15, 1));
        assert!(h2.mock.calls().contains(&"set_mode:Raw".to_string()));
        h2.close();
    }

    #[test]
    fn start_streams_short_payload_sends_error_no_stream() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        // header + 26 bytes of payload (< 27 required)
        let mut p = vec![1, START_STREAMS];
        p.extend_from_slice(&[0u8; 26]);
        assert_eq!(h.send(&p), freerdp_sys::CHANNEL_RC_OK);
        assert_eq!(h.written(), vec![header(ERROR)]);
        assert!(h.mock.calls().is_empty());
        h.close();
    }

    #[test]
    fn sample_request_forwards_channel_ptr() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        let ptr = h.channel_ptr;
        h.send(&header(SAMPLE_REQ));
        assert_eq!(h.mock.calls(), vec![format!("request_sample:{ptr}")]);
        assert!(h.written().is_empty()); // no response PDU for SampleRequest
        h.close();
    }

    #[test]
    fn stop_and_deactivate_stop_stream_and_ack() {
        for msg in [STOP_STREAMS, DEACTIVATE] {
            let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
            let mut h = Harness::new(mock.clone());
            h.send(&header(msg));
            assert_eq!(h.mock.calls(), vec!["stop_stream"]);
            assert_eq!(h.written(), vec![header(SUCCESS)]);
            assert_eq!(h.mock.stop_count.load(Ordering::SeqCst), 1);
            h.close();
        }
    }

    #[test]
    fn activate_acks_without_stopping() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&header(ACTIVATE));
        assert!(h.mock.calls().is_empty());
        assert_eq!(h.written(), vec![header(SUCCESS)]);
        h.close();
    }

    #[test]
    fn stream_list_response_exact_bytes() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&header(STREAM_LIST_REQ));
        let mut expected = vec![1, STREAM_LIST_RESP];
        expected.extend_from_slice(&1u16.to_le_bytes()); // frame source types
        expected.push(1); // category
        expected.push(1); // selected
        expected.push(0); // can_be_shared
        assert_eq!(h.written(), vec![expected]);
        h.close();
    }

    #[test]
    fn media_type_list_clamps_to_max_and_lists_h264_first() {
        // camera 1920x1080 but max 640x480 -> clamped; fps.max(1)
        let mock = MockWebcam::new(true, (1920, 1080), (640, 480), 0);
        let mut h = Harness::new(mock.clone());
        h.send(&header(MEDIA_TYPE_LIST_REQ));
        let mut expected = vec![1, MEDIA_TYPE_LIST_RESP];
        expected.extend_from_slice(&mt_bytes(FMT_H264, 640, 480, 1));
        expected.extend_from_slice(&mt_bytes(FMT_MJPG, 640, 480, 1));
        assert_eq!(h.written(), vec![expected]);
        h.close();
    }

    #[test]
    fn media_type_list_without_h264_only_mjpeg() {
        let mock = MockWebcam::new(false, (320, 240), (640, 480), 20);
        let mut h = Harness::new(mock.clone());
        h.send(&header(MEDIA_TYPE_LIST_REQ));
        let mut expected = vec![1, MEDIA_TYPE_LIST_RESP];
        expected.extend_from_slice(&mt_bytes(FMT_MJPG, 320, 240, 20));
        assert_eq!(h.written(), vec![expected]);
        h.close();
    }

    #[test]
    fn current_media_type_prefers_h264_and_clamps() {
        let mock = MockWebcam::new(true, (800, 600), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&header(CURRENT_MEDIA_TYPE_REQ));
        let mut expected = vec![1, CURRENT_MEDIA_TYPE_RESP];
        expected.extend_from_slice(&mt_bytes(FMT_H264, 640, 480, 15));
        assert_eq!(h.written(), vec![expected]);
        h.close();

        let mock2 = MockWebcam::new(false, (800, 600), (640, 480), 15);
        let mut h2 = Harness::new(mock2.clone());
        h2.send(&header(CURRENT_MEDIA_TYPE_REQ));
        let mut expected2 = vec![1, CURRENT_MEDIA_TYPE_RESP];
        expected2.extend_from_slice(&mt_bytes(FMT_MJPG, 640, 480, 15));
        assert_eq!(h2.written(), vec![expected2]);
        h2.close();
    }

    #[test]
    fn property_list_response_exact_bytes() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&header(PROPERTY_LIST_REQ));
        let mut expected = vec![1, PROPERTY_LIST_RESP];
        expected.extend_from_slice(&0u64.to_le_bytes());
        assert_eq!(h.written(), vec![expected]);
        h.close();
    }

    #[test]
    fn unknown_msg_sends_error() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&header(250));
        assert_eq!(h.written(), vec![header(ERROR)]);
        h.close();
    }

    #[test]
    fn malformed_pdus_never_panic() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        assert_eq!(h.send(&[]), freerdp_sys::CHANNEL_RC_OK); // too short
        assert_eq!(h.send(&[1]), freerdp_sys::CHANNEL_RC_OK); // 1 byte
        assert!(h.written().is_empty());
        // null stream is tolerated
        let rc = unsafe { on_data(h.ctx as *mut _, std::ptr::null_mut()) };
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        h.close();
    }

    #[test]
    fn frames_are_written_as_exact_sample_response_pdus() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        let ptr = h.channel_ptr;
        h.send(&start_streams_payload(3, FMT_MJPG, 320, 240, 15, 1));
        assert_eq!(h.written(), vec![header(SUCCESS)]);

        let data1 = vec![0xAAu8; 64];
        let data2 = vec![0xBBu8; 128];
        h.mock.send_frame(data1.clone(), ptr);
        h.mock.send_frame(data2.clone(), ptr);
        wait_writes(&h, 3); // Success + 2 samples

        let mut s1 = vec![1, SAMPLE_RESP, 3];
        s1.extend_from_slice(&data1);
        let mut s2 = vec![1, SAMPLE_RESP, 3];
        s2.extend_from_slice(&data2);
        assert_eq!(h.written(), vec![header(SUCCESS), s1, s2]);
        h.close();
    }

    #[test]
    fn on_close_stops_stream_and_returns_ok() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        h.send(&start_streams_payload(0, FMT_MJPG, 320, 240, 15, 1));
        let rc = h.close();
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        assert_eq!(h.mock.stop_count.load(Ordering::SeqCst), 1);
        assert!(h.mock.calls().contains(&"stop_stream".to_string()));
    }

    #[test]
    fn on_open_returns_ok() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        let rc = unsafe { on_open(h.ctx as *mut _) };
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        h.close();
    }

    // ── Lifetime guarantee (Fix A: writer quiesce on close) ──────────────
    //
    // These pin the NEW safety contract: when on_close returns, no frame
    // Write is in flight or will ever run again on this channel — upstream
    // dvcman frees the DVCMAN_CHANNEL right after OnClose returns (3.15 and
    // 3.26), so any later (*channel).Write is a use-after-free. They are
    // deterministic and memory-safe: the fake Write parks the writer thread
    // on a condvar instead of touching freed memory.
    //
    // Against UNFIXED code they FAIL (on_close returns while the writer is
    // still inside Write); against the WriterGate fix they PASS.

    /// Park the writer INSIDE Write, call on_close on another thread, and
    /// require it not to return while the write is in flight.
    ///
    /// The fake channel is leaked up front: pre-fix the writer may still
    /// be inside Write when the test ends, and freeing the fake then would
    /// corrupt the allocator (SIGABRT) instead of the intended assertion.
    #[test]
    fn on_close_waits_for_inflight_write() {
        let hold = Arc::new(WriteHold {
            min_size: 3, // hold frame PDUs only; 2-byte acks pass through
            ..Default::default()
        });
        hold.enabled.store(true, Ordering::SeqCst);
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::with_hold(mock.clone(), Some(hold.clone()));
        let ptr = h.channel_ptr;
        leak_fake(&mut h); // keep the fake alive no matter how this test exits
        h.send(&start_streams_payload(0, FMT_MJPG, 320, 240, 15, 1));

        // One frame -> writer enters Write and parks there.
        h.mock.send_frame(vec![0xAA; 32], ptr);
        let (lock, cvar) = &*hold.entered;
        let mut entered = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !*entered {
            let now = std::time::Instant::now();
            assert!(now < deadline, "writer never entered Write");
            let (g, _) = cvar
                .wait_timeout(entered, deadline - now)
                .unwrap();
            entered = g;
        }
        drop(entered);
        assert_eq!(hold.inflight.load(Ordering::SeqCst), 1);

        // Close on a separate thread (post-fix it blocks until quiesced).
        // Raw pointers are not Send; the ctx is exclusively owned by the
        // closer from here on (h.closed = true below).
        let ctx_addr = h.ctx as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rc = unsafe { on_close(ctx_addr as *mut IWTSVirtualChannelCallback) };
            let _ = done_tx.send(rc);
        });

        // Give the closer ample time to (incorrectly) return early.
        let early_return = done_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok();

        // ALWAYS release the parked write before asserting: pre-fix the
        // writer would otherwise stay parked forever. Post-fix the closer
        // is still waiting here and completes right after the release.
        let (rlock, rcvar) = &*hold.release;
        *rlock.lock().unwrap() = true;
        rcvar.notify_all();

        // Bounded wait for completion: if on_close never completes even
        // after the write was released, fail instead of hanging the suite.
        let rc = if early_return {
            // Already returned; result was consumed by recv_timeout above.
            freerdp_sys::CHANNEL_RC_OK
        } else {
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("on_close never returned after the in-flight write completed")
        };

        // Let the released writer fully exit before any assertion can
        // unwind (its rx disconnected on stop_stream).
        settle_writer();
        h.closed = true; // ctx freed by the closer thread
        assert!(
            !early_return,
            "on_close returned while a frame Write was still in flight — \
             upstream frees the channel right after OnClose: use-after-free"
        );
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        assert_eq!(hold.inflight.load(Ordering::SeqCst), 0);
    }

    /// Frames queued behind an in-flight write must be DRAINED, not written
    /// after close (their channel_ptr dies with the channel).
    #[test]
    fn on_close_drains_queued_frames_no_writes_after_close() {
        let hold = Arc::new(WriteHold {
            min_size: 3, // hold frame PDUs only; 2-byte acks pass through
            ..Default::default()
        });
        hold.enabled.store(true, Ordering::SeqCst);
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::with_hold(mock.clone(), Some(hold.clone()));
        let ptr = h.channel_ptr;
        h.send(&start_streams_payload(0, FMT_MJPG, 320, 240, 15, 1));

        // Frame 1 parks the writer inside Write; frames 2..4 queue behind.
        for i in 0..4u8 {
            h.mock.send_frame(vec![i; 32], ptr);
        }
        let (lock, cvar) = &*hold.entered;
        let mut entered = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !*entered {
            let now = std::time::Instant::now();
            assert!(now < deadline, "writer never entered Write");
            let (g, _) = cvar.wait_timeout(entered, deadline - now).unwrap();
            entered = g;
        }
        drop(entered);

        let ctx_addr = h.ctx as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rc = unsafe { on_close(ctx_addr as *mut IWTSVirtualChannelCallback) };
            let _ = done_tx.send(rc);
        });
        // Let on_close run its stop/drain while the writer is parked.
        std::thread::sleep(std::time::Duration::from_millis(100));

        let (rlock, rcvar) = &*hold.release;
        *rlock.lock().unwrap() = true;
        rcvar.notify_all();
        let rc = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("on_close never returned after the in-flight write completed");
        assert_eq!(rc, freerdp_sys::CHANNEL_RC_OK);
        h.closed = true;

        // Only the Success ack and the ONE in-flight frame may appear.
        // Pre-fix the writer drains the queue after release and writes
        // frames 1..3 too (5 writes) — wait for that to settle so the
        // assertion FAILS deterministically instead of racing.
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while h.written().len() < 5 && std::time::Instant::now() < settle {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let writes = h.written();
        // Settle the (pre-fix still running) writer BEFORE asserting so a
        // failing assert cannot race the harness drop into the allocator.
        settle_writer();
        leak_fake(&mut h);
        assert_eq!(writes.len(), 2, "queued frames must be drained, not written");
        assert_eq!(writes[0], header(SUCCESS));
        assert_eq!(writes[1][..3], [1, SAMPLE_RESP, 0]);
        assert_eq!(writes[1][3], 0u8); // frame 0, the in-flight one

        // And nothing ever arrives afterwards.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(h.written().len(), 2, "write after close = use-after-free");
    }

    /// A frame racing in AFTER stop_stream (consumer push_frame holds the
    /// sender a moment longer) must never reach the channel.
    #[test]
    fn late_frame_after_close_is_never_written() {
        let mock = MockWebcam::new(false, (640, 480), (640, 480), 15);
        let mut h = Harness::new(mock.clone());
        let ptr = h.channel_ptr;
        h.send(&start_streams_payload(0, FMT_MJPG, 320, 240, 15, 1));
        // Keep our own sender clone alive past stop_stream, like the
        // consumer's push_frame path can.
        let tx = mock.frame_tx.lock().unwrap().clone().unwrap();
        h.close();
        let _ = tx.send(WebcamFrame {
            data: vec![0xEE; 16],
            channel_ptr: ptr,
        });
        // Pre-fix the writer is still alive (sender kept open) and writes
        // the late frame — wait for that so the assertion FAILS
        // deterministically instead of racing.
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while h.written().len() < 2 && std::time::Instant::now() < settle {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let late_writes = h.written();
        // Reclaim the writer thread before asserting (and before the fake
        // channel is dropped): dropping our sender clone disconnects rx
        // and the loop exits.
        drop(tx);
        settle_writer();
        leak_fake(&mut h);
        assert_eq!(
            late_writes,
            vec![header(SUCCESS)],
            "a frame sent after close must never be written"
        );
    }
}
