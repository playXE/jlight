use super::value::*;
use crate::bytecode::*;
use crate::util::arc::Arc;
use std::vec::Vec;
pub struct Module {
    pub globals: Vec<Value>,
}
