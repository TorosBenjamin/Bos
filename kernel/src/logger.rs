use core::fmt::Display;
use core::fmt::Write;
use embedded_graphics::geometry::Point;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use kernel::graphics::DisplayData;
use kernel::graphics::writer::Writer;
use log::{Level, LevelFilter, Log};
use owo_colors::OwoColorize;
use uart_16550::SerialPort;
use unicode_segmentation::UnicodeSegmentation;

struct Inner {
    serial_port: SerialPort,
    display: Option<DisplayData>,
}

impl Inner {
    fn write_with_color(&mut self, color: Color, string: impl Display) {
        // Write to serial
        {
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

        let mut writer = WriterWithCr::new(&mut self.serial_port);
        write!(writer, "{string}").unwrap();

        // Write to screen
        if let Some(display_data) = &mut self.display {
            let mut writer = Writer {
                position: &mut display_data.position,
                text_color: match color {
                    Color::Default => Rgb888::WHITE,
                    // Mimick the ANSI escape colors
                    Color::Gray => Rgb888::new(128, 128, 128),
                    Color::BrightRed => Rgb888::new(255, 85, 85),
                    Color::BrightYellow => Rgb888::new(255, 255, 85),
                    Color::BrightBlue => Rgb888::new(85, 85, 255),
                    Color::BrightCyan => Rgb888::new(85, 255, 255),
                    Color::BrightMagenta => Rgb888::new(255, 85, 255),
                    Color::BrightGreen => Rgb888::GREEN,
                },
            };
            write!(writer, "{string}").unwrap();
        }
    }
}

struct KernelLogger {
    inner: spin::Mutex<Inner>,
}

static LOGGER: KernelLogger = KernelLogger {
    inner: spin::Mutex::new(Inner {
        serial_port: unsafe { SerialPort::new(0x3f8) },
        display: None,
    }),
};

impl Log for KernelLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        todo!()
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
            kernel::memory::cpu_local_data::try_get_local().map_or(0, |data| data.kernel_id);
        let width = match kernel::memory::cpu_local_data::cpus_count() {
            1 => 1,
            n => (n - 1).ilog(16) as usize + 1,
        };
        inner.write_with_color(Color::Gray, format_args!("[{cpu_id:0width$X}] "));
        inner.write_with_color(Color::Default, record.args());
        inner.write_with_color(Color::Default, "\n");
    }

    fn flush(&self) {
        todo!()
    }
}

pub fn init() -> Result<(), log::SetLoggerError> {
    let mut inner = LOGGER.inner.try_lock().unwrap();
    &mut inner.serial_port.init();
    inner.display = Some(DisplayData {
        position: Point::zero(),
    });
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
    BrightGreen,
}
