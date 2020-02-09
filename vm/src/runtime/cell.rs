use super::module::Module;
use super::value::*;
use crate::heap::space::Space;
use crate::util::arc::Arc;
use crate::util::tagged::*;
use alloc::string::String;
use alloc::vec::Vec;
pub const MIN_OLD_SPACE_GENERATION: u8 = 5;

pub enum NativeResult {
    Error(Value),
    Ok(Value),
    YieldProcess,
}

pub type NativeFn = extern "C" fn(Value, &[Value]) -> NativeResult;
pub struct Function {
    pub name: Arc<String>,
    pub upvalues: Vec<Value>,
    pub argc: i32,
    pub native: Option<NativeFn>,
    pub module: Arc<Module>,
}

pub enum CellValue {
    None,
    Number(f64),
    Bool(bool),
    String(Arc<String>),
    Array(Vec<Value>),
    ByteArray(Vec<u8>),
    Function(Function),
    Module(Arc<Module>),
}

pub const MARK_BIT: usize = 0;
pub struct Cell {
    pub value: CellValue,
    pub prototype: Option<CellPointer>,
    pub attributes: TaggedPointer<AttributesMap>,
    pub generation: u8,
    pub soft: bool,
    pub mark: bool,
    pub forward: crate::util::mem::Address,
}

pub type AttributesMap = lru::LruCache<Arc<String>, Value>;

impl Cell {
    pub fn trace<F>(&self, mut cb: F)
    where
        F: FnMut(*const CellPointer),
    {
        if let Some(ref prototype) = &self.prototype {
            cb(prototype)
        }
        if self.attributes.is_null() == false {
            for (_, attribute) in self.attributes.as_ref().unwrap().iter() {
                if attribute.is_cell() {
                    cb(&attribute.as_cell());
                }
            }
        }
    }
}
pub struct CellPointer {
    pub raw: TaggedPointer<Cell>,
}

impl CellPointer {
    pub fn copy_to(
        &self,
        old_space: &mut Space,
        new_space: &mut Space,
        needs_gc: &mut bool,
    ) -> CellPointer {
        self.increment_generation();
        let result;
        if self.get().generation >= 5 {
            result = old_space.allocate(std::mem::size_of::<Cell>(), needs_gc);
        } else {
            result = new_space.allocate(std::mem::size_of::<Cell>(), needs_gc);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                result.to_mut_ptr::<u8>(),
                std::mem::size_of::<Self>(),
            );
        }
        CellPointer {
            raw: TaggedPointer::new(result.to_mut_ptr()),
        }
    }

    pub fn increment_generation(&self) {
        let cell = self.get_mut();
        if cell.generation < MIN_OLD_SPACE_GENERATION {
            cell.generation += 1;
        }
    }

    pub fn is_marked(&self) -> bool {
        self.get().mark
    }

    pub fn mark(&self, value: bool) {
        self.get_mut().mark = value;
    }

    pub fn is_soft_marked(&self) -> bool {
        self.get().soft
    }

    pub fn soft_mark(&self, value: bool) {
        self.get_mut().soft = value;
    }

    pub fn get(&self) -> &Cell {
        self.raw.as_ref().unwrap()
    }

    pub fn get_mut(&self) -> &mut Cell {
        self.raw.as_mut().unwrap()
    }
}

impl Copy for CellPointer {}
impl Clone for CellPointer {
    fn clone(&self) -> Self {
        *self
    }
}
