use embedded_graphics::prelude::Point;

pub mod rgb_pixel;
pub mod frame_buffer_info;
pub mod frame_buffer_embedded_graphics;
pub mod writer;
pub mod display;

pub struct DisplayData {
    pub position: Point
}
