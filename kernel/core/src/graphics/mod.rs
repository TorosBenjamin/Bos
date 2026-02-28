use embedded_graphics::prelude::Point;

pub mod display;
pub mod frame_buffer_embedded_graphics;
pub mod frame_buffer_info;
pub mod rgb_pixel;
pub mod writer;

pub struct DisplayData {
    pub position: Point,
}
