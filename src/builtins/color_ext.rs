//! Extended color built-ins (`adjust-hue`, `saturate`, `desaturate`,
//! `complement`, `invert`, `grayscale`, `opacify`/`transparentize`, the
//! `hue`/`saturation`/`lightness` getters, …).
//!
//! Fill in `try_call` for this family. Shared argument helpers live in the
//! parent module: `super::{arg, require, num, as_color, channel, clamp01}`;
//! `super::color::rgb_repr` formats `rgb()`/`rgba()` representations.

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::Value;

pub(super) fn try_call(
    _name: &str,
    _pos_args: &[Value],
    _named: &[(String, Value)],
    _pos: Pos,
) -> Option<Result<Value, Error>> {
    None
}
