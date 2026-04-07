use std::fmt;
use std::io::Write;

use godot::classes::ProjectSettings;
use godot::obj::Singleton;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{format::Writer, time::FormatTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

fn get_user_dir() -> String {
    let user_dir = ProjectSettings::singleton()
        .globalize_path("user://")
        .to_string();
    user_dir
}

struct CompactTime;
impl FormatTime for CompactTime {
    fn format_time(&self, w: &mut Writer<'_>) -> Result<(), std::fmt::Error> {
        write!(w, "{}", TimeNoDate::from(std::time::SystemTime::now()))
    }
}
static mut M_FILE_WRITER_MUTEX: Option<WorkerGuard> = None;
pub fn initialize_tracing() {
    let file_appender = tracing_appender::rolling::daily(get_user_dir(), "backstitch.log");
    let (non_blocking_file_writer, _guard) = tracing_appender::non_blocking(file_appender);
    // if the mutex gets dropped, the file writer will be closed, so we need to keep it alive
    unsafe {
        M_FILE_WRITER_MUTEX = Some(_guard);
    }
    println!("!!! Logging to {:?}/backstitch.log", get_user_dir());

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_timer(CompactTime)
        .compact()
        // .with_span_events(FmtSpan::ENTER | FmtSpan::CLOSE)
        .with_writer(CustomStdoutWriter::custom_stdout)
        .with_filter(
            EnvFilter::new("info")
                // .add_directive("tokio=trace".parse().unwrap())
                // .add_directive("runtime=trace".parse().unwrap())
                .add_directive("backstitch_rust_core=debug".parse().unwrap())
                .add_directive("samod=info".parse().unwrap())
                .add_directive("samod_core=info".parse().unwrap()),
        );
    let file_layer = tracing_subscriber::fmt::layer()
        .with_line_number(true)
        .with_ansi(false)
        .with_writer(non_blocking_file_writer.clone())
        .with_filter(
            EnvFilter::new("info")
                .add_directive("backstitch_rust_core=trace".parse().unwrap())
                .add_directive("samod=info".parse().unwrap())
                .add_directive("samod_core=info".parse().unwrap()),
        );

    #[cfg(feature = "tokio-console")]
    let subscriber = tracing_subscriber::registry()
        .with(
            console_subscriber::ConsoleLayer::builder()
                .with_default_env()
                .spawn(),
        )
        .with(stdout_layer)
        .with(file_layer)
        .try_init();

    #[cfg(not(feature = "tokio-console"))]
    let subscriber = tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .try_init();

    if let Err(e) = subscriber {
        tracing::error!("Failed to initialize tracing subscriber: {:?}", e);
    } else {
        tracing::info!("Tracing subscriber initialized");
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
        // if self.year > 9999 {
        //     write!(f, "+{}", self.year)?;
        // } else if self.year < 0 {
        //     write!(f, "{:05}", self.year)?;
        // } else {
        //     write!(f, "{:04}", self.year)?;
        // }

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

const CRATE_NAME: &str = "backstitch_rust_core";

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

// pub(crate) struct CustomJSONStdoutWriter{
// 	inner: std::io::Stdout,
// }
// impl CustomJSONStdoutWriter {
// 	pub fn custom_json_stdout() -> CustomJSONStdoutWriter {
// 		CustomJSONStdoutWriter {
// 			inner: std::io::stdout(),
// 		}
// 	}
// }

// impl Write for CustomJSONStdoutWriter {
//     fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
// 		// deserialize the entire fucking json object
// 		let mut json: serde_json::Value = serde_json::from_slice(buf)?;
// 		// replace the level names
// 		let level = json["level"].as_str().unwrap();
// 		let level = LEVEL_NAMES_TO_REPLACEMENT.iter().find(|(from, _)| *from == level).unwrap().1;
// 		let mut json_mut = json.as_object_mut().unwrap();
// 		json_mut.insert("level".to_string(), serde_json::Value::String(level.to_string()));
// 		let s = json.to_string();
// 		println!("{}", s);
// 		// serialize the json object back to a string
//         // let s = String::from_utf8_lossy(buf);
//         // let s = LEVEL_NAMES_TO_REPLACEMENT.iter().fold(s.to_string(), |acc, (from, to)| acc.replace(from, to));
//         // self.inner.write(s.as_bytes())
// 		Ok(buf.len())
//     }
//     fn flush(&mut self) -> std::io::Result<()> {
//         self.inner.flush()
//     }
// }

// // This needs to be a seperate impl block because they place different bounds on the type parameters.
// impl<S, N, E, W> Layer<S, N, E, W>
// where
//     S: Subscriber + for<'a> LookupSpan<'a>,
//     N: for<'writer> FormatFields<'writer> + 'static,
//     W: for<'writer> MakeWriter<'writer> + 'static,
// {
//     /// Sets the [event formatter][`FormatEvent`] that the layer being built will
//     /// use to format events.
//     ///
//     /// The event formatter may be any type implementing the [`FormatEvent`]
//     /// trait, which is implemented for all functions taking a [`FmtContext`], a
//     /// [`Writer`], and an [`Event`].
//     ///
//     /// # Examples
//     ///
//     /// Setting a type implementing [`FormatEvent`] as the formatter:
//     /// ```rust
//     /// use tracing_subscriber::fmt::{self, format};
//     ///
//     /// let layer = fmt::layer()
//     ///     .event_format(format().compact());
//     /// # // this is necessary for type inference.
//     /// # use tracing_subscriber::Layer as _;
//     /// # let _ = layer.with_subscriber(tracing_subscriber::registry::Registry::default());
//     /// ```
//     /// [`FormatEvent`]: format::FormatEvent
//     /// [`Event`]: tracing::Event
//     /// [`Writer`]: format::Writer
//     pub fn event_format<E2>(self, e: E2) -> Layer<S, N, E2, W>
//     where
//         E2: FormatEvent<S, N> + 'static,
//     {
//         Layer {
//             fmt_fields: self.fmt_fields,
//             fmt_event: e,
//             fmt_span: self.fmt_span,
//             make_writer: self.make_writer,
//             is_ansi: self.is_ansi,
//             log_internal_errors: self.log_internal_errors,
//             _inner: self._inner,
//         }
//     }

//     /// Updates the event formatter by applying a function to the existing event formatter.
//     ///
//     /// This sets the event formatter that the layer being built will use to record fields.
//     ///
//     /// # Examples
//     ///
//     /// Updating an event formatter:
//     ///
//     /// ```rust
//     /// let layer = tracing_subscriber::fmt::layer()
//     ///     .map_event_format(|e| e.compact());
//     /// # // this is necessary for type inference.
//     /// # use tracing_subscriber::Layer as _;
//     /// # let _ = layer.with_subscriber(tracing_subscriber::registry::Registry::default());
//     /// ```
//     pub fn map_event_format<E2>(self, f: impl FnOnce(E) -> E2) -> Layer<S, N, E2, W>
//     where
//         E2: FormatEvent<S, N> + 'static,
//     {
//         Layer {
//             fmt_fields: self.fmt_fields,
//             fmt_event: f(self.fmt_event),
//             fmt_span: self.fmt_span,
//             make_writer: self.make_writer,
//             is_ansi: self.is_ansi,
//             log_internal_errors: self.log_internal_errors,
//             _inner: self._inner,
//         }
//     }
// }

// impl<S, N> FormatEvent<S, N>
//     for fn(ctx: &FmtContext<'_, S, N>, Writer<'_>, &Event<'_>) -> fmt::Result
// where
//     S: Subscriber + for<'a> LookupSpan<'a>,
//     N: for<'a> FormatFields<'a> + 'static,
// {
//     fn format_event(
//         &self,
//         ctx: &FmtContext<'_, S, N>,
//         writer: Writer<'_>,
//         event: &Event<'_>,
//     ) -> fmt::Result {
//         (*self)(ctx, writer, event)
//     }
// }
// pub trait FormatEventEXT<S, N>
// where
//     S: Subscriber + for<'a> LookupSpan<'a>,
//     N: for<'a> FormatFields<'a> + 'static,
// {
//     /// Write a log message for `Event` in `Context` to the given [`Writer`].
//     fn format_event(
//         &self,
//         ctx: &FmtContext<'_, S, N>,
//         writer: Writer<'_>,
//         event: &Event<'_>,
//     ) -> fmt::Result;
// }

// impl<S, N, T> FormatEventEXT<S, N> for Format<Compact, T>
// where
//     S: Subscriber + for<'a> LookupSpan<'a>,
//     N: for<'a> FormatFields<'a> + 'static,
//     T: FormatTime,
// {
//     fn format_event(
//         &self,
//         ctx: &FmtContext<'_, S, N>,
//         mut writer: Writer<'_>,
//         event: &Event<'_>,
//     ) -> fmt::Result {
//         #[cfg(feature = "tracing-log")]
//         let normalized_meta = event.normalized_metadata();
//         #[cfg(feature = "tracing-log")]
//         let meta = normalized_meta.as_ref().unwrap_or_else(|| event.metadata());
//         #[cfg(not(feature = "tracing-log"))]
//         let meta = event.metadata();

//         // if the `Format` struct *also* has an ANSI color configuration,
//         // override the writer...the API for configuring ANSI color codes on the
//         // `Format` struct is deprecated, but we still need to honor those
//         // configurations.
//         if let Some(ansi) = self.ansi {
//             writer = writer.with_ansi(ansi);
//         }

//         self.format_timestamp(&mut writer)?;

//         if self.display_level {
//             let fmt_level = {
//                 #[cfg(feature = "ansi")]
//                 {
//                     FmtLevel::new(meta.level(), writer.has_ansi_escapes())
//                 }
//                 #[cfg(not(feature = "ansi"))]
//                 {
//                     FmtLevel::new(meta.level())
//                 }
//             };
//             write!(writer, "{} ", fmt_level)?;
//         }

//         if self.display_thread_name {
//             let current_thread = std::thread::current();
//             match current_thread.name() {
//                 Some(name) => {
//                     write!(writer, "{} ", FmtThreadName::new(name))?;
//                 }
//                 // fall-back to thread id when name is absent and ids are not enabled
//                 None if !self.display_thread_id => {
//                     write!(writer, "{:0>2?} ", current_thread.id())?;
//                 }
//                 _ => {}
//             }
//         }

//         if self.display_thread_id {
//             write!(writer, "{:0>2?} ", std::thread::current().id())?;
//         }

//         let fmt_ctx = {
//             #[cfg(feature = "ansi")]
//             {
//                 FmtCtx::new(ctx, event.parent(), writer.has_ansi_escapes())
//             }
//             #[cfg(not(feature = "ansi"))]
//             {
//                 FmtCtx::new(&ctx, event.parent())
//             }
//         };
//         write!(writer, "{}", fmt_ctx)?;

//         let dimmed = writer.dimmed();

//         let mut needs_space = false;
//         if self.display_target {
//             write!(
//                 writer,
//                 "{}{}",
//                 dimmed.paint(meta.target()),
//                 dimmed.paint(":")
//             )?;
//             needs_space = true;
//         }

//         if self.display_filename {
//             if let Some(filename) = meta.file() {
//                 if self.display_target {
//                     writer.write_char(' ')?;
//                 }
//                 write!(writer, "{}{}", dimmed.paint(filename), dimmed.paint(":"))?;
//                 needs_space = true;
//             }
//         }

//         if self.display_line_number {
//             if let Some(line_number) = meta.line() {
//                 write!(
//                     writer,
//                     "{}{}{}{}",
//                     dimmed.prefix(),
//                     line_number,
//                     dimmed.suffix(),
//                     dimmed.paint(":")
//                 )?;
//                 needs_space = true;
//             }
//         }

//         if needs_space {
//             writer.write_char(' ')?;
//         }

//         ctx.format_fields(writer.by_ref(), event)?;

//         for span in ctx
//             .event_scope()
//             .into_iter()
//             .flat_map(crate::registry::Scope::from_root)
//         {
//             let exts = span.extensions();
//             if let Some(fields) = exts.get::<FormattedFields<N>>() {
//                 if !fields.is_empty() {
//                     write!(writer, " {}", dimmed.paint(&fields.fields))?;
//                 }
//             }
//         }
//         writeln!(writer)
//     }
// }
