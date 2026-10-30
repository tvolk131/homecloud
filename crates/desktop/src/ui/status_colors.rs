//! Shared semantic status colors. Preserve the current dark appearance.
use iced::Color;

pub(super) const SUCCESS_CONTAINER: Color = Color::from_rgb8(24, 58, 46);
pub(super) const ON_SUCCESS_CONTAINER: Color = Color::from_rgb8(154, 224, 183);
pub(super) const SUCCESS: Color = ON_SUCCESS_CONTAINER;
pub(super) const WARNING: Color = Color::from_rgb8(255, 207, 112);
