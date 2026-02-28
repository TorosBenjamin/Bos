use core::fmt::Display;
use core::fmt::Write;
use log::{Level, LevelFilter, Log};
use owo_colors::OwoColorize;
use uart_16550::SerialPort;
use unicode_segmentation::UnicodeSegmentation;
use crate::memory;

struct Inner {
    serial_port: SerialPort,
}

impl Inner {
    fn write_with_color(&mut self, color: Color, string: impl Display) {
        let string: &dyn Display = match color {
            Color::Default => &string,
            Color::Gray => &string.dimmed(),
            Color::BrightRed => &string.bright_red(),
            Color::BrightYellow => &string.bright_yellow(),
            Color::BrightBlue => &string.bright_blue(),
            Color::BrightCyan => &string.bright_cyan(),
            Color::BrightMagenta => &string.bright_magenta(),
            Color::BrightGreen => &string.bright_green(),
        };
        let mut writer = WriterWithCr::new(&mut self.serial_port);
        write!(writer, "{string}").unwrap();
    }
}

struct KernelLogger {
    inner: spin::Mutex<Inner>,
}

static LOGGER: KernelLogger = KernelLogger {
    inner: spin::Mutex::new(Inner {
        serial_port: unsafe { SerialPort::new(0x3f8) },
    }),
};

impl Log for KernelLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        let mut inner = self.inner.lock();
        let level = record.level();
        inner.write_with_color(
            match level {
                Level::Error => Color::BrightRed,
                Level::Warn => Color::BrightYellow,
                Level::Info => Color::BrightBlue,
                Level::Debug => Color::BrightCyan,
                Level::Trace => Color::BrightMagenta,
            },
            format_args!("{level:5} "),
        );
        let cpu_id =
            memory::cpu_local_data::try_get_local().map_or(0, |data| data.kernel_id);
        let width = match memory::cpu_local_data::cpus_count() {
            1 => 1,
            n => (n - 1).ilog(16) as usize + 1,
        };
        inner.write_with_color(Color::Gray, format_args!("[{cpu_id:0width$X}] "));
        inner.write_with_color(Color::Default, record.args());
        inner.write_with_color(Color::Default, "\n");
    }

    fn flush(&self) {}
}

pub fn init() -> Result<(), log::SetLoggerError> {
    let mut inner = LOGGER.inner.try_lock().unwrap();
    inner.serial_port.init();
    log::set_max_level(LevelFilter::max());
    log::set_logger(&LOGGER)
}

struct WriterWithCr<T> {
    writer: T,
}

impl<T> WriterWithCr<T> {
    pub const fn new(writer: T) -> Self {
        Self { writer }
    }
}

impl<T: Write> Write for WriterWithCr<T> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.graphemes(true) {
            match c {
                "\n" => self.writer.write_str("\r\n")?,
                s => self.writer.write_str(s)?,
            }
        }
        Ok(())
    }
}

enum Color {
    Default,
    Gray,
    BrightRed,
    BrightYellow,
    BrightBlue,
    BrightCyan,
    BrightMagenta,
    #[allow(dead_code)]
    BrightGreen,
}
