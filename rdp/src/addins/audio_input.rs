// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

#![allow(dead_code)]
use std::sync::Arc;
use std::thread;

use freerdp_sys::{
    AUDIO_FORMAT, AudinReceive, BOOL, BYTE, CHANNEL_RC_OK, IAudinDevice,
    PFREERDP_AUDIN_DEVICE_ENTRY_POINTS, UINT, UINT32, WAVE_FORMAT_PCM,
};

use crate::context::OwnerFromCtx;
use crate::integrations::AudioInputIntegration;
use crate::utils::log;

#[repr(C)]
pub struct MicPlugin {
    iface: IAudinDevice,

    // Custom data
    rdpcontext: *mut freerdp_sys::rdpContext,
    format: AUDIO_FORMAT,
    frames_per_packet: u32,
    audio_input: Option<Arc<dyn AudioInputIntegration>>,
    stop_tx: Option<flume::Sender<()>>,
    /// Join handle for the reader thread. Close joins it so no receive
    /// callback is in flight when Close returns (upstream audin frees the
    /// plugin right after Close — FreeRDP 3.15 and 3.26).
    reader_join: Option<thread::JoinHandle<()>>,
}

/// Hard bound for joining the reader thread during close. The reader's
/// receive callback is a short host-stack call; 5 s is far beyond any
/// legitimate packet and keeps disconnect from stalling indefinitely.
const READER_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Library-side audio format envelope. The RDP HOST negotiates
/// sample_rate/bits/frames_per_packet; upstream FreeRDP stores them raw and
/// never gates them, so the library must reject out-of-envelope values where
/// they are ACCEPTED (set_format / format_supported), before the reader sizes
/// its per-chunk buffer from them (Vec::with_capacity(fps * bits/8 * 2)).
/// Same constants as the rdphtml5 consumer (PR-07): hi-res audio up to
/// 192 kHz, 8/16/32-bit, and FreeRDP's own OPUS_MAX_FRAMES = 5760.
const MAX_AUDIO_SAMPLE_RATE: u32 = 192_000;
const MAX_AUDIO_FRAMES_PER_PACKET: u32 = 5760;

/// Whether a host-negotiated mono PCM format is within the safe envelope.
/// Pure: unit-testable without a FreeRDP connection.
fn audio_format_in_envelope(sample_rate: u32, bits: u16, frames_per_packet: u32) -> bool {
    sample_rate != 0
        && sample_rate <= MAX_AUDIO_SAMPLE_RATE
        && matches!(bits, 8 | 16 | 32)
        && frames_per_packet != 0
        && frames_per_packet <= MAX_AUDIO_FRAMES_PER_PACKET
}

pub unsafe extern "C" fn mic_entry(p_entry_points: PFREERDP_AUDIN_DEVICE_ENTRY_POINTS) -> UINT {
    if p_entry_points.is_null() {
        return 1;
    }

    let rdpcontext = unsafe { (*p_entry_points).rdpcontext };

    let mut plugin = Box::new(MicPlugin {
        iface: IAudinDevice {
            Open: Some(open),
            FormatSupported: Some(format_supported),
            SetFormat: Some(set_format),
            Close: Some(close),
            Free: Some(free),
        },
        rdpcontext,
        format: unsafe { std::mem::zeroed() },
        frames_per_packet: 0,
        audio_input: None,
        stop_tx: None,
        reader_join: None,
    });

    unsafe {
        if let Some(register_fn) = (*p_entry_points).pRegisterAudinDevice {
            register_fn(
                (*p_entry_points).plugin,
                &mut plugin.iface as *mut IAudinDevice,
            );
        }
    }

    _ = Box::into_raw(plugin);
    log::debug!("Mic addin plugin registered");
    CHANNEL_RC_OK
}

unsafe extern "C" fn format_supported(
    _device: *mut IAudinDevice,
    format: *const AUDIO_FORMAT,
) -> BOOL {
    unsafe {
        if (*format).wFormatTag != WAVE_FORMAT_PCM as u16 {
            return false.into();
        }
        if (*format).nChannels != 1 {
            return false.into();
        }
        // Gate the format envelope here too: only 8/16/32-bit PCM mono is
        // admissible (the reader duplicates mono->stereo for these widths).
        if !matches!((*format).wBitsPerSample, 8 | 16 | 32) {
            return false.into();
        }
    }
    true.into()
}

unsafe extern "C" fn set_format(
    device: *mut IAudinDevice,
    format: *const AUDIO_FORMAT,
    frames_per_packet: UINT32,
) -> UINT {
    let plugin = unsafe { &mut *(device as *mut MicPlugin) };
    let mut fmt = unsafe { *format };
    // Reject out-of-envelope host values where they are ACCEPTED: upstream
    // treats SetFormat != CHANNEL_RC_OK as fatal (audin_main.c), so a hostile
    // fps/rate/bits never reaches the reader's per-chunk allocation.
    if !audio_format_in_envelope(fmt.nSamplesPerSec, fmt.wBitsPerSample, frames_per_packet) {
        log::error!(
            "Mic set_format rejected: rate={} bits={} fps={} outside the safe envelope",
            fmt.nSamplesPerSec,
            fmt.wBitsPerSample,
            frames_per_packet
        );
        return freerdp_sys::ERROR_INVALID_PARAMETER;
    }
    fmt.nBlockAlign = (fmt.wBitsPerSample / 8) * fmt.nChannels;
    fmt.nAvgBytesPerSec = fmt.nSamplesPerSec * fmt.nBlockAlign as u32;
    plugin.format = fmt;
    plugin.frames_per_packet = frames_per_packet;
    log::debug!(
        "Mic set_format: sample_rate={}, channels={}, bits={}, fps={}",
        fmt.nSamplesPerSec,
        fmt.nChannels,
        fmt.wBitsPerSample,
        frames_per_packet
    );

    if let Some(tx) = plugin
        .rdpcontext
        .owner()
        .and_then(|rdp| rdp.update_tx.as_ref())
    {
        let _ = tx.send(crate::messaging::RdpMessage::MicConfig {
            sample_rate: fmt.nSamplesPerSec,
            frames_per_packet,
        });
    }

    CHANNEL_RC_OK
}

unsafe extern "C" fn open(
    device: *mut IAudinDevice,
    receive: AudinReceive,
    user_data: *mut std::ffi::c_void,
) -> UINT {
    let plugin = unsafe { &mut *(device as *mut MicPlugin) };

    log::debug!("Mic device open called");

    if receive.is_none() {
        log::error!("Mic device open called with null receive callback");
        return CHANNEL_RC_OK;
    }

    let rdp = if let Some(rdp) = plugin.rdpcontext.owner() {
        rdp
    } else {
        log::error!("Failed to obtain Rdp owner context in mic open");
        return 1;
    };

    let audio_input_integration = if let Some(ref integration) = rdp.config.integrations.audio_input
    {
        integration
    } else {
        log::warn!("Audio input integration not configured");
        return CHANNEL_RC_OK;
    };

    let receive_cb = receive.unwrap();
    let user_data_usize = user_data as usize;
    let fps = plugin.frames_per_packet;
    let sample_rate = plugin.format.nSamplesPerSec;
    let bits = plugin.format.wBitsPerSample;
    let channels = plugin.format.nChannels;

    let data_rx = match audio_input_integration.start(sample_rate, channels, bits, fps) {
        Ok(rx) => rx,
        Err(e) => {
            log::error!("Failed to start audio input integration: {}", e);
            return 1;
        }
    };

    if let Some(ref tx) = rdp.update_tx {
        let _ = tx.send(crate::messaging::RdpMessage::MicConfig {
            sample_rate,
            frames_per_packet: fps,
        });
    }

    plugin.audio_input = Some(audio_input_integration.clone());
    let (stop_tx, stop_rx) = flume::bounded(1);
    plugin.stop_tx = Some(stop_tx);

    let format_tag = plugin.format.wFormatTag;
    let sample_rate_val = plugin.format.nSamplesPerSec;
    let bits_val = plugin.format.wBitsPerSample;
    let block_align = plugin.format.nBlockAlign * 2;
    let avg_bps = plugin.format.nAvgBytesPerSec * 2;

    let reader = thread::spawn(move || {
        let ud = user_data_usize as *mut std::ffi::c_void;

        let stereo_fmt = AUDIO_FORMAT {
            wFormatTag: format_tag,
            nChannels: 2,
            nSamplesPerSec: sample_rate_val,
            nAvgBytesPerSec: avg_bps,
            nBlockAlign: block_align,
            wBitsPerSample: bits_val,
            cbSize: 0,
            data: std::ptr::null_mut(),
        };

        if sample_rate == 0 || fps == 0 {
            log::error!("Mic reader: invalid format or frames_per_packet");
            return;
        }

        let duration =
            std::time::Duration::from_nanos((fps as u64 * 1_000_000_000) / sample_rate as u64);
        let mono_packet_bytes = fps as usize * (bits as usize / 8);
        let stereo_packet_bytes = mono_packet_bytes * 2;

        log::info!(
            "Mic reader thread started: rate={}, frames={}, duration={:?}",
            sample_rate,
            fps,
            duration
        );

        loop {
            // Wait for a data packet OR the stop signal, whichever comes
            // first: close must wake the reader even if the consumer keeps
            // its sender alive with no data flowing.
            let mono_data = {
                let mut slot: Option<Vec<u8>> = None;
                let mut stopped = false;
                flume::Selector::new()
                    .recv(&data_rx, |r| {
                        if let Ok(d) = r {
                            slot = Some(d);
                        }
                    })
                    .recv(&stop_rx, |_| {
                        stopped = true;
                    })
                    .wait();
                if stopped {
                    break;
                }
                match slot {
                    Some(d) => d,
                    None => break, // data channel disconnected
                }
            };
            if stop_rx.try_recv().is_ok() {
                break;
            }

            let mut stereo_data = Vec::with_capacity(stereo_packet_bytes);
            let bytes_per_sample = (bits as usize) / 8;
            for i in 0..fps as usize {
                let start = i * bytes_per_sample;
                let end = start + bytes_per_sample;
                if end <= mono_data.len() {
                    stereo_data.extend_from_slice(&mono_data[start..end]);
                    stereo_data.extend_from_slice(&mono_data[start..end]);
                } else {
                    let silence = vec![0u8; bytes_per_sample];
                    stereo_data.extend_from_slice(&silence);
                    stereo_data.extend_from_slice(&silence);
                }
            }

            unsafe {
                let res = (receive_cb)(
                    &stereo_fmt,
                    stereo_data.as_ptr() as *const BYTE,
                    stereo_data.len(),
                    ud,
                );
                if res != CHANNEL_RC_OK {
                    log::error!("Mic receive failed with {}", res);
                    break;
                }
            }
        }
        log::info!("Mic reader thread ending");
    });
    plugin.reader_join = Some(reader);

    log::debug!(
        "Mic opened: sample_rate={}, channels={}, bits={}, fps={}",
        plugin.format.nSamplesPerSec,
        plugin.format.nChannels,
        plugin.format.wBitsPerSample,
        fps
    );
    CHANNEL_RC_OK
}

unsafe extern "C" fn close(device: *mut IAudinDevice) -> UINT {
    log::debug!("Mic close called");
    let plugin = unsafe { &mut *(device as *mut MicPlugin) };
    if let Some(stop_tx) = plugin.stop_tx.take() {
        let _ = stop_tx.send(());
    }
    if let Some(audio_input) = plugin.audio_input.take() {
        audio_input.stop();
    }
    // Join the reader so no receive callback is in flight when we return:
    // upstream audin frees the plugin/callback right after Close. The
    // reader wakes on the disconnected data channel (integration stopped)
    // or the stop signal; the join is bounded so a stuck host callback
    // cannot hang disconnect forever.
    if let Some(reader) = plugin.reader_join.take() {
        let deadline = std::time::Instant::now() + READER_JOIN_TIMEOUT;
        while !reader.is_finished() {
            if std::time::Instant::now() >= deadline {
                log::error!(
                    "Mic: reader still running after {}s join timeout; \
                     proceeding with close (in-flight receive may race)",
                    READER_JOIN_TIMEOUT.as_secs()
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if reader.is_finished() {
            let _ = reader.join();
        }
    }
    CHANNEL_RC_OK
}

unsafe extern "C" fn free(device: *mut IAudinDevice) -> UINT {
    log::debug!("Mic free called");
    let _plugin = unsafe { Box::from_raw(device as *mut MicPlugin) };
    CHANNEL_RC_OK
}

#[cfg(test)]
mod tests {
    //! Characterization tests for the mic (audin) device contract as the
    //! external consumer relies on it: format negotiation, the mono->stereo
    //! reader relay (exact bytes, stereo AUDIO_FORMAT, userData forwarding),
    //! MicConfig messages, and close/free semantics. Test-only: pins the
    //! CURRENT observable behavior so the reader-lifetime fix (Fix B) can be
    //! proven not to alter it.
    use super::*;
    use crate::integrations::AudioInputIntegration;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── fakes ────────────────────────────────────────────────────────────

    /// Captured receive-callback invocations: (format, data, userData).
    type RecvSink = Arc<Mutex<Vec<(AUDIO_FORMAT, Vec<u8>, usize)>>>;

    /// Optional hold: park the reader INSIDE the receive callback so a test
    /// can run close()/free() while a callback is in flight — the exact UAF
    /// window (upstream frees the audin plugin/callback right after Close).
    /// Only packets of at least `min_size` bytes are held. Memory-safe: the
    /// reader blocks on a condvar, nothing is freed underneath it.
    #[derive(Default)]
    struct RecvHold {
        enabled: std::sync::atomic::AtomicBool,
        min_size: usize,
        entered: Arc<(Mutex<bool>, std::sync::Condvar)>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        inflight: Arc<AtomicU32>,
    }

    /// Per-test receive state, recovered from the userData pointer.
    struct RecvState {
        sink: RecvSink,
        hold: Option<Arc<RecvHold>>,
    }

    /// The receive callback FreeRDP passes to Open. Recovers the per-test
    /// state from the userData pointer (a leaked Box<RecvState>), so calls
    /// from the reader thread land in the right test. No globals.
    unsafe extern "C" fn fake_receive(
        format: *const AUDIO_FORMAT,
        data: *const BYTE,
        size: usize,
        user_data: *mut std::ffi::c_void,
    ) -> UINT {
        let fmt = unsafe { *format };
        let bytes = unsafe { std::slice::from_raw_parts(data, size) }.to_vec();
        // Clone the Arcs out first so the state stays alive even if the
        // harness is dropped while we are parked (pre-fix RED run).
        let (sink, hold) = unsafe {
            let st = &*(user_data as *const RecvState);
            (st.sink.clone(), st.hold.clone())
        };
        if let Some(hold) = &hold {
            if hold.enabled.load(Ordering::SeqCst) && size >= hold.min_size.max(1) {
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
        sink.lock().unwrap().push((fmt, bytes, user_data as usize));
        CHANNEL_RC_OK
    }

    unsafe extern "C" fn failing_receive(
        _format: *const AUDIO_FORMAT,
        _data: *const BYTE,
        _size: usize,
        _user_data: *mut std::ffi::c_void,
    ) -> UINT {
        7 // non-OK: reader must stop relaying
    }

    #[derive(Debug)]
    struct MockAudioInput {
        calls: Mutex<Vec<String>>,
        starts: AtomicU32,
        stops: AtomicU32,
        fail_start: bool,
        data_tx: Mutex<Option<flume::Sender<Vec<u8>>>>,
    }

    impl MockAudioInput {
        fn new(fail_start: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                starts: AtomicU32::new(0),
                stops: AtomicU32::new(0),
                fail_start,
                data_tx: Mutex::new(None),
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn push(&self, data: Vec<u8>) {
            let guard = self.data_tx.lock().unwrap();
            guard
                .as_ref()
                .expect("start must have been called")
                .send(data)
                .expect("reader must still be receiving");
        }
    }

    impl AudioInputIntegration for MockAudioInput {
        fn start(
            &self,
            sample_rate: u32,
            channels: u16,
            bits_per_sample: u16,
            frames_per_packet: u32,
        ) -> anyhow::Result<flume::Receiver<Vec<u8>>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.calls.lock().unwrap().push(format!(
                "start:{sample_rate},{channels},{bits_per_sample},{frames_per_packet}"
            ));
            if self.fail_start {
                anyhow::bail!("mock start failure");
            }
            let (tx, rx) = flume::unbounded();
            *self.data_tx.lock().unwrap() = Some(tx);
            Ok(rx)
        }
        fn stop(&self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
            *self.data_tx.lock().unwrap() = None;
            self.calls.lock().unwrap().push("stop".to_string());
        }
    }

    fn pcm_format(rate: u32, channels: u16, bits: u16) -> AUDIO_FORMAT {
        AUDIO_FORMAT {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: 0, // set_format must recompute
            nBlockAlign: 0,     // set_format must recompute
            wBitsPerSample: bits,
            cbSize: 0,
            data: std::ptr::null_mut(),
        }
    }

    /// Heap MicPlugin wired exactly like mic_entry does, plus a real Rdp
    /// owner behind a zeroed RdpContext (no FreeRDP connection needed:
    /// owner_from_ctx only reads the owner back-pointer).
    struct Harness {
        plugin: *mut MicPlugin,
        rdp: *mut crate::Rdp,
        ctx: *mut crate::context::RdpContext,
        mock: Arc<MockAudioInput>,
        sink: RecvSink,
        user_data: *mut std::ffi::c_void,
        update_rx: flume::Receiver<crate::messaging::RdpMessage>,
        freed: bool,
    }

    impl Harness {
        fn new(fail_start: bool) -> Self {
            Self::with_hold(fail_start, None)
        }
        fn with_hold(fail_start: bool, hold: Option<Arc<RecvHold>>) -> Self {
            let mock = MockAudioInput::new(fail_start);
            let integrations = crate::integrations::RdpIntegrations {
                audio_output: None,
                audio_input: Some(mock.clone()),
                webcam: None,
                clipboard: None,
                smartcard: None,
                printer: None,
            };
            let (update_tx, update_rx) = flume::unbounded();
            let (rdp, _command_tx) = crate::Rdp::new(
                crate::settings::RdpSettings::default(),
                update_tx,
                true,
                None,
                integrations,
            );
            // Rdp is !Unpin (PhantomPinned): keep it at a stable heap address.
            let rdp = Box::into_raw(Box::new(rdp));
            let ctx = Box::into_raw(Box::new(crate::context::RdpContext {
                common: unsafe { std::mem::zeroed() },
                owner: rdp,
            }));

            let plugin = Box::into_raw(Box::new(MicPlugin {
                iface: IAudinDevice {
                    Open: Some(open),
                    FormatSupported: Some(format_supported),
                    SetFormat: Some(set_format),
                    Close: Some(close),
                    Free: Some(free),
                },
                rdpcontext: ctx as *mut freerdp_sys::rdpContext,
                format: unsafe { std::mem::zeroed() },
                frames_per_packet: 0,
                audio_input: None,
                stop_tx: None,
                reader_join: None,
            }));

            // userData = leaked per-test state; reclaimed in Drop.
            let sink: RecvSink = Arc::new(Mutex::new(Vec::new()));
            let state = RecvState {
                sink: sink.clone(),
                hold,
            };
            let user_data = Box::into_raw(Box::new(state)) as *mut std::ffi::c_void;

            Self {
                plugin,
                rdp,
                ctx,
                mock,
                sink,
                user_data,
                update_rx,
                freed: false,
            }
        }

        fn set_format(&self, fmt: &AUDIO_FORMAT, fps: u32) -> UINT {
            unsafe { set_format(self.device(), fmt, fps) }
        }
        fn device(&self) -> *mut IAudinDevice {
            self.plugin as *mut IAudinDevice
        }
        fn open(&self) -> UINT {
            unsafe { open(self.device(), Some(fake_receive), self.user_data) }
        }
        fn open_with(&self, receive: AudinReceive) -> UINT {
            unsafe { open(self.device(), receive, self.user_data) }
        }
        fn close(&self) -> UINT {
            unsafe { close(self.device()) }
        }
        fn received(&self) -> Vec<(AUDIO_FORMAT, Vec<u8>, usize)> {
            self.sink.lock().unwrap().clone()
        }
        fn mic_configs(&self) -> Vec<(u32, u32)> {
            let mut out = Vec::new();
            while let Ok(msg) = self.update_rx.try_recv() {
                if let crate::messaging::RdpMessage::MicConfig {
                    sample_rate,
                    frames_per_packet,
                } = msg
                {
                    out.push((sample_rate, frames_per_packet));
                }
            }
            out
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            unsafe {
                if !self.freed {
                    let _ = free(self.device());
                }
                let _ = Box::from_raw(self.user_data as *mut RecvState);
                let _ = Box::from_raw(self.ctx);
                let _ = Box::from_raw(self.rdp);
            }
        }
    }

    /// Wait until the reader has delivered n packets (bounded).
    fn wait_received(h: &Harness, n: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while h.received().len() < n {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {n} packets, got {}",
                h.received().len()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    // ── format negotiation ───────────────────────────────────────────────

    #[test]
    fn format_supported_pcm_mono_only() {
        let h = Harness::new(false);
        let ok = pcm_format(48000, 1, 16);
        assert_ne!(unsafe { format_supported(h.device(), &ok) }, 0);
        let stereo = pcm_format(48000, 2, 16);
        assert_eq!(unsafe { format_supported(h.device(), &stereo) }, 0);
        let non_pcm = AUDIO_FORMAT {
            wFormatTag: 0x0055, // MP3
            ..pcm_format(48000, 1, 16)
        };
        assert_eq!(unsafe { format_supported(h.device(), &non_pcm) }, 0);
    }

    #[test]
    fn set_format_recomputes_align_and_rate_and_notifies() {
        let h = Harness::new(false);
        let fmt = pcm_format(48000, 1, 16);
        assert_eq!(h.set_format(&fmt, 480), CHANNEL_RC_OK);
        unsafe {
            assert_eq!((*h.plugin).format.nBlockAlign, 2);
            assert_eq!((*h.plugin).format.nAvgBytesPerSec, 96000);
            assert_eq!((*h.plugin).frames_per_packet, 480);
        }
        assert_eq!(h.mic_configs(), vec![(48000, 480)]);
    }

    // ── open contract ────────────────────────────────────────────────────

    #[test]
    fn open_null_receive_is_ok_noop() {
        let h = Harness::new(false);
        assert_eq!(h.open_with(None), CHANNEL_RC_OK);
        assert!(h.mock.calls().is_empty());
    }

    #[test]
    fn open_start_failure_returns_error() {
        let h = Harness::new(true);
        h.set_format(&pcm_format(48000, 1, 16), 480);
        assert_eq!(h.open(), 1);
        assert_eq!(h.mock.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn open_starts_integration_with_negotiated_format_and_notifies() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(44100, 1, 16), 441);
        assert_eq!(h.open(), CHANNEL_RC_OK);
        assert_eq!(h.mock.calls(), vec!["start:44100,1,16,441"]);
        // set_format + open each emit one MicConfig
        assert_eq!(h.mic_configs(), vec![(44100, 441), (44100, 441)]);
    }

    // ── mono -> stereo relay (the consumer-visible audio contract) ──────

    #[test]
    fn mono_frames_are_duplicated_to_stereo_exact_bytes() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(48000, 1, 16), 4); // 4 frames/packet, 2B each
        assert_eq!(h.open(), CHANNEL_RC_OK);

        // 4 mono samples: 0x1122, 0x3344, 0x5566, 0x7788 (LE)
        let mono: Vec<u8> = vec![0x22, 0x11, 0x44, 0x33, 0x66, 0x55, 0x88, 0x77];
        h.mock.push(mono);
        wait_received(&h, 1);

        let (fmt, data, ud) = &h.received()[0];
        // stereo duplication, sample by sample
        assert_eq!(
            data,
            &vec![
                0x22, 0x11, 0x22, 0x11, //
                0x44, 0x33, 0x44, 0x33, //
                0x66, 0x55, 0x66, 0x55, //
                0x88, 0x77, 0x88, 0x77,
            ]
        );
        assert_eq!(fmt.wFormatTag, WAVE_FORMAT_PCM as u16);
        assert_eq!(fmt.nChannels, 2);
        assert_eq!(fmt.nSamplesPerSec, 48000);
        assert_eq!(fmt.wBitsPerSample, 16);
        assert_eq!(fmt.nBlockAlign, 4); // mono align 2 * 2
        assert_eq!(fmt.nAvgBytesPerSec, 192000); // mono 96000 * 2
        assert_eq!(fmt.cbSize, 0);
        assert_eq!(*ud, h.user_data as usize);
        h.close();
    }

    #[test]
    fn short_mono_frame_is_padded_with_silence() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(48000, 1, 16), 4);
        assert_eq!(h.open(), CHANNEL_RC_OK);
        // only 1 sample present; remaining 3 must be silence
        h.mock.push(vec![0xAB, 0xCD]);
        wait_received(&h, 1);
        let (_, data, _) = &h.received()[0];
        assert_eq!(
            data,
            &vec![
                0xAB, 0xCD, 0xAB, 0xCD, //
                0, 0, 0, 0, //
                0, 0, 0, 0, //
                0, 0, 0, 0,
            ]
        );
        h.close();
    }

    #[test]
    fn multiple_frames_relay_in_order() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(48000, 1, 16), 2); // 2 frames x 2B = 4B mono
        assert_eq!(h.open(), CHANNEL_RC_OK);
        for i in 0..5u8 {
            h.mock.push(vec![i, i, i, i]);
        }
        wait_received(&h, 5);
        let got = h.received();
        for (i, (_, data, _)) in got.iter().enumerate() {
            let b = i as u8;
            assert_eq!(data, &vec![b, b, b, b, b, b, b, b]);
        }
        h.close();
    }

    #[test]
    fn receive_failure_stops_the_reader() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(48000, 1, 16), 2);
        assert_eq!(h.open_with(Some(failing_receive)), CHANNEL_RC_OK);
        h.mock.push(vec![1, 2, 3, 4]);
        // reader breaks on first non-OK; further pushes must not panic
        std::thread::sleep(std::time::Duration::from_millis(50));
        let tx = h.mock.data_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(vec![5, 6, 7, 8]); // may fail if reader exited; fine
        }
        h.close();
    }

    // ── close / free contract ────────────────────────────────────────────

    #[test]
    fn close_stops_integration_and_returns_ok() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(48000, 1, 16), 4);
        assert_eq!(h.open(), CHANNEL_RC_OK);
        assert_eq!(h.close(), CHANNEL_RC_OK);
        assert_eq!(h.mock.stops.load(Ordering::SeqCst), 1);
        assert!(h.mock.calls().contains(&"stop".to_string()));
        unsafe {
            assert!((*h.plugin).stop_tx.is_none());
            assert!((*h.plugin).audio_input.is_none());
        }
    }

    #[test]
    fn close_without_open_is_ok_noop() {
        let h = Harness::new(false);
        assert_eq!(h.close(), CHANNEL_RC_OK);
        assert_eq!(h.mock.stops.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn free_returns_ok() {
        let mut h = Harness::new(false);
        let rc = unsafe { free(h.device()) };
        assert_eq!(rc, CHANNEL_RC_OK);
        h.freed = true;
    }

    #[test]
    fn invalid_format_reader_exits_without_relay() {
        let h = Harness::new(false);
        // sample_rate 0 -> reader thread must exit without relaying
        h.set_format(&pcm_format(0, 1, 16), 4);
        assert_eq!(h.open(), CHANNEL_RC_OK);
        let tx = h.mock.data_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(vec![0; 8]);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(h.received().is_empty());
        h.close();
    }

    // ── Lifetime guarantee (Fix B: reader quiesce on close) ──────────────
    //
    // NEW safety contract: when Close returns, no receive callback is in
    // flight or will ever run again — upstream audin frees the plugin and
    // its callback right after Close (FreeRDP 3.15 and 3.26), so any later
    // (receive_cb)(...) is a use-after-free. Deterministic and memory-safe:
    // the fake receive parks the reader on a condvar.
    //
    // Against UNFIXED code these FAIL (Close returns while the reader is
    // still inside receive); against the join/quiesce fix they PASS.

    /// Park the reader INSIDE receive, call close on another thread, and
    /// require it not to return while the callback is in flight.
    #[test]
    fn close_waits_for_inflight_receive() {
        let hold = Arc::new(RecvHold {
            min_size: 1,
            ..Default::default()
        });
        hold.enabled.store(true, Ordering::SeqCst);
        let h = Harness::with_hold(false, Some(hold.clone()));
        h.set_format(&pcm_format(48000, 1, 16), 4);
        assert_eq!(h.open(), CHANNEL_RC_OK);

        // One packet -> reader enters receive and parks there.
        h.mock.push(vec![0xAA; 8]);
        let (lock, cvar) = &*hold.entered;
        let mut entered = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !*entered {
            let now = std::time::Instant::now();
            assert!(now < deadline, "reader never entered receive");
            let (g, _) = cvar.wait_timeout(entered, deadline - now).unwrap();
            entered = g;
        }
        drop(entered);
        assert_eq!(hold.inflight.load(Ordering::SeqCst), 1);

        // Close on a separate thread (post-fix it blocks until quiesced).
        let dev = h.device() as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rc = unsafe { close(dev as *mut IAudinDevice) };
            let _ = done_tx.send(rc);
        });

        // Give the closer ample time to (incorrectly) return early.
        let early_return = done_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok();

        // ALWAYS release the parked callback before asserting.
        let (rlock, rcvar) = &*hold.release;
        *rlock.lock().unwrap() = true;
        rcvar.notify_all();

        let rc = if early_return {
            CHANNEL_RC_OK
        } else {
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("close never returned after the in-flight receive completed")
        };

        // Let the released reader fully exit before any assertion unwinds.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !early_return,
            "close returned while a receive callback was still in flight — \
             upstream frees the plugin right after Close: use-after-free"
        );
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_eq!(hold.inflight.load(Ordering::SeqCst), 0);
    }

    /// Packets queued behind an in-flight receive must never be relayed
    /// after close. The defect window is the callback itself: while the
    /// reader is INSIDE receive (which can block in the host stack), close
    /// returns and upstream frees the plugin; the receive still running is
    /// a use-after-free, and its completion touches freed state. This test
    /// pins that no receive is in flight when close returns, with queued
    /// packets present (they must drop, not relay).
    #[test]
    fn close_drains_queued_packets_no_receive_after_close() {
        let hold = Arc::new(RecvHold {
            min_size: 1,
            ..Default::default()
        });
        hold.enabled.store(true, Ordering::SeqCst);
        let h = Harness::with_hold(false, Some(hold.clone()));
        h.set_format(&pcm_format(48000, 1, 16), 2);
        assert_eq!(h.open(), CHANNEL_RC_OK);

        // Packet 0 parks the reader inside receive; packets 1..3 queue.
        for i in 0..4u8 {
            h.mock.push(vec![i; 4]);
        }
        let (lock, cvar) = &*hold.entered;
        let mut entered = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !*entered {
            let now = std::time::Instant::now();
            assert!(now < deadline, "reader never entered receive");
            let (g, _) = cvar.wait_timeout(entered, deadline - now).unwrap();
            entered = g;
        }
        drop(entered);
        assert_eq!(hold.inflight.load(Ordering::SeqCst), 1);

        // Close on another thread; it must NOT return while receive is in
        // flight (pre-fix it returns immediately -> early_return = true).
        let dev = h.device() as usize;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rc = unsafe { close(dev as *mut IAudinDevice) };
            let _ = done_tx.send(rc);
        });
        let early_return = done_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok();

        // Release the parked receive; the reader then sees the stop signal
        // and exits without relaying the queued packets.
        let (rlock, rcvar) = &*hold.release;
        *rlock.lock().unwrap() = true;
        rcvar.notify_all();
        let rc = if early_return {
            CHANNEL_RC_OK
        } else {
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("close never returned after the in-flight receive completed")
        };
        std::thread::sleep(std::time::Duration::from_millis(50));

        let got = h.received();
        assert!(
            !early_return,
            "close returned while a receive callback was in flight with \
             packets queued — use-after-free on the freed plugin"
        );
        assert_eq!(rc, CHANNEL_RC_OK);
        assert_eq!(hold.inflight.load(Ordering::SeqCst), 0);
        assert_eq!(
            got.len(),
            1,
            "queued packets must be drained, not relayed after close"
        );
        assert_eq!(got[0].1, vec![0u8; 8]); // packet 0, the in-flight one
    }

    /// A consumer packet racing in AFTER close must never reach the receive
    /// callback. With the shipped stop-check right after recv, a packet
    /// sent after close (reader parked in recv) is dropped by the stop
    /// check — this test pins that behavior so the fix preserves it, and
    /// also pins that close itself never leaves a receive in flight.
    #[test]
    fn late_packet_after_close_is_never_relayed() {
        let h = Harness::new(false);
        h.set_format(&pcm_format(48000, 1, 16), 2);
        assert_eq!(h.open(), CHANNEL_RC_OK);
        // Keep our own sender clone alive past close, like a consumer
        // push_data path can.
        let tx = h.mock.data_tx.lock().unwrap().clone().unwrap();
        h.close();
        // Reader is parked in recv; the late packet wakes it, the stop
        // check fires, and nothing is relayed.
        let _ = tx.send(vec![0xEE; 4]);
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while h.received().len() < 1 && std::time::Instant::now() < settle {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let got = h.received();
        drop(tx);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            got.is_empty(),
            "a packet sent after close must never be relayed"
        );
    }
    // ── Format-envelope guarantee (Fix H) ────────────────────────────────

    #[test]
    fn envelope_accepts_standard_formats() {
        for (rate, bits, fps) in [
            (8000, 16, 160),
            (44100, 16, 441),
            (48000, 16, 480),
            (48000, 32, 960),
            (192000, 32, 5760),
            (8000, 8, 80),
        ] {
            assert!(
                audio_format_in_envelope(rate, bits, fps),
                "should accept rate={rate} bits={bits} fps={fps}"
            );
        }
    }

    #[test]
    fn envelope_rejects_host_oversize_and_zero() {
        assert!(!audio_format_in_envelope(48000, 16, u32::MAX));
        assert!(!audio_format_in_envelope(48000, 16, 5761));
        assert!(!audio_format_in_envelope(48000, 16, 0));
        assert!(!audio_format_in_envelope(0, 16, 480));
        assert!(!audio_format_in_envelope(192001, 16, 480));
        assert!(!audio_format_in_envelope(48000, 24, 480));
        assert!(!audio_format_in_envelope(48000, 0, 480));
    }

    #[test]
    fn set_format_rejects_oversize_frames_per_packet() {
        let h = Harness::new(false);
        let fmt = pcm_format(48000, 1, 16);
        assert_ne!(h.set_format(&fmt, u32::MAX), CHANNEL_RC_OK);
        assert_ne!(h.set_format(&fmt, 5761), CHANNEL_RC_OK);
        assert_ne!(h.set_format(&fmt, 0), CHANNEL_RC_OK);
        assert!(h.mic_configs().is_empty());
    }

    #[test]
    fn set_format_rejects_bad_rate_and_bits() {
        let h = Harness::new(false);
        assert_ne!(h.set_format(&pcm_format(0, 1, 16), 480), CHANNEL_RC_OK);
        assert_ne!(h.set_format(&pcm_format(192001, 1, 16), 480), CHANNEL_RC_OK);
        assert_ne!(h.set_format(&pcm_format(48000, 1, 24), 480), CHANNEL_RC_OK);
        assert!(h.mic_configs().is_empty());
    }

    #[test]
    fn set_format_accepts_valid_and_notifies() {
        let h = Harness::new(false);
        assert_eq!(h.set_format(&pcm_format(48000, 1, 16), 480), CHANNEL_RC_OK);
        assert_eq!(h.mic_configs(), vec![(48000, 480)]);
    }

    #[test]
    fn format_supported_gates_bits() {
        let h = Harness::new(false);
        assert_ne!(
            unsafe { format_supported(h.device(), &pcm_format(48000, 1, 16)) },
            0
        );
        assert_ne!(
            unsafe { format_supported(h.device(), &pcm_format(48000, 1, 8)) },
            0
        );
        assert_ne!(
            unsafe { format_supported(h.device(), &pcm_format(48000, 1, 32)) },
            0
        );
        assert_eq!(
            unsafe { format_supported(h.device(), &pcm_format(48000, 1, 24)) },
            0
        );
    }
}
