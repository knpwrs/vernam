//! Playing a video attachment, with its sound, as Desktop's lightbox does:
//! AVFoundation plays it on macOS, and its frames are read out to draw in
//! the window. On Linux ffmpeg decodes the frames as raw BGRA through a
//! pipe, paced by the player's clock (when nobody reads, the pipe fills
//! and ffmpeg waits), and the sound track plays through the audio player
//! (audio.rs).

use std::path::Path;

/// One frame: BGRA rows, `width * 4` bytes each.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// A video being played (or paused); dropping it stops it.
pub struct VideoPlayer {
    #[cfg(target_os = "macos")]
    player: objc2::rc::Retained<objc2_av_foundation::AVPlayer>,
    #[cfg(target_os = "macos")]
    item: objc2::rc::Retained<objc2_av_foundation::AVPlayerItem>,
    #[cfg(target_os = "macos")]
    output: objc2::rc::Retained<objc2_av_foundation::AVPlayerItemVideoOutput>,
    #[cfg(target_os = "linux")]
    imp: linux::Imp,
}

impl VideoPlayer {
    /// Opens `path`, paused at the start. Must be called on the main thread.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            use objc2::AnyThread as _;
            use objc2::rc::Retained;
            use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVPlayerItemVideoOutput};
            use objc2_foundation::{MainThreadMarker, NSDictionary, NSNumber, NSString, NSURL};
            let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("not on the main thread"))?;
            let path = path.to_str().ok_or_else(|| anyhow::anyhow!("not a UTF-8 path"))?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            let item = unsafe { AVPlayerItem::playerItemWithURL(&url, mtm) };
            // Frames as BGRA, which is what gpui draws.
            let key: &NSString = unsafe {
                let key: &objc2_core_foundation::CFString = objc2_core_video::kCVPixelBufferPixelFormatTypeKey;
                &*(key as *const objc2_core_foundation::CFString).cast::<NSString>()
            };
            let format = NSNumber::new_u32(objc2_core_video::kCVPixelFormatType_32BGRA);
            let attributes: Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> =
                NSDictionary::from_slices(&[key], &[&*format as &objc2::runtime::AnyObject]);
            let output = unsafe {
                AVPlayerItemVideoOutput::initWithPixelBufferAttributes(AVPlayerItemVideoOutput::alloc(), Some(&attributes))
            };
            unsafe { item.addOutput(&output) };
            let player = unsafe { AVPlayer::playerWithPlayerItem(Some(&item), mtm) };
            Ok(Self { player, item, output })
        }
        #[cfg(target_os = "linux")]
        {
            Ok(Self { imp: linux::Imp::open(path)? })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = path;
            anyhow::bail!("video playback isn't available here")
        }
    }

    pub fn play(&self) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.play();
        }
        #[cfg(target_os = "linux")]
        self.imp.play();
    }

    pub fn pause(&self) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.pause();
        }
        #[cfg(target_os = "linux")]
        self.imp.pause();
    }

    pub fn is_playing(&self) -> bool {
        #[cfg(target_os = "macos")]
        return unsafe { self.player.rate() } > 0.;
        #[cfg(target_os = "linux")]
        return self.imp.is_playing();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        false
    }

    /// Where it is, in seconds.
    pub fn position(&self) -> f64 {
        #[cfg(target_os = "macos")]
        return finite(unsafe { self.player.currentTime().seconds() });
        #[cfg(target_os = "linux")]
        return self.imp.position();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        0.
    }

    /// Its length in seconds (0 until it's loaded).
    pub fn duration(&self) -> f64 {
        #[cfg(target_os = "macos")]
        return finite(unsafe { self.item.duration().seconds() });
        #[cfg(target_os = "linux")]
        return self.imp.duration();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        0.
    }

    pub fn seek(&self, seconds: f64) {
        #[cfg(target_os = "macos")]
        unsafe {
            let time = objc2_core_media::CMTime::with_seconds(seconds.max(0.), 600);
            self.player.seekToTime(time);
        }
        #[cfg(target_os = "linux")]
        self.imp.seek(seconds);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = seconds;
    }

    /// The frame to show now, if it changed since the last one.
    pub fn frame(&self) -> Option<VideoFrame> {
        #[cfg(target_os = "macos")]
        {
            use objc2_core_video::{
                CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
                CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
                CVPixelBufferUnlockBaseAddress,
            };
            let time = unsafe { self.player.currentTime() };
            if !unsafe { self.output.hasNewPixelBufferForItemTime(time) } {
                return None;
            }
            let buffer =
                unsafe { self.output.copyPixelBufferForItemTime_itemTimeForDisplay(time, std::ptr::null_mut()) }?;
            if unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) } != 0 {
                return None;
            }
            let (width, height) = (CVPixelBufferGetWidth(&buffer), CVPixelBufferGetHeight(&buffer));
            let stride = CVPixelBufferGetBytesPerRow(&buffer);
            let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
            let frame = (!base.is_null() && stride >= width * 4).then(|| {
                // Rows without their padding.
                let mut bgra = Vec::with_capacity(width * height * 4);
                for row in 0..height {
                    let line = unsafe { std::slice::from_raw_parts(base.add(row * stride), width * 4) };
                    bgra.extend_from_slice(line);
                }
                VideoFrame { width: width as u32, height: height as u32, bgra }
            });
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) };
            frame
        }
        #[cfg(target_os = "linux")]
        {
            self.imp.frame()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        None
    }
}

#[cfg(target_os = "macos")]
fn finite(seconds: f64) -> f64 {
    if seconds.is_finite() { seconds.max(0.) } else { 0. }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.pause();
        }
    }
}

/// ffmpeg decoding BGRA frames through a pipe. The player's clock is the
/// truth: the reader thread only reads the next frame once the clock
/// reaches it, and ffmpeg waits behind the pipe meanwhile. The sound track
/// is an [`audio::linux::PipePlayer`], kept at the same rate.
#[cfg(target_os = "linux")]
mod linux {
    use std::io::Read as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdout, Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use crate::audio::linux::PipePlayer;
    use crate::video;

    use super::VideoFrame;

    /// One frame time, in seconds, before the clock says so.
    const EARLY: f64 = 0.005;
    /// How long the reader ever waits between checks (the clock tick).
    const TICK: Duration = Duration::from_millis(20);

    /// What the player is doing, shared with the reader thread.
    struct Play {
        playing: bool,
        /// Where it was at `at` (frozen while paused).
        position: f64,
        at: Option<Instant>,
        rate: f32,
        /// The next frame index the reader decodes.
        next_frame: u32,
        fps: f32,
        /// The video ran out.
        eof: bool,
        /// The player was dropped.
        quit: bool,
    }

    impl Play {
        /// Where it is now.
        fn now(&self) -> f64 {
            match (self.playing, self.at) {
                (true, Some(at)) => self.position + at.elapsed().as_secs_f64() * self.rate as f64,
                _ => self.position,
            }
        }
    }

    /// What the reader should do besides decode the next frame.
    enum Order {
        /// Restart ffmpeg at the given time.
        Seek(f64),
    }

    struct Shared {
        path: PathBuf,
        width: u32,
        height: u32,
        play: Mutex<Play>,
        tick: Condvar,
        /// ffmpeg, held where both the reader and the drop can stop it.
        child: Mutex<Option<Child>>,
        /// New orders for the reader.
        order: Mutex<Option<Order>>,
        /// The latest decoded frame, with its index.
        slot: Mutex<Option<(u32, VideoFrame)>>,
    }

    impl Shared {
        fn kill_child(&self) {
            if let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        /// Starts ffmpeg at `start`, replacing any current one.
        fn spawn_child(&self, start: f64) -> anyhow::Result<()> {
            self.kill_child();
            let child = decoder(&self.path, start)
                .spawn()
                .map_err(|error| anyhow::anyhow!("running ffmpeg (install ffmpeg): {error}"))?;
            *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            Ok(())
        }

        fn stdout(&self) -> Option<std::process::ChildStdout> {
            // Taken once; the reader owns it from then on (until a seek
            // replaces the process, when it's taken again).
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            child.as_mut().and_then(|child| child.stdout.take())
        }
    }

    pub(super) struct Imp {
        shared: Arc<Shared>,
        audio: Option<PipePlayer>,
        length: f64,
        /// The frame index last handed out.
        drawn: AtomicU32,
    }

    impl Imp {
        pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
            let (width, height, fps) = video::probe(path)?;
            let fps = if fps > 0. { fps } else { 24. };
            let length = crate::audio::linux::duration(path)?;
            let shared = Arc::new(Shared {
                path: path.to_owned(),
                width,
                height,
                play: Mutex::new(Play {
                    playing: false,
                    position: 0.,
                    at: None,
                    rate: 1.,
                    next_frame: 0,
                    fps,
                    eof: false,
                    quit: false,
                }),
                tick: Condvar::new(),
                child: Mutex::new(None),
                order: Mutex::new(None),
                slot: Mutex::new(None),
            });
            // A video without a sound track plays silent.
            let audio = PipePlayer::open(path, length).ok();
            shared.spawn_child(0.)?;
            std::thread::Builder::new()
                .name("video-decode".into())
                .spawn({
                    let shared = shared.clone();
                    move || read_frames(shared)
                })
                .map_err(|error| anyhow::anyhow!("spawning the video decoder: {error}"))?;
            Ok(Self { shared, audio, length, drawn: AtomicU32::new(u32::MAX) })
        }

        pub(super) fn play(&self) {
            let mut play = self.shared.play.lock().unwrap_or_else(|e| e.into_inner());
            if play.eof {
                // At the end: start over, as the viewer asks for.
                play.position = 0.;
                play.next_frame = 0;
                play.eof = false;
                *self.shared.order.lock().unwrap_or_else(|e| e.into_inner()) = Some(Order::Seek(0.));
                if let Some(audio) = &self.audio {
                    audio.seek(0.);
                }
            }
            play.playing = true;
            play.at = Some(Instant::now());
            if let Some(audio) = &self.audio {
                audio.play();
            }
            self.shared.tick.notify_all();
        }

        pub(super) fn pause(&self) {
            let mut play = self.shared.play.lock().unwrap_or_else(|e| e.into_inner());
            play.position = play.now();
            play.at = None;
            play.playing = false;
            if let Some(audio) = &self.audio {
                audio.pause();
            }
            self.shared.tick.notify_all();
        }

        pub(super) fn is_playing(&self) -> bool {
            let play = self.shared.play.lock().unwrap_or_else(|e| e.into_inner());
            play.playing && !play.eof
        }

        pub(super) fn position(&self) -> f64 {
            let mut play = self.shared.play.lock().unwrap_or_else(|e| e.into_inner());
            let position = play.now().min(self.length);
            // Frozen as of now, so pausing next resumes here.
            if play.playing {
                play.position = position;
                play.at = Some(Instant::now());
            }
            position
        }

        pub(super) fn duration(&self) -> f64 {
            self.length
        }

        pub(super) fn seek(&self, seconds: f64) {
            let position = seconds.clamp(0., self.length);
            let mut play = self.shared.play.lock().unwrap_or_else(|e| e.into_inner());
            play.eof = false;
            play.position = position;
            play.at = play.playing.then(Instant::now);
            play.next_frame = (position * play.fps as f64).floor() as u32;
            *self.shared.order.lock().unwrap_or_else(|e| e.into_inner()) = Some(Order::Seek(position));
            if let Some(audio) = &self.audio {
                audio.seek(position);
            }
            self.shared.tick.notify_all();
        }

        /// The frame drawn now, if a new one was decoded since last time.
        pub(super) fn frame(&self) -> Option<VideoFrame> {
            let slot = self.shared.slot.lock().unwrap_or_else(|e| e.into_inner());
            let (index, frame) = slot.as_ref()?;
            if self.drawn.load(Ordering::Relaxed) == *index {
                return None;
            }
            self.drawn.store(*index, Ordering::Relaxed);
            Some(frame.clone())
        }
    }

    impl Drop for Imp {
        fn drop(&mut self) {
            self.shared.play.lock().unwrap_or_else(|e| e.into_inner()).quit = true;
            self.shared.tick.notify_all();
            self.shared.kill_child();
        }
    }

    /// The frame decoder: BGRA raw video on stdout from `start`, and no
    /// audio (the sound track plays through [`PipePlayer`]). `-an` puts
    /// the video stream in the output; `-vn` would leave it none, and
    /// ffmpeg would exit at once.
    pub(super) fn decoder(path: &Path, start: f64) -> Command {
        let mut command = Command::new(video::tool("ffmpeg"));
        command
            .arg("-v")
            .arg("error")
            .args(["-ss", &format!("{start}"), "-i"])
            .arg(path)
            .args(["-an", "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    /// The reader: decodes frames as the clock reaches them, and follows
    /// seeks by restarting ffmpeg at the new time. Seeks only land here,
    /// so a frame read can't be overtaken by one.
    fn read_frames(shared: Arc<Shared>) {
        let mut out: Option<ChildStdout> = shared.stdout();
        let bytes = (shared.width as usize) * (shared.height as usize) * 4;
        let mut bgra = vec![0u8; bytes];
        loop {
            // New orders first.
            let order = shared.order.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(Order::Seek(position)) = order {
                match shared.spawn_child(position) {
                    Ok(()) => {
                        out = shared.stdout();
                        // Paused, nothing decodes for the clock: read the
                        // frame the seek landed on now, as AVPlayer shows
                        // the seeked frame. The play lock goes back first:
                        // the read blocks for the whole decode, and the
                        // interface takes that lock for every position.
                        let (paused, index) = {
                            let play = shared.play.lock().unwrap_or_else(|e| e.into_inner());
                            (!play.playing, play.next_frame)
                        };
                        if paused {
                            if let Some(out) = out.as_mut() {
                                if read_exact_quiet(out, &mut bgra) {
                                    *shared.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((
                                        index,
                                        VideoFrame { width: shared.width, height: shared.height, bgra: bgra.clone() },
                                    ));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!("couldn't restart the video decoder: {error}");
                        out = None;
                    }
                }
                continue;
            }
            if shared.play.lock().unwrap_or_else(|e| e.into_inner()).quit {
                return;
            }
            // Wait until the next frame is due.
            let play = shared.play.lock().unwrap_or_else(|e| e.into_inner());
            let due = play.next_frame as f64 / play.fps as f64;
            let now = play.now();
            if !(play.playing && now + EARLY >= due) {
                let wait = if play.playing {
                    Duration::from_secs_f64((((due - now) / play.rate as f64) - EARLY).clamp(0.001, TICK.as_secs_f64()))
                } else {
                    TICK
                };
                let (guard, _) = shared.tick.wait_timeout(play, wait).unwrap_or_else(|e| e.into_inner());
                drop(guard);
                continue;
            }
            // Decode it (ffmpeg waits behind the pipe whenever nobody's
            // reading, so this only blocks for the frame being written).
            drop(play);
            let Some(out) = out.as_mut() else {
                // No decoder (a failed restart): stop as if at the end.
                let mut play = shared.play.lock().unwrap_or_else(|e| e.into_inner());
                play.playing = false;
                play.at = None;
                play.eof = true;
                continue;
            };
            let decoded = read_exact_quiet(out, &mut bgra);
            let mut play = shared.play.lock().unwrap_or_else(|e| e.into_inner());
            if play.quit {
                return;
            }
            if !decoded {
                // The video ran out: stop where it did.
                play.position = play.now().min(play.next_frame as f64 / play.fps as f64);
                play.playing = false;
                play.at = None;
                play.eof = true;
                continue;
            }
            // A seek queued while this frame was in flight: next_frame
            // already names the seeked position, which this older frame
            // must not claim. The seek decodes its own.
            if shared.order.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                continue;
            }
            let index = play.next_frame;
            *shared.slot.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((index, VideoFrame { width: shared.width, height: shared.height, bgra: bgra.clone() }));
            play.next_frame += 1;
        }
    }

    /// Reads `buf` fully; false when the stream ran out.
    pub(super) fn read_exact_quiet(out: &mut ChildStdout, buf: &mut [u8]) -> bool {
        let mut at = 0;
        while at < buf.len() {
            match out.read(&mut buf[at..]) {
                Ok(0) => return false,
                Ok(read) => at += read,
                // A signal interrupts the read, not the stream.
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::{decoder, read_exact_quiet};

    /// The decoder's command really produces whole frames: with the video
    /// stream disabled, ffmpeg exits at once and every decode ends before
    /// its first frame.
    #[test]
    fn the_decoder_produces_whole_frames() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.avi");
        let made = std::process::Command::new(crate::video::tool("ffmpeg"))
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=10:duration=1"])
            .args(["-c:v", "mpeg4", "-q:v", "2", "-y"])
            .arg(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(made.success(), "the test clip encoded");
        let mut child = decoder(&path, 0.).spawn().unwrap();
        let mut frame = vec![0u8; 64 * 64 * 4];
        assert!(read_exact_quiet(child.stdout.as_mut().unwrap(), &mut frame), "the first frame arrives whole");
    }
}
