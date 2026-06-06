//! Math built-in functions (global `floor`, `ceil`, `round`, `abs`, …).
//!
//! Fill in `try_call` for this family. Shared argument helpers live in the
//! parent module: `super::{arg, require, num, as_color, channel, clamp01}`.
//! Return `Some(Ok(..))`/`Some(Err(..))` for a name this family owns, or
//! `None` to let the next family try.

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
