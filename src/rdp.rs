//! One FreeRDP owner thread per session. GTK never owns a FreeRDP context.
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

#[repr(C)]
struct Native {
    _private: [u8; 0],
}
type FrameCallback = unsafe extern "C" fn(*mut c_void, *const u8, u32, u32, u32) -> c_int;
type CertCallback = unsafe extern "C" fn(
    *mut c_void,
    c_int,
    *const c_char,
    u16,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
) -> c_int;
extern "C" {
    fn rdp_manager_initialize() -> c_int;
    fn rdp_manager_build_config() -> *const c_char;
    fn rdp_manager_new(
        host: *const c_char,
        port: u16,
        user: *const c_char,
        password: *const c_char,
        domain: *const c_char,
        width: u32,
        height: u32,
        opaque: *mut c_void,
        frame: FrameCallback,
        certificate: CertCallback,
    ) -> *mut Native;
    fn rdp_manager_connect(native: *mut Native) -> c_int;
    fn rdp_manager_poll(native: *mut Native) -> c_int;
    fn rdp_manager_graphics_stats(native: *mut Native, output: *mut c_char, capacity: usize);
    fn rdp_manager_input(native: *mut Native, kind: u32, flags: u16, a: u16, b: u16) -> c_int;
    fn rdp_manager_cancel(native: *mut Native);
    fn rdp_manager_error(native: *mut Native) -> u32;
    fn rdp_manager_error_name(native: *mut Native) -> *const c_char;
    fn rdp_manager_free(native: *mut Native);
}

pub fn diagnostics() -> String {
    let config = unsafe { CStr::from_ptr(rdp_manager_build_config()) }.to_string_lossy();
    let flags = config
        .split_whitespace()
        .filter(|v| {
            [
                "WITH_GFX_H264=",
                "WITH_VIDEO_FFMPEG=",
                "WITH_FFMPEG_HWACCEL=",
                "WITH_OPENH264=",
                "WITH_SIMD=",
                "WITH_SWSCALE=",
            ]
            .iter()
            .any(|prefix| v.starts_with(prefix))
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("FreeRDP build capabilities (not proof of active GPU decoding):\n{flags}\nPresentation: GDI -> CPU copy -> GDK MemoryTexture; zero-copy not implemented.")
}

pub struct Config {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub password: Zeroizing<String>,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug)]
pub struct Certificate {
    pub host: String,
    pub port: u16,
    pub common: String,
    pub subject: String,
    pub issuer: String,
    pub fingerprint: String,
    pub old_fingerprint: String,
    pub changed: bool,
}
pub enum CertificateDecision {
    Reject,
    Session,
    Remember,
}

pub enum Event {
    Connected,
    Certificate(Certificate, mpsc::Sender<CertificateDecision>),
    Ended {
        code: u32,
        message: String,
        cancelled: bool,
    },
}
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub kind: u32,
    pub flags: u16,
    pub a: u16,
    pub b: u16,
}
impl Input {
    pub fn scan(code: u16, down: bool, repeat: bool) -> Self {
        Self {
            kind: 0,
            flags: if down {
                if repeat {
                    0x4000
                } else {
                    0
                }
            } else {
                0x8000
            } | if code & 0x100 != 0 { 0x100 } else { 0 },
            a: code & 0xff,
            b: 0,
        }
    }
    pub fn mouse(flags: u16, x: u16, y: u16) -> Self {
        Self {
            kind: 2,
            flags,
            a: x,
            b: y,
        }
    }
}
pub struct Control {
    stopped: AtomicBool,
    overflow: AtomicBool,
    native: Mutex<Option<usize>>,
    tx: SyncSender<Input>,
    motion: Mutex<Option<Input>>,
    frame: Mutex<Option<Frame>>,
}
impl Control {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        // Holding this mutex excludes freeing the pointer. The C operation only
        // sets FreeRDP's thread-safe abort event; all other native calls stay on the owner.
        if let Ok(native) = self.native.lock() {
            if let Some(native) = *native {
                unsafe {
                    rdp_manager_cancel(native as *mut Native);
                }
            }
        }
    }
    pub fn send(&self, input: Input) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        match self.tx.try_send(input) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.overflow.store(true, Ordering::Release);
                self.stop();
            }
        }
    }
    pub fn motion(&self, x: u16, y: u16) {
        if let Ok(mut motion) = self.motion.lock() {
            *motion = Some(Input::mouse(0x0800, x, y));
        }
    }
    pub fn take_frame(&self) -> Option<Frame> {
        self.frame.lock().ok()?.take()
    }
}

struct Callbacks {
    control: Arc<Control>,
    events: mpsc::Sender<Event>,
}
fn frame_size(width: u32, height: u32, stride: u32) -> Option<usize> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 16_777_216
        || stride < width.checked_mul(4)?
    {
        return None;
    }
    let size = (stride as usize).checked_mul(height as usize)?;
    (size <= 128 * 1024 * 1024).then_some(size)
}
fn copy_frame(source: &[u8], width: u32, height: u32, stride: u32) -> Option<Frame> {
    if source.len() < frame_size(width, height, stride)? {
        return None;
    }
    let line = width as usize * 4;
    let mut data = vec![0; line * height as usize];
    for (row, output) in data.chunks_exact_mut(line).enumerate() {
        output.copy_from_slice(&source[row * stride as usize..row * stride as usize + line]);
        for pixel in output.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Some(Frame {
        width,
        height,
        data,
    })
}
unsafe extern "C" fn frame_callback(
    opaque: *mut c_void,
    data: *const u8,
    width: u32,
    height: u32,
    stride: u32,
) -> c_int {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if opaque.is_null() || data.is_null() {
            return 0;
        }
        let callbacks = unsafe { &*(opaque as *const Callbacks) };
        if callbacks.control.stopped.load(Ordering::Acquire) {
            return 0;
        }
        // Reserve the single presentation slot before copying megapixels.
        // Return 2 to keep native dirty state until GTK consumes the old frame.
        let Ok(mut latest) = callbacks.control.frame.lock() else {
            return 0;
        };
        if latest.is_some() {
            return 2;
        }
        let Some(size) = frame_size(width, height, stride) else {
            return 0;
        };
        // FreeRDP owns this stride*height GDI buffer and keeps it alive during the callback.
        let source = unsafe { std::slice::from_raw_parts(data, size) };
        let Some(frame) = copy_frame(source, width, height, stride) else {
            return 0;
        };
        *latest = Some(frame);
        1
    }))
    .unwrap_or(0)
}
unsafe fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .chars()
        .take(4096)
        .collect()
}
unsafe extern "C" fn certificate_callback(
    opaque: *mut c_void,
    changed: c_int,
    host: *const c_char,
    port: u16,
    common: *const c_char,
    subject: *const c_char,
    issuer: *const c_char,
    fingerprint: *const c_char,
    old: *const c_char,
) -> c_int {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if opaque.is_null() {
            return 0;
        }
        let callbacks = unsafe { &*(opaque as *const Callbacks) };
        let cert = unsafe {
            Certificate {
                host: text(host),
                port,
                common: text(common),
                subject: text(subject),
                issuer: text(issuer),
                fingerprint: text(fingerprint),
                old_fingerprint: text(old),
                changed: changed != 0,
            }
        };
        let (tx, rx) = mpsc::channel();
        if callbacks.events.send(Event::Certificate(cert, tx)).is_err() || changed != 0 {
            return 0;
        }
        while !callbacks.control.stopped.load(Ordering::Acquire) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(decision) => {
                    return match decision {
                        CertificateDecision::Reject => 0,
                        CertificateDecision::Session => 1,
                        CertificateDecision::Remember => 2,
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        0
    }))
    .unwrap_or(0)
}

pub fn start(config: Config) -> Result<(Arc<Control>, Receiver<Event>), String> {
    let (tx, input) = mpsc::sync_channel(1024);
    let (events, rx) = mpsc::channel();
    let control = Arc::new(Control {
        stopped: AtomicBool::new(false),
        overflow: AtomicBool::new(false),
        native: Mutex::new(None),
        tx,
        motion: Mutex::new(None),
        frame: Mutex::new(None),
    });
    let worker = control.clone();
    std::thread::Builder::new()
        .name("rdp-session".into())
        .spawn(move || {
            let mut callbacks = Box::new(Callbacks {
                control: worker.clone(),
                events: events.clone(),
            });
            let result = run(config, &worker, &input, &mut callbacks);
            let cancelled =
                worker.stopped.load(Ordering::Acquire) && !worker.overflow.load(Ordering::Acquire);
            let (code, message) = if worker.overflow.load(Ordering::Acquire) {
                (
                    0xFFFF_FFFE,
                    "Сеанс остановлен: переполнена очередь ввода.".into(),
                )
            } else {
                match result {
                    Ok(code) => (
                        code,
                        if code == 0 {
                            "Сеанс завершён".into()
                        } else {
                            format!("RDP завершён с кодом 0x{code:08X}")
                        },
                    ),
                    Err(error) => (error.code, error.message),
                }
            };
            let _ = events.send(Event::Ended {
                code,
                message,
                cancelled,
            });
        })
        .map_err(|_| "Не удалось запустить поток RDP.".to_string())?;
    Ok((control, rx))
}

struct RunFailure {
    code: u32,
    message: String,
}
impl From<&str> for RunFailure {
    fn from(message: &str) -> Self {
        Self {
            code: 0,
            message: message.into(),
        }
    }
}

fn run(
    config: Config,
    control: &Control,
    input: &Receiver<Input>,
    callbacks: &mut Callbacks,
) -> Result<u32, RunFailure> {
    static INITIALIZED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*INITIALIZED.get_or_init(|| unsafe { rdp_manager_initialize() != 0 }) {
        return Err("Не удалось инициализировать FreeRDP/WinPR SSL.".into());
    }
    let host = CString::new(config.host).map_err(|_| "Некорректный адрес сервера.")?;
    let user = CString::new(config.username).map_err(|_| "Некорректный пользователь.")?;
    let domain = CString::new(config.domain).map_err(|_| "Некорректный домен.")?;
    let pass = CString::new(config.password.as_str())
        .map_err(|_| "Пароль содержит недопустимый символ.")?;
    let native = unsafe {
        rdp_manager_new(
            host.as_ptr(),
            config.port,
            user.as_ptr(),
            pass.as_ptr(),
            domain.as_ptr(),
            config.width,
            config.height,
            callbacks as *mut _ as *mut c_void,
            frame_callback,
            certificate_callback,
        )
    };
    let mut temporary = pass.into_bytes_with_nul();
    temporary.zeroize();
    drop(config.password);
    if native.is_null() {
        return Err("Не удалось подготовить native FreeRDP frontend.".into());
    }
    {
        let mut handle = control.native.lock().map_err(|_| "Ошибка состояния RDP.")?;
        *handle = Some(native as usize);
        if control.stopped.load(Ordering::Acquire) {
            unsafe {
                rdp_manager_cancel(native);
            }
        }
    }
    let connected = unsafe { rdp_manager_connect(native) } != 0;
    if connected {
        let _ = callbacks.events.send(Event::Connected);
        let trace = std::env::var_os("RDP_MANAGER_TRACE_GRAPHICS").is_some();
        let mut last_trace = std::time::Instant::now() - Duration::from_secs(3);
        'session: while !control.stopped.load(Ordering::Acquire) {
            if trace && last_trace.elapsed() >= Duration::from_secs(3) {
                let mut output = [0i8; 512];
                unsafe {
                    rdp_manager_graphics_stats(native, output.as_mut_ptr(), output.len());
                }
                eprintln!(
                    "[rdp-graphics] {}",
                    unsafe { CStr::from_ptr(output.as_ptr()) }.to_string_lossy()
                );
                last_trace = std::time::Instant::now();
            }
            for _ in 0..128 {
                match input.try_recv() {
                    Ok(event) => {
                        if unsafe {
                            rdp_manager_input(native, event.kind, event.flags, event.a, event.b)
                        } == 0
                        {
                            break 'session;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break 'session,
                }
            }
            if let Some(event) = control
                .motion
                .lock()
                .ok()
                .and_then(|mut motion| motion.take())
            {
                if unsafe { rdp_manager_input(native, event.kind, event.flags, event.a, event.b) }
                    == 0
                {
                    break;
                }
            }
            if unsafe { rdp_manager_poll(native) } <= 0 {
                break;
            }
        }
    }
    let code = unsafe { rdp_manager_error(native) };
    let name = unsafe { text(rdp_manager_error_name(native)) };
    // Clearing under the same lock as stop() prevents a use-after-free during cancellation.
    if let Ok(mut handle) = control.native.lock() {
        *handle = None;
    }
    unsafe {
        rdp_manager_free(native);
    }
    if !connected && !control.stopped.load(Ordering::Acquire) {
        return Err(RunFailure{code,message:format!("Не удалось подключиться: {name} (0x{code:08X}). Проверьте сервер, сертификат и учётные данные.")});
    }
    Ok(code)
}

/// GTK logical coordinates -> fixed remote desktop, accounting for letterboxing.
pub fn remote_point(
    x: f64,
    y: f64,
    view_w: f64,
    view_h: f64,
    width: u32,
    height: u32,
) -> Option<(u16, u16)> {
    if ![x, y, view_w, view_h].iter().all(|v| v.is_finite())
        || view_w <= 0.0
        || view_h <= 0.0
        || width == 0
        || height == 0
    {
        return None;
    }
    let scale = (view_w / width as f64).min(view_h / height as f64);
    let left = (view_w - width as f64 * scale) / 2.0;
    let top = (view_h - height as f64 * scale) / 2.0;
    let x = (x - left) / scale;
    let y = (y - top) / scale;
    if x < 0.0 || y < 0.0 || x >= width as f64 || y >= height as f64 {
        return None;
    }
    Some((x.floor() as u16, y.floor() as u16))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_backpressure_retains_latest_native_update() {
        let (tx, _input) = mpsc::sync_channel(1);
        let (events, _rx) = mpsc::channel();
        let control = Arc::new(Control {
            stopped: AtomicBool::new(false),
            overflow: AtomicBool::new(false),
            native: Mutex::new(None),
            tx,
            motion: Mutex::new(None),
            frame: Mutex::new(None),
        });
        let mut callbacks = Callbacks {
            control: control.clone(),
            events,
        };
        let opaque = &mut callbacks as *mut Callbacks as *mut c_void;
        let first = [1, 2, 3, 0];
        let last = [4, 5, 6, 0];
        unsafe {
            assert_eq!(frame_callback(opaque, first.as_ptr(), 1, 1, 4), 1);
            assert_eq!(frame_callback(opaque, last.as_ptr(), 1, 1, 4), 2);
            assert_eq!(control.take_frame().unwrap().data, [1, 2, 3, 255]);
            // Native retries its still-dirty buffer, even with no further EndPaint.
            assert_eq!(frame_callback(opaque, last.as_ptr(), 1, 1, 4), 1);
            assert_eq!(control.take_frame().unwrap().data, [4, 5, 6, 255]);
        }
    }

    #[test]
    fn frames_are_bounded_packed_and_opaque() {
        let frame = copy_frame(
            &[1, 2, 3, 0, 99, 99, 99, 99, 4, 5, 6, 0, 99, 99, 99, 99],
            1,
            2,
            8,
        )
        .unwrap();
        assert_eq!(frame.data, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(copy_frame(&[0; 4], 2, 1, 4).is_none());
        assert!(frame_size(8192, 8192, 32768).is_none());
        assert!(frame_size(0, 1, 0).is_none());
    }
    #[test]
    fn pointer_coordinates_include_scale_and_letterbox() {
        assert_eq!(
            remote_point(500.0, 400.0, 1000.0, 800.0, 1280, 720),
            Some((640, 360))
        );
        assert!(remote_point(500.0, 10.0, 1000.0, 800.0, 1280, 720).is_none());
        assert!(remote_point(f64::NAN, 0.0, 100.0, 100.0, 10, 10).is_none());
        assert_eq!(Input::scan(0x11d, false, false).flags, 0x8100);
    }
    #[test]
    #[ignore = "Run scripts/test-rdp.sh: synthetic loopback RDP peer, no desktop capture"]
    fn rdp_live_certificate_frame_input_and_cancellation() {
        let root = std::path::PathBuf::from(
            std::env::var("RDP_MANAGER_NATIVE_FIXTURE").expect("Use scripts/test-rdp.sh"),
        );
        assert!(root
            .to_string_lossy()
            .starts_with("/tmp/rdp-manager-native."));
        let port = std::env::var("RDP_MANAGER_NATIVE_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let config = || Config {
            host: "127.0.0.1".into(),
            port,
            username: "fixture".into(),
            domain: String::new(),
            password: Zeroizing::new("fixture-password".into()),
            width: 640,
            height: 480,
        };
        let (control, events) = start(config()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut prompted = false;
        while std::time::Instant::now() < deadline {
            match events.recv_timeout(Duration::from_secs(2)).unwrap() {
                Event::Certificate(cert, reply) => {
                    assert!(!cert.changed);
                    assert!(!cert.fingerprint.is_empty());
                    prompted = true;
                    reply.send(CertificateDecision::Reject).unwrap();
                }
                Event::Connected => panic!("Rejected certificate connected"),
                Event::Ended { .. } => break,
            }
        }
        assert!(prompted);
        control.stop();
        let mut bad = config();
        bad.password = Zeroizing::new("wrong-password".into());
        let (bad_control, bad_events) = start(bad).unwrap();
        loop {
            match bad_events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Certificate(_, reply) => {
                    reply.send(CertificateDecision::Session).unwrap();
                }
                Event::Connected => panic!("Wrong NLA password connected"),
                Event::Ended { code, .. } => {
                    assert_ne!(code, 0);
                    break;
                }
            }
        }
        bad_control.stop();
        let (control, events) = start(config()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut connected = false;
        let mut painted = false;
        while std::time::Instant::now() < deadline {
            match events.try_recv() {
                Ok(Event::Certificate(cert, reply)) => {
                    assert!(!cert.changed);
                    reply.send(CertificateDecision::Session).unwrap();
                }
                Ok(Event::Connected) => {
                    connected = true;
                    control.send(Input::scan(0x1e, true, false));
                    control.send(Input::scan(0x1e, false, false));
                    control.send(Input {
                        kind: 1,
                        flags: 0,
                        a: 0x41,
                        b: 0,
                    });
                    control.send(Input::mouse(0x9000, 10, 12));
                    control.send(Input::mouse(0x1000, 10, 12));
                }
                Ok(Event::Ended { message, .. }) => panic!("Unexpected disconnect: {message}"),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
            if let Some(frame) = control.take_frame() {
                assert!(
                    std::fs::read_to_string(root.join("input.log"))
                        .unwrap()
                        .contains("FRAME_END"),
                    "Published an unfinished server frame"
                );
                assert_eq!((frame.width, frame.height), (640, 480));
                assert_eq!(&frame.data[..4], &[0x11, 0x22, 0xCC, 255]);
                painted = true;
            }
            if painted
                && std::fs::read_to_string(root.join("input.log"))
                    .unwrap_or_default()
                    .contains("M:36864:10:12")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(connected && painted);
        let log = std::fs::read_to_string(root.join("input.log")).unwrap();
        assert!(log.contains("K:0:30"));
        assert!(log.contains("U:0:65"));
        assert!(log.contains("M:36864:10:12"));
        control.send(Input::scan(29, true, false));
        control.send(Input::mouse(0xA000, 10, 12));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let log = std::fs::read_to_string(root.join("input.log")).unwrap();
            if log.contains("K:0:29") && log.contains("M:40960:10:12") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        control.stop();
        let mut ended = false;
        while let Ok(event) = events.recv_timeout(Duration::from_secs(3)) {
            if let Event::Ended { cancelled, .. } = event {
                assert!(cancelled);
                ended = true;
                break;
            }
        }
        assert!(ended);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut released = false;
        while std::time::Instant::now() < deadline {
            let log = std::fs::read_to_string(root.join("input.log")).unwrap();
            if log.contains("K:32768:29") && log.contains("M:8192:10:12") {
                released = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(released);
        // Session-only acceptance must prompt again; stopping while it is pending must not hang.
        let (control, events) = start(config()).unwrap();
        match events.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Certificate(_, reply) => {
                control.stop();
                drop(reply);
            }
            Event::Ended { message, .. } => panic!("No prompt: {message}"),
            Event::Connected => panic!("Certificate was persisted unexpectedly"),
        }
        loop {
            if matches!(
                events.recv_timeout(Duration::from_secs(3)).unwrap(),
                Event::Ended { .. }
            ) {
                break;
            }
        }
        // Explicit remember must survive a new context. Changed certificates still fail.
        for attempt in 0..3 {
            let (control, events) = start(config()).unwrap();
            let mut prompted = false;
            let mut connected = false;
            let mut changed = false;
            loop {
                match events.recv_timeout(Duration::from_secs(5)).unwrap() {
                    Event::Certificate(cert, reply) => {
                        prompted = true;
                        changed = cert.changed;
                        if attempt == 0 {
                            assert!(!changed);
                            reply.send(CertificateDecision::Remember).unwrap();
                        } else if attempt == 1 {
                            panic!("Remembered certificate prompted again");
                        } else {
                            assert!(changed);
                        }
                    }
                    Event::Connected => {
                        connected = true;
                        control.stop();
                    }
                    Event::Ended { .. } => break,
                }
            }
            if attempt == 0 {
                assert!(prompted && connected);
            }
            if attempt == 1 {
                assert!(!prompted && connected);
            }
            if attempt == 2 {
                assert!(prompted && changed && !connected);
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stalled_port = listener.local_addr().unwrap().port();
        let (accepted, wait) = mpsc::channel();
        let (release, done) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            accepted.send(()).unwrap();
            let _ = done.recv_timeout(Duration::from_secs(10));
            drop(socket);
        });
        let mut stalled = config();
        stalled.port = stalled_port;
        let (control, events) = start(stalled).unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        control.stop();
        let ended = matches!(
            events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Ended {
                cancelled: true,
                ..
            }
        );
        let _ = release.send(());
        server.join().unwrap();
        assert!(ended);
    }
}
