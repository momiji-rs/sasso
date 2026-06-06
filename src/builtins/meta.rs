//! Meta / introspection built-ins (`type-of`, `unit`, `unitless`,
//! `inspect`, `comparable`, …).
//!
//! Fill in `try_call` for this family. Shared argument helpers live in the
//! parent module: `super::{arg, require, num, as_color, channel, clamp01}`.

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
