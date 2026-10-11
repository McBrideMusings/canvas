//! `canvas record --window <app> <out.mp4|out.webp> [--seconds N]`: records
//! the frontmost on-screen window of one app through ScreenCaptureKit for N
//! seconds (default 5, at most 60), at up to 30 frames a second on the real
//! clock, and writes an H.264 mp4 at the window's pixel size or an animated
//! WebP at its point size. Prints `{"path", "seconds", "width", "height",
//! "frames"}`, `frames` being the frames written. ScreenCaptureKit sends a
//! frame only when the window's content changes, so a still window writes few
//! frames over the full length. Every failure is one line on stderr: no
//! window for the app, the Screen Recording grant missing, an extension other
//! than .mp4 or .webp.

use std::path::PathBuf;

pub const MAX_SECONDS: u32 = 60;
const DEFAULT_SECONDS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Mp4,
    Webp,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub app: String,
    pub out: PathBuf,
    pub format: Format,
    pub seconds: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArgError {
    /// The command's shape is wrong: print the usage line.
    Usage,
    /// The shape is right but a value isn't: print this one line.
    Bad(String),
}

pub fn parse_args(args: &[String]) -> Result<Args, ArgError> {
    let mut app = None;
    let mut out = None;
    let mut seconds = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--window" => app = Some(rest.next().ok_or(ArgError::Usage)?.clone()),
            "--seconds" => seconds = Some(rest.next().ok_or(ArgError::Usage)?.clone()),
            flag if flag.starts_with("--") => {
                return Err(ArgError::Bad(format!("unknown flag {flag}")))
            }
            _ if out.is_none() => out = Some(arg.clone()),
            _ => return Err(ArgError::Usage),
        }
    }
    let (Some(app), Some(out)) = (app, out) else {
        return Err(ArgError::Usage);
    };
    if app.trim().is_empty() {
        return Err(ArgError::Bad("--window needs an app name".into()));
    }
    let seconds = match seconds {
        None => DEFAULT_SECONDS,
        Some(s) => match s.parse::<u32>() {
            Ok(n) if (1..=MAX_SECONDS).contains(&n) => n,
            _ => {
                return Err(ArgError::Bad(format!(
                    "--seconds {s}: give a whole number from 1 to {MAX_SECONDS}"
                )))
            }
        },
    };
    let out = PathBuf::from(out);
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let format = match ext.as_deref() {
        Some("mp4") => Format::Mp4,
        Some("webp") => Format::Webp,
        Some(other) => {
            return Err(ArgError::Bad(format!(
                "unknown extension .{other}: name an .mp4 or .webp file"
            )))
        }
        None => {
            return Err(ArgError::Bad(format!(
                "{}: name an .mp4 or .webp file",
                out.display()
            )))
        }
    };
    Ok(Args {
        app,
        out,
        format,
        seconds,
    })
}

/// What a finished recording reports.
#[derive(Debug)]
pub struct Recorded {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
}

pub fn no_window(app: &str) -> String {
    format!("no window for {app}: open one of its windows on screen, then run again")
}

pub const NOT_GRANTED: &str = "Screen Recording not granted: allow canvas, or the terminal app that runs it, in System Settings > Privacy & Security > Screen Recording, then run again";

pub fn run(args: Args) -> Result<(), String> {
    let path =
        std::path::absolute(&args.out).map_err(|e| format!("{}: {e}", args.out.display()))?;
    let shown = path.display().to_string();
    canvas_core::log::info(
        "record started",
        &[
            ("app", &args.app),
            ("path", &shown),
            ("seconds", &args.seconds),
        ],
    );
    match capture::record(&args.app, &path, args.format, args.seconds) {
        Ok(rec) => {
            canvas_core::log::info(
                "record finished",
                &[
                    ("app", &args.app),
                    ("path", &shown),
                    ("seconds", &args.seconds),
                    ("frames", &rec.frames),
                    ("width", &rec.width),
                    ("height", &rec.height),
                ],
            );
            println!(
                "{}",
                serde_json::json!({
                    "path": shown,
                    "seconds": args.seconds,
                    "width": rec.width,
                    "height": rec.height,
                    "frames": rec.frames,
                })
            );
            Ok(())
        }
        Err(e) => {
            canvas_core::log::error(
                "record failed",
                &[
                    ("app", &args.app),
                    ("path", &shown),
                    ("seconds", &args.seconds),
                    ("error", &e),
                ],
            );
            Err(e)
        }
    }
}

/// Builds an animated WebP from frames already encoded as lossless VP8L
/// bitstreams, each with the time in milliseconds it appears; the last frame
/// lasts until `total_ms`. Every frame covers the whole canvas and replaces
/// the one before it (no blending), and the animation loops forever.
pub fn animated_webp(width: u32, height: u32, frames: &[(Vec<u8>, u64)], total_ms: u64) -> Vec<u8> {
    fn u24(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes()[..3]);
    }
    fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    }
    const ANIMATION: u8 = 1 << 1;
    const ALPHA: u8 = 1 << 4;
    const NO_BLEND: u8 = 1 << 1;
    const MAX_DURATION: u64 = (1 << 24) - 1;

    let mut body = b"WEBP".to_vec();
    let mut vp8x = vec![ANIMATION | ALPHA, 0, 0, 0];
    u24(&mut vp8x, width - 1);
    u24(&mut vp8x, height - 1);
    chunk(&mut body, b"VP8X", &vp8x);
    // Background colour (transparent) and a loop count of 0, forever.
    chunk(&mut body, b"ANIM", &[0, 0, 0, 0, 0, 0]);
    for (i, (vp8l, at)) in frames.iter().enumerate() {
        let next = frames.get(i + 1).map_or(total_ms, |(_, t)| *t);
        let duration = next.saturating_sub(*at).min(MAX_DURATION) as u32;
        let mut anmf = Vec::with_capacity(16 + 8 + vp8l.len() + 1);
        u24(&mut anmf, 0);
        u24(&mut anmf, 0);
        u24(&mut anmf, width - 1);
        u24(&mut anmf, height - 1);
        u24(&mut anmf, duration);
        anmf.push(NO_BLEND);
        chunk(&mut anmf, b"VP8L", vp8l);
        chunk(&mut body, b"ANMF", &anmf);
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    file
}

/// One RGBA frame as a lossless VP8L bitstream: image-webp writes a whole
/// simple-format file (`RIFF`, size, `WEBP`, then one `VP8L` chunk), and the
/// bitstream is that chunk's body.
pub fn vp8l_frame(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut file = Vec::new();
    image_webp::WebPEncoder::new(&mut file)
        .encode(rgba, width, height, image_webp::ColorType::Rgba8)
        .map_err(|e| format!("encoding a WebP frame: {e}"))?;
    if file.len() < 20 || &file[12..16] != b"VP8L" {
        return Err("encoding a WebP frame: the encoder wrote no VP8L chunk".into());
    }
    let size = u32::from_le_bytes([file[16], file[17], file[18], file[19]]) as usize;
    file.get(20..20 + size)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "encoding a WebP frame: the VP8L chunk is cut short".into())
}

#[cfg(not(target_os = "macos"))]
mod capture {
    pub fn record(
        _app: &str,
        _out: &std::path::Path,
        _format: super::Format,
        _seconds: u32,
    ) -> Result<super::Recorded, String> {
        Err("canvas record needs macOS ScreenCaptureKit".into())
    }
}

#[cfg(target_os = "macos")]
mod capture {
    use std::path::Path;
    use std::ptr::NonNull;
    use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
    use std::sync::Mutex;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass};
    use objc2_av_foundation::{
        AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVFileTypeMPEG4, AVMediaTypeVideo,
        AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoHeightKey, AVVideoWidthKey,
    };
    use objc2_core_foundation::CFRetained;
    use objc2_core_graphics::{
        CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGWindowListCopyWindowInfo,
        CGWindowListOption,
    };
    use objc2_core_media::{CMSampleBuffer, CMTime};
    use objc2_core_video::{
        kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
        CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
        SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType, SCWindow,
    };

    use super::{Format, Recorded};

    const FPS: i32 = 30;
    /// A WebP frame closer than this to the one before it is dropped: browsers
    /// stretch a frame of 10ms or less to 100ms.
    const WEBP_MIN_GAP_MS: u64 = 20;
    /// A window narrower or shorter than this, in points, is never the one
    /// recorded.
    const MIN_WINDOW_POINTS: f64 = 40.0;
    /// The encoded WebP frames held in memory, and so the file's size, stop
    /// here and the recording fails.
    const MAX_WEBP_BYTES: usize = 256 << 20;

    /// Moves an Objective-C object out of a completion handler's thread.
    struct SendBox<T>(T);
    unsafe impl<T> Send for SendBox<T> {}

    fn wait<T>(rx: &Receiver<T>, secs: u64, what: &str) -> Result<T, String> {
        rx.recv_timeout(Duration::from_secs(secs))
            .map_err(|_| format!("ScreenCaptureKit gave no answer while {what} within {secs}s"))
    }

    /// SCStreamErrorUserDeclined: the grant is missing even though the
    /// preflight check passed (it reads a cache that can lag the setting).
    const USER_DECLINED: isize = -3801;

    fn describe(err: *mut NSError) -> String {
        match unsafe { err.as_ref() } {
            Some(e) if e.code() == USER_DECLINED => super::NOT_GRANTED.into(),
            Some(e) => e.localizedDescription().to_string(),
            None => "unknown error".into(),
        }
    }

    /// Where a recording is written until it is whole: `.c.mp4.part` beside
    /// `c.mp4`, so a failed run leaves the target as it was.
    fn part_path(out: &Path) -> std::path::PathBuf {
        let name = out
            .file_name()
            .map_or_else(Default::default, |n| n.to_string_lossy());
        out.with_file_name(format!(".{name}.part"))
    }

    pub fn record(app: &str, out: &Path, format: Format, seconds: u32) -> Result<Recorded, String> {
        if !CGPreflightScreenCaptureAccess() {
            // Puts canvas in the Screen Recording list (asking once), so the
            // person has a switch to turn on.
            CGRequestScreenCaptureAccess();
            return Err(super::NOT_GRANTED.into());
        }
        let window = find_window(app)?.ok_or_else(|| super::no_window(app))?;

        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
        };
        let rect = unsafe { filter.contentRect() };
        let scale = match format {
            Format::Mp4 => f64::from(unsafe { filter.pointPixelScale() }),
            Format::Webp => 1.0,
        };
        // H.264 needs even dimensions.
        let even = |v: f64| (((v * scale) as u32) & !1).max(2);
        let (width, height) = (even(rect.size.width), even(rect.size.height));

        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(width as usize);
            config.setHeight(height as usize);
            config.setMinimumFrameInterval(CMTime::new(1, FPS));
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setShowsCursor(false);
            config.setIgnoreShadowsSingleWindow(true);
            config.setQueueDepth(6);
        }

        let part = part_path(out);
        let encoder = match format {
            Format::Mp4 => Encoder::mp4(&part, width, height)?,
            Format::Webp => Encoder::webp(width, height),
        };
        let (first_tx, first_rx) = mpsc::channel();
        let output = Output::new(Sink {
            state: Mutex::new(State {
                first: None,
                end_secs: f64::from(seconds),
                frames: 0,
                first_tx: Some(first_tx),
                encoder,
                failure: None,
            }),
        });

        let captured = run_stream(&output, &filter, &config, &first_rx, seconds);
        let (encoder, frames, failure) = {
            let Ok(mut state) = output.ivars().state.lock() else {
                let _ = std::fs::remove_file(&part);
                return Err("the capture thread panicked".into());
            };
            (
                std::mem::replace(&mut state.encoder, Encoder::Done),
                state.frames,
                state.failure.take(),
            )
        };
        let finished = match captured.and_then(|()| failure.map_or(Ok(()), Err)) {
            Ok(()) => encoder.finish(&part, f64::from(seconds)),
            Err(e) => {
                encoder.cancel();
                Err(e)
            }
        };
        let finished = finished.and_then(|extra| {
            std::fs::rename(&part, out)
                .map(|()| extra)
                .map_err(|e| format!("{}: {e}", out.display()))
        });
        match finished {
            Ok(extra) => Ok(Recorded {
                width,
                height,
                frames: frames + extra,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                Err(e)
            }
        }
    }

    /// Starts the stream, records `seconds` from the window's first frame,
    /// and stops it.
    fn run_stream(
        output: &Output,
        filter: &SCContentFilter,
        config: &SCStreamConfiguration,
        first_rx: &Receiver<()>,
        seconds: u32,
    ) -> Result<(), String> {
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, config, None)
        };
        let queue = DispatchQueue::new("com.piercemakes.canvas.record", None);
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| format!("adding the stream output: {}", e.localizedDescription()))?;

        let (started_tx, started_rx) = mpsc::channel();
        let started = RcBlock::new(move |err: *mut NSError| {
            let _ = started_tx.send(if err.is_null() {
                Ok(())
            } else {
                Err(describe(err))
            });
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&started)) };
        wait(&started_rx, 10, "starting the capture")?
            .map_err(|e| format!("starting the capture: {e}"))?;

        let first = wait(first_rx, 5, "waiting for the window's first frame");
        if first.is_ok() {
            std::thread::sleep(Duration::from_secs(u64::from(seconds)));
        }

        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stopped = RcBlock::new(move |err: *mut NSError| {
            let _ = stopped_tx.send(if err.is_null() {
                Ok(())
            } else {
                Err(describe(err))
            });
        });
        unsafe { stream.stopCaptureWithCompletionHandler(Some(&stopped)) };
        let stop = wait(&stopped_rx, 10, "stopping the capture");
        first?;
        stop?.map_err(|e| format!("stopping the capture: {e}"))
    }

    /// The frontmost on-screen, normal-layer window owned by the app named
    /// `app` (its name, or its bundle id, case-insensitive).
    fn find_window(app: &str) -> Result<Option<Retained<SCWindow>>, String> {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
            let found = match unsafe { Retained::retain(content) } {
                Some(c) => Ok(SendBox(c)),
                None => Err(describe(err)),
            };
            let _ = tx.send(found);
        });
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                true, true, &block,
            )
        };
        let content = wait(&rx, 10, "listing windows")?
            .map_err(|e| match e.as_str() {
                super::NOT_GRANTED => e,
                _ => format!("listing windows: {e}"),
            })?
            .0;

        let order = front_to_back();
        let rank = |id: u32| order.iter().position(|w| *w == id).unwrap_or(usize::MAX);
        let mut best: Option<(usize, Retained<SCWindow>)> = None;
        for window in unsafe { content.windows() } {
            let Some(owner) = (unsafe { window.owningApplication() }) else {
                continue;
            };
            let name = unsafe { owner.applicationName() }.to_string();
            let bundle = unsafe { owner.bundleIdentifier() }.to_string();
            let ours = name.eq_ignore_ascii_case(app) || bundle.eq_ignore_ascii_case(app);
            if !ours || unsafe { window.windowLayer() } != 0 || !unsafe { window.isOnScreen() } {
                continue;
            }
            // Skips the slivers and helper windows some apps keep on screen.
            let frame = unsafe { window.frame() };
            if frame.size.width < MIN_WINDOW_POINTS || frame.size.height < MIN_WINDOW_POINTS {
                continue;
            }
            let r = rank(unsafe { window.windowID() });
            if best.as_ref().is_none_or(|(b, _)| r < *b) {
                best = Some((r, window));
            }
        }
        Ok(best.map(|(_, w)| w))
    }

    /// On-screen window ids, frontmost first, as the window server orders them.
    fn front_to_back() -> Vec<u32> {
        let option =
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
        let Some(list) = CGWindowListCopyWindowInfo(option, 0) else {
            return Vec::new();
        };
        // CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
        let list: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*(&*list as *const _ as *const NSArray<NSDictionary<NSString, AnyObject>>) };
        let key = NSString::from_str("kCGWindowNumber");
        list.iter()
            .filter_map(|info| info.objectForKey(&key))
            .filter_map(|n| n.downcast::<NSNumber>().ok())
            .map(|n| n.unsignedIntValue())
            .collect()
    }

    /// Each frame as a VP8L bitstream with the millisecond it appears.
    type Encoded = Vec<(Vec<u8>, u64)>;

    struct Frame {
        rgba: Vec<u8>,
        at_ms: u64,
    }

    enum Encoder {
        Mp4 {
            writer: Retained<AVAssetWriter>,
            input: Retained<AVAssetWriterInput>,
            /// The first frame's time, where the file's session starts.
            start: Option<CMTime>,
            /// The newest frame the writer wasn't ready for, appended at the
            /// end if no later frame replaced it, so the clip ends on the
            /// window's last state.
            pending: Option<CFRetained<CMSampleBuffer>>,
        },
        Webp {
            width: u32,
            height: u32,
            tx: SyncSender<Frame>,
            worker: JoinHandle<Result<Encoded, String>>,
            last_ms: Option<u64>,
            /// The newest frame dropped, sent at the end if no later frame
            /// replaced it.
            pending: Option<Frame>,
        },
        Done,
    }

    impl Encoder {
        fn mp4(out: &Path, width: u32, height: u32) -> Result<Self, String> {
            // A part file a crashed run left behind; AVAssetWriter refuses
            // to write over a file.
            match std::fs::remove_file(out) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", out.display())),
            }
            let url = NSURL::fileURLWithPath(&NSString::from_str(&out.to_string_lossy()));
            let constants = unsafe {
                (
                    AVFileTypeMPEG4,
                    AVMediaTypeVideo,
                    AVVideoCodecKey,
                    AVVideoCodecTypeH264,
                    AVVideoWidthKey,
                    AVVideoHeightKey,
                )
            };
            let (
                Some(file_type),
                Some(media),
                Some(codec_key),
                Some(h264),
                Some(w_key),
                Some(h_key),
            ) = constants
            else {
                return Err("AVFoundation is missing its H.264 constants".into());
            };
            let writer =
                unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type) }
                    .map_err(|e| format!("{}: {}", out.display(), e.localizedDescription()))?;
            let w = NSNumber::new_u32(width);
            let h = NSNumber::new_u32(height);
            let settings: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
                &[codec_key, w_key, h_key],
                &[h264.as_ref() as &AnyObject, w.as_ref(), h.as_ref()],
            );
            let input = unsafe {
                AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                    media,
                    Some(&settings),
                )
            };
            unsafe { input.setExpectsMediaDataInRealTime(true) };
            if !unsafe { writer.canAddInput(&input) } {
                return Err("AVAssetWriter refused an H.264 input of this size".into());
            }
            unsafe { writer.addInput(&input) };
            if !unsafe { writer.startWriting() } {
                return Err(format!(
                    "writing {}: {}",
                    out.display(),
                    writer_error(&writer)
                ));
            }
            Ok(Encoder::Mp4 {
                writer,
                input,
                start: None,
                pending: None,
            })
        }

        fn webp(width: u32, height: u32) -> Self {
            // Frames wait here while the encoder works; a full queue drops the
            // new frame, so the one before it shows for longer.
            let (tx, rx) = mpsc::sync_channel::<Frame>(4);
            let worker = std::thread::spawn(move || {
                let mut frames = Vec::new();
                let mut bytes = 0;
                for frame in rx {
                    let vp8l = super::vp8l_frame(&frame.rgba, width, height)?;
                    bytes += vp8l.len();
                    if bytes > MAX_WEBP_BYTES {
                        return Err(format!(
                            "the WebP passed {} MB at {:.1}s: record fewer seconds, or to .mp4",
                            MAX_WEBP_BYTES >> 20,
                            frame.at_ms as f64 / 1000.0
                        ));
                    }
                    frames.push((vp8l, frame.at_ms));
                }
                Ok(frames)
            });
            Encoder::Webp {
                width,
                height,
                tx,
                worker,
                last_ms: None,
                pending: None,
            }
        }

        /// Takes one complete frame at `secs` into the clip; false when it was
        /// dropped (kept as pending until a later frame replaces it).
        fn push(
            &mut self,
            buffer: &CMSampleBuffer,
            secs: f64,
            start: CMTime,
        ) -> Result<bool, String> {
            match self {
                Encoder::Mp4 {
                    writer,
                    input,
                    start: session,
                    pending,
                } => {
                    if session.is_none() {
                        unsafe { writer.startSessionAtSourceTime(start) };
                        *session = Some(start);
                    }
                    if !unsafe { input.isReadyForMoreMediaData() } {
                        *pending = Some(unsafe { CFRetained::retain(NonNull::from(buffer)) });
                        return Ok(false);
                    }
                    *pending = None;
                    if !unsafe { input.appendSampleBuffer(buffer) } {
                        return Err(format!("appending a frame: {}", writer_error(writer)));
                    }
                    Ok(true)
                }
                Encoder::Webp {
                    width,
                    height,
                    tx,
                    last_ms,
                    pending,
                    ..
                } => {
                    let at_ms = (secs * 1000.0) as u64;
                    let Some(rgba) = rgba(buffer, *width, *height) else {
                        return Ok(false);
                    };
                    let frame = Frame { rgba, at_ms };
                    if last_ms.is_some_and(|l| at_ms < l + WEBP_MIN_GAP_MS) {
                        *pending = Some(frame);
                        return Ok(false);
                    }
                    match tx.try_send(frame) {
                        Ok(()) => {
                            *last_ms = Some(at_ms);
                            *pending = None;
                            Ok(true)
                        }
                        Err(TrySendError::Full(frame)) => {
                            *pending = Some(frame);
                            Ok(false)
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            Err("the WebP encoder stopped".into())
                        }
                    }
                }
                Encoder::Done => Ok(false),
            }
        }

        /// Abandons the recording: an mp4 writer stops writing its file.
        fn cancel(self) {
            if let Encoder::Mp4 { writer, .. } = self {
                if unsafe { writer.status() } == AVAssetWriterStatus::Writing {
                    unsafe { writer.cancelWriting() };
                }
            }
        }

        /// Writes the whole file to `part`; the frames written at the end
        /// (a pending one) come back to add to the count.
        fn finish(self, part: &Path, seconds: f64) -> Result<u64, String> {
            match self {
                Encoder::Mp4 {
                    writer,
                    input,
                    start,
                    pending,
                } => {
                    // Calling on a writer that has failed raises an exception.
                    if unsafe { writer.status() } != AVAssetWriterStatus::Writing {
                        return Err(format!("writing the mp4: {}", writer_error(&writer)));
                    }
                    let mut extra = 0;
                    if let Some(buffer) = pending {
                        for _ in 0..100 {
                            if unsafe { input.isReadyForMoreMediaData() } {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        if unsafe {
                            input.isReadyForMoreMediaData() && input.appendSampleBuffer(&buffer)
                        } {
                            extra = 1;
                        }
                    }
                    unsafe { input.markAsFinished() };
                    if let Some(start) = start {
                        // The clip lasts `seconds` from its first frame, however
                        // few frames a still window sent.
                        let length = (seconds * f64::from(start.timescale)) as i64;
                        unsafe {
                            writer.endSessionAtSourceTime(CMTime {
                                value: start.value + length,
                                ..start
                            })
                        };
                    }
                    let (tx, rx) = mpsc::channel();
                    let done = RcBlock::new(move || {
                        let _ = tx.send(());
                    });
                    unsafe { writer.finishWritingWithCompletionHandler(&done) };
                    wait(&rx, 30, "finishing the mp4")?;
                    if unsafe { writer.status() } != AVAssetWriterStatus::Completed {
                        return Err(format!("writing the mp4: {}", writer_error(&writer)));
                    }
                    Ok(extra)
                }
                Encoder::Webp {
                    width,
                    height,
                    tx,
                    worker,
                    last_ms,
                    pending,
                } => {
                    let mut extra = 0;
                    if let Some(mut frame) = pending {
                        // Shifted past the gap, so a browser shows it as timed.
                        if let Some(l) = last_ms {
                            frame.at_ms = frame.at_ms.max(l + WEBP_MIN_GAP_MS);
                        }
                        if tx.send(frame).is_ok() {
                            extra = 1;
                        }
                    }
                    drop(tx);
                    let frames = worker.join().map_err(|_| "the WebP encoder panicked")??;
                    if frames.is_empty() {
                        return Err("the window sent no frames".into());
                    }
                    let file =
                        super::animated_webp(width, height, &frames, (seconds * 1000.0) as u64);
                    std::fs::write(part, file).map_err(|e| format!("{}: {e}", part.display()))?;
                    Ok(extra)
                }
                Encoder::Done => Ok(0),
            }
        }
    }

    fn writer_error(writer: &AVAssetWriter) -> String {
        unsafe { writer.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "AVAssetWriter failed".into())
    }

    /// The frame's BGRA pixels as tightly packed RGBA, or None for a frame
    /// whose size isn't the stream's.
    fn rgba(buffer: &CMSampleBuffer, width: u32, height: u32) -> Option<Vec<u8>> {
        let pixels = unsafe { buffer.image_buffer() }?;
        if CVPixelBufferGetWidth(&pixels) != width as usize
            || CVPixelBufferGetHeight(&pixels) != height as usize
        {
            return None;
        }
        unsafe { CVPixelBufferLockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly) };
        let base = CVPixelBufferGetBaseAddress(&pixels) as *const u8;
        let stride = CVPixelBufferGetBytesPerRow(&pixels);
        let row = width as usize * 4;
        let mut out = Vec::with_capacity(row * height as usize);
        if !base.is_null() {
            for y in 0..height as usize {
                let src = unsafe { std::slice::from_raw_parts(base.add(y * stride), row) };
                for px in src.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            }
        }
        unsafe { CVPixelBufferUnlockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly) };
        (!base.is_null()).then_some(out)
    }

    /// The frame's ScreenCaptureKit status; only `Complete` frames carry new
    /// pixels.
    fn status(buffer: &CMSampleBuffer) -> Option<SCFrameStatus> {
        let list = unsafe { buffer.sample_attachments_array(false) }?;
        let list: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*(&*list as *const _ as *const NSArray<NSDictionary<NSString, AnyObject>>) };
        let info = list.firstObject()?;
        let value = info.objectForKey(unsafe { SCStreamFrameInfoStatus })?;
        let n = value.downcast::<NSNumber>().ok()?;
        Some(SCFrameStatus(n.integerValue()))
    }

    fn seconds(t: CMTime) -> f64 {
        t.value as f64 / f64::from(t.timescale.max(1))
    }

    struct State {
        first: Option<CMTime>,
        end_secs: f64,
        frames: u64,
        first_tx: Option<Sender<()>>,
        encoder: Encoder,
        failure: Option<String>,
    }

    struct Sink {
        state: Mutex<State>,
    }

    impl Sink {
        fn take(&self, buffer: &CMSampleBuffer) {
            if status(buffer) != Some(SCFrameStatus::Complete) {
                return;
            }
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if state.failure.is_some() {
                return;
            }
            let pts = unsafe { buffer.presentation_time_stamp() };
            let start = *state.first.get_or_insert(pts);
            let secs = seconds(pts) - seconds(start);
            if secs > state.end_secs {
                return;
            }
            match state.encoder.push(buffer, secs, start) {
                Ok(true) => state.frames += 1,
                Ok(false) => {}
                Err(e) => state.failure = Some(e),
            }
            if let Some(tx) = state.first_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "CanvasRecordOutput"]
        #[ivars = Sink]
        struct Output;

        unsafe impl NSObjectProtocol for Output {}

        unsafe impl SCStreamOutput for Output {
            #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
            fn did_output(
                &self,
                _stream: &SCStream,
                buffer: &CMSampleBuffer,
                kind: SCStreamOutputType,
            ) {
                if kind == SCStreamOutputType::Screen {
                    self.ivars().take(buffer);
                }
            }
        }
    );

    impl Output {
        fn new(sink: Sink) -> Retained<Self> {
            let this = Self::alloc().set_ivars(sink);
            unsafe { msg_send![super(this), init] }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_the_window_the_file_and_the_seconds() {
        let a = parse_args(&args("--window Canvas /tmp/c.mp4 --seconds 3")).unwrap();
        assert_eq!(
            a,
            Args {
                app: "Canvas".into(),
                out: "/tmp/c.mp4".into(),
                format: Format::Mp4,
                seconds: 3
            }
        );
        let b = parse_args(&args("/tmp/c.WEBP --window Safari")).unwrap();
        assert_eq!((b.format, b.seconds), (Format::Webp, 5));
    }

    #[test]
    fn refuses_bad_values_with_one_line() {
        assert_eq!(
            parse_args(&args("--window Canvas /tmp/c.gif")),
            Err(ArgError::Bad(
                "unknown extension .gif: name an .mp4 or .webp file".into()
            ))
        );
        assert_eq!(
            parse_args(&args("--window Canvas /tmp/c.mp4 --seconds 61")),
            Err(ArgError::Bad(
                "--seconds 61: give a whole number from 1 to 60".into()
            ))
        );
        assert_eq!(
            parse_args(&args("--window Canvas /tmp/c.mp4 --seconds 0")),
            Err(ArgError::Bad(
                "--seconds 0: give a whole number from 1 to 60".into()
            ))
        );
        assert_eq!(
            parse_args(&args("--window Canvas /tmp/c.mp4 --fps 10")),
            Err(ArgError::Bad("unknown flag --fps".into()))
        );
        assert_eq!(parse_args(&args("/tmp/c.mp4")), Err(ArgError::Usage));
        assert_eq!(parse_args(&args("--window Canvas")), Err(ArgError::Usage));
        assert_eq!(
            parse_args(&args("--window Canvas a.mp4 b.mp4")),
            Err(ArgError::Usage)
        );
    }

    #[test]
    fn names_the_reason_in_the_error_lines() {
        assert_eq!(
            no_window("Safari"),
            "no window for Safari: open one of its windows on screen, then run again"
        );
        assert!(NOT_GRANTED.contains("System Settings > Privacy & Security > Screen Recording"));
    }

    #[test]
    fn builds_an_animated_webp_a_decoder_reads_back() {
        let (w, h) = (4u32, 3u32);
        let red: Vec<u8> = [255, 0, 0, 255].repeat((w * h) as usize);
        let blue: Vec<u8> = [0, 0, 255, 255].repeat((w * h) as usize);
        let frames = vec![
            (vp8l_frame(&red, w, h).unwrap(), 0),
            (vp8l_frame(&blue, w, h).unwrap(), 400),
        ];
        let file = animated_webp(w, h, &frames, 1000);

        let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(file)).unwrap();
        assert!(decoder.is_animated());
        assert_eq!(decoder.dimensions(), (w, h));
        assert_eq!(decoder.num_frames(), 2);
        let mut buf = vec![0; decoder.output_buffer_size().unwrap()];
        assert_eq!(decoder.read_frame(&mut buf).unwrap(), 400);
        assert_eq!(&buf[..4], &[255, 0, 0, 255]);
        assert_eq!(decoder.read_frame(&mut buf).unwrap(), 600);
        assert_eq!(&buf[..4], &[0, 0, 255, 255]);
    }
}
