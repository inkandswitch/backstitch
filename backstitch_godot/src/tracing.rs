use std::io::Write;
use std::{fmt, path::PathBuf, sync::Mutex};

use godot::classes::ProjectSettings;
use godot::obj::Singleton;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{
        format::{FmtSpan, Writer},
        time::FormatTime,
    },
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

fn get_backstitch_dir() -> PathBuf {
    let path = ProjectSettings::singleton()
        .globalize_path("res://")
        .to_string();
    let path = PathBuf::from(path).join(".backstitch");
    let _ = std::fs::create_dir_all(&path);
    path
}

struct CompactTime;
impl FormatTime for CompactTime {
    fn format_time(&self, w: &mut Writer<'_>) -> Result<(), std::fmt::Error> {
        write!(w, "{}", TimeNoDate::from(std::time::SystemTime::now()))
    }
}
static mut M_FILE_WRITER_MUTEX: Option<WorkerGuard> = None;

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;
static PREVIOUS_PANIC_HOOK: Mutex<Option<PanicHook>> = Mutex::new(None);

#[cfg(feature = "tokio-console")]
struct ConsoleServer {
    shutdown: tokio::sync::oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(feature = "tokio-console")]
static CONSOLE_SERVER: Mutex<Option<ConsoleServer>> = Mutex::new(None);

/// spawns the console subscriber layer and returns a layer that can be used to add to the tracing registry.
/// console_subscriber::ConsoleLayer::builder().spawn() does not return the thread handle, so we need to spawn it ourselves in order to be able to join it later.
#[cfg(feature = "tokio-console")]
fn spawn_console_layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn console_filter(meta: &tracing::Metadata<'_>) -> bool {
        // events will have *targets* beginning with "runtime"
        if meta.is_event() {
            return meta.target().starts_with("runtime") || meta.target().starts_with("tokio");
        }

        // spans will have *names* beginning with "runtime". for backwards
        // compatibility with older Tokio versions, enable anything with the `tokio`
        // target as well.
        meta.name().starts_with("runtime.") || meta.target().starts_with("tokio")
    }

    let (layer, server) = console_subscriber::ConsoleLayer::builder()
        .with_default_env()
        .build();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("console_subscriber".into())
        .spawn(move || {
            // Discard this thread's traces so the server cannot recurse into our subscriber.
            let _subscriber_guard =
                tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("console subscriber runtime initialization failed");
            runtime.block_on(async move {
                tokio::select! {
                    result = server.serve() => {
                        if let Err(err) = result {
                            eprintln!("console subscriber server failed: {err}");
                        }
                    }
                    _ = shutdown_rx => {}
                }
            });
            // Dropping the runtime aborts the listener and aggregator and releases the port.
            drop(runtime);
        })
        .expect("console subscriber could not spawn thread");
    *CONSOLE_SERVER.lock().unwrap_or_else(|err| err.into_inner()) = Some(ConsoleServer {
        shutdown: shutdown_tx,
        thread,
    });
    layer.with_filter(tracing_subscriber::filter::FilterFn::new(console_filter))
}

pub fn initialize_tracing() {
    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .max_log_files(5)
        .filename_prefix("backstitch.log")
        .build(get_backstitch_dir())
        .expect("Failed to initialize rolling file appender!");
    let (non_blocking_file_writer, _guard) = tracing_appender::non_blocking(file_appender);
    // if the mutex gets dropped, the file writer will be closed, so we need to keep it alive
    unsafe {
        M_FILE_WRITER_MUTEX = Some(_guard);
    }
    println!("!!! Logging to {:?}/backstitch.log", get_backstitch_dir());

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_timer(CompactTime)
        .compact()
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(CustomStdoutWriter::custom_stdout)
        .with_filter(
            EnvFilter::new("off")
                // .add_directive("tokio=trace".parse().unwrap())
                // .add_directive("runtime=trace".parse().unwrap())
                .add_directive("backstitch_godot=debug".parse().unwrap())
                .add_directive("backstitch=debug".parse().unwrap())
                .add_directive("tracing_panic=info".parse().unwrap()),
            // .add_directive("samod=info".parse().unwrap())
            // .add_directive("samod_core=info".parse().unwrap()),
        );
    let file_layer = tracing_subscriber::fmt::layer()
        .with_line_number(true)
        .with_ansi(false)
        .with_writer(non_blocking_file_writer.clone())
        .with_filter(
            EnvFilter::new("info")
                .add_directive("backstitch_godot=debug".parse().unwrap())
                .add_directive("backstitch=debug".parse().unwrap())
                .add_directive("samod=info".parse().unwrap())
                .add_directive("samod_core=info".parse().unwrap())
                .add_directive("tracing_panic=info".parse().unwrap()),
        );

    #[cfg(feature = "tokio-console")]
    let subscriber = tracing_subscriber::registry()
        .with(spawn_console_layer())
        .with(stdout_layer)
        .with(file_layer)
        .try_init();

    #[cfg(not(feature = "tokio-console"))]
    let subscriber = tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .try_init();

    // Retain the previous panic hook so we can restore it later.
    let previous_hook = std::panic::take_hook();
    *PREVIOUS_PANIC_HOOK
        .lock()
        .unwrap_or_else(|err| err.into_inner()) = Some(previous_hook);
    std::panic::set_hook(Box::new(|panic_info| {
        tracing_panic::panic_hook(panic_info);
        if let Some(hook) = PREVIOUS_PANIC_HOOK
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
        {
            hook(panic_info);
        }
    }));

    if let Err(e) = subscriber {
        tracing::error!("Failed to initialize tracing subscriber: {:?}", e);
    } else {
        tracing::info!("Tracing subscriber initialized");
    }
}

pub(crate) fn deinitialize_tracing() {
    //woop
    #[cfg(feature = "tokio-console")]
    if let Some(server) = CONSOLE_SERVER
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
    {
        let _ = server.shutdown.send(());
        let _ = server.thread.join();
    }

    unsafe {
        M_FILE_WRITER_MUTEX = None;
    }

    if let Some(previous) = PREVIOUS_PANIC_HOOK
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
    {
        std::panic::set_hook(previous);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimeNoDate {
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanos: u32,
}

impl fmt::Display for TimeNoDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            // "-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
            "{:02}:{:02}:{:02}.{:06}",
            self.hour,
            self.minute,
            self.second,
            self.nanos / 1_000
        )
    }
}

impl From<std::time::SystemTime> for TimeNoDate {
    fn from(timestamp: std::time::SystemTime) -> TimeNoDate {
        let (t, nanos) = match timestamp.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => {
                debug_assert!(duration.as_secs() <= i64::MAX as u64);
                (duration.as_secs() as i64, duration.subsec_nanos())
            }
            Err(error) => {
                let duration = error.duration();
                debug_assert!(duration.as_secs() <= i64::MAX as u64);
                let (secs, nanos) = (duration.as_secs() as i64, duration.subsec_nanos());
                if nanos == 0 {
                    (-secs, 0)
                } else {
                    (-secs - 1, 1_000_000_000 - nanos)
                }
            }
        };

        // 2000-03-01 (mod 400 year, immediately after feb29
        const LEAPOCH: i64 = 946_684_800 + 86400 * (31 + 29);
        const DAYS_PER_400Y: i32 = 365 * 400 + 97;
        const DAYS_PER_100Y: i32 = 365 * 100 + 24;
        const DAYS_PER_4Y: i32 = 365 * 4 + 1;
        static DAYS_IN_MONTH: [i8; 12] = [31, 30, 31, 30, 31, 31, 30, 31, 30, 31, 31, 29];

        // Note(dcb): this bit is rearranged slightly to avoid integer overflow.
        let mut days: i64 = (t / 86_400) - (LEAPOCH / 86_400);
        let mut remsecs: i32 = (t % 86_400) as i32;
        if remsecs < 0i32 {
            remsecs += 86_400;
            days -= 1
        }

        let mut qc_cycles: i32 = (days / i64::from(DAYS_PER_400Y)) as i32;
        let mut remdays: i32 = (days % i64::from(DAYS_PER_400Y)) as i32;
        if remdays < 0 {
            remdays += DAYS_PER_400Y;
            qc_cycles -= 1;
        }

        let mut c_cycles: i32 = remdays / DAYS_PER_100Y;
        if c_cycles == 4 {
            c_cycles -= 1;
        }
        remdays -= c_cycles * DAYS_PER_100Y;

        let mut q_cycles: i32 = remdays / DAYS_PER_4Y;
        if q_cycles == 25 {
            q_cycles -= 1;
        }
        remdays -= q_cycles * DAYS_PER_4Y;

        let mut remyears: i32 = remdays / 365;
        if remyears == 4 {
            remyears -= 1;
        }
        remdays -= remyears * 365;

        let mut years: i64 = i64::from(remyears)
            + 4 * i64::from(q_cycles)
            + 100 * i64::from(c_cycles)
            + 400 * i64::from(qc_cycles);

        let mut months: i32 = 0;
        while i32::from(DAYS_IN_MONTH[months as usize]) <= remdays {
            remdays -= i32::from(DAYS_IN_MONTH[months as usize]);
            months += 1
        }

        if months >= 10 {
            months -= 12;
            years += 1;
        }

        TimeNoDate {
            year: years + 2000,
            month: (months + 3) as u8,
            day: (remdays + 1) as u8,
            hour: (remsecs / 3600) as u8,
            minute: (remsecs / 60 % 60) as u8,
            second: (remsecs % 60) as u8,
            nanos,
        }
    }
}

// custom stdout Writer
pub struct CustomStdoutWriter {
    inner: std::io::Stdout,
}
impl CustomStdoutWriter {
    pub fn custom_stdout() -> CustomStdoutWriter {
        CustomStdoutWriter {
            inner: std::io::stdout(),
        }
    }
}

// the formatting in tracing-subscriber REALLY SUCKS, so we need to just search-and-replace the output strings
// Search and replace for the level names
const LEVEL_NAMES_TO_REPLACEMENT: &[(&str, &str)] = &[
    ("TRACE", "T"),
    ("DEBUG", "D"),
    (" INFO", "I"), // extra space because INFO is 4 letters long
    (" WARN", "W"),
    ("ERROR", "X"),
];

const CRATE_NAME: &str = "backstitch_godot";

impl Write for CustomStdoutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let s = String::from_utf8_lossy(buf);
        let s =
		// replace the level names
		LEVEL_NAMES_TO_REPLACEMENT.iter().fold(s.to_string(), |acc, (from, to)| acc.replace(from, to))
		.replace(CRATE_NAME, "<PWRC>");

        let size_diff = buf.len() - s.len();
        let actual_written = self.inner.write(s.as_bytes())?;
        Ok(actual_written + size_diff)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
