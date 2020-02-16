//! Incremental mark&sweep garbage collector.
//!
//! This GC is based on mruby gc.
use crate::heap::freelist::FreeList;
use crate::heap::freelist_alloc::FreeListAllocator;
use crate::runtime::cell::*;
use crate::runtime::process::*;
use crate::runtime::value::*;
use crate::util::arc::Arc;
use crate::util::mem::{Address, Region};

fn paint_grey(o: CellPointer) {
    o.get_mut().color = CELL_GREY;
}

fn paint_black(o: CellPointer) {
    o.get_mut().color = CELL_BLACK
}

fn paint_white(o: CellPointer) {
    o.get_mut().color = CELL_WHITES;
}

fn is_grey(o: CellPointer) -> bool {
    o.get().color == CELL_GREY
}

fn paint_partial_white(s: &IncrementalCollector, o: CellPointer) {
    o.get_mut().color = s.current_white_part;
}

fn is_black(o: CellPointer) -> bool {
    (o.get().color & CELL_BLACK) != 0
}

fn is_white(o: CellPointer) -> bool {
    (o.get().color & CELL_WHITES) != 0
}

const DEFAULT_STEP_RATIO: usize = 200;
const DEFAULT_INTERVAL_RATIO: usize = 200;
const MAJOR_GC_INC_RATIO: usize = 120;
const MAJOR_GC_TOOMANY: usize = 10000;

use crate::heap::space::Space;
use std::collections::LinkedList;
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
enum GcState {
    Root,
    Mark,
    Sweep,
}

pub struct IncrementalCollector {
    current_white_part: u8,
    generational: bool,
    allocator: FreeListAllocator,
    remembered: std::collections::HashSet<usize, fxhash::FxBuildHasher>,
    full: bool,
    grey: LinkedList<CellPointer>,
    step_ratio: usize,
    state: GcState,
    roots: Vec<CellPointer>,
    live_after_mark: usize,
    live: usize,
    threshold: usize,
    interval_ratio: usize,
    major_old_threshold: usize,
    process: Option<Arc<Process>>,
}

impl IncrementalCollector {
    const fn other_white_part(&self) -> u8 {
        self.current_white_part ^ CELL_WHITES
    }

    fn flip_white(&mut self) {
        self.current_white_part ^= CELL_WHITES;
    }

    fn is_dead(&self, cell: CellPointer) -> bool {
        let cell_color = cell.get().color;
        (cell_color & self.other_white_part() & CELL_WHITES) != 0
    }

    fn incremental_step(&mut self) {
        let (limit, mut result) = ((1024 / 100) * self.step_ratio, 0);

        while result < limit {
            result += self.incremental_gc(limit);
            if self.state == GcState::Root {
                break;
            }
        }
    }

    fn incremental_gc_until(&mut self, state: GcState) {
        log::trace!("Incremental GC until {:?}", state);
        loop {
            log::trace!("Incremental GC until loop {:?}", self.state,);
            self.incremental_gc(std::usize::MAX);
            if self.state == state {
                break;
            }
        }
    }

    fn incremental_gc(&mut self, limit: usize) -> usize {
        match self.state {
            GcState::Root => {
                let proc = self.process.as_ref().unwrap().clone();
                self.trace_process(&proc);
                self.state = GcState::Mark;
                self.flip_white();
                return 0;
            }
            GcState::Mark => {
                if !self.grey.is_empty() {
                    return self.incremental_marking_phase(limit);
                }

                self.final_mark();
                self.state = GcState::Sweep;
                self.live_after_mark = self.live;
                return 0;
            }
            GcState::Sweep => {
                let tried_sweep;
                tried_sweep = self.incremental_sweep_phase(limit);
                if tried_sweep == 0 {
                    self.state = GcState::Root;
                }
                return tried_sweep;
            }
        }
    }

    fn incremental_sweep_phase(&mut self, limit: usize) -> usize {
        let mut tried_sweep = 0;
        /*let mut garbage_start = Address::null();
        let end = self.allocator.top;
        let mut scan = self.heap.start;
        while scan < end && tried_sweep < limit {
            let cell_ptr = scan.to_mut_ptr::<Cell>();
            let cell = CellPointer {
                raw: crate::util::tagged::TaggedPointer::new(cell_ptr),
            };

            if self.is_minor() && cell.get().generation >= 5 {
                self.add_freelist(garbage_start, Address::from_ptr(cell_ptr));
                scan = scan.offset(std::mem::size_of::<Cell>());
                continue;
            }
            if self.is_dead(cell) {
                if !garbage_start.is_non_null() {
                    garbage_start = Address::from_ptr(cell_ptr);
                }
                log::trace!("Sweep {:p}", cell_ptr);
                tried_sweep += 1;
            } else {
                self.add_freelist(garbage_start, Address::from_ptr(cell_ptr));
                if !self.generational {
                    paint_partial_white(self, cell);
                }
            }

            scan = scan.offset(std::mem::size_of::<Cell>());
        }*/
        let mut freelist = FreeList::new();
        macro_rules! add_freelist {
            ($start: expr,$end: expr) => {
                if $start.is_non_null() {
                    let size = $end.offset_from($start);
                    freelist.add($start, size);
                }
            };
        }
        for page in self.allocator.space.pages.iter() {
            let mut garbage_start = Address::null();
            let end = page.top;
            log::trace!(
                "Sweeping memory page from {:p} to {:p} (memory page limit is {:p})",
                page.data.to_ptr::<u8>(),
                page.top.to_ptr::<u8>(),
                page.limit.to_ptr::<u8>()
            );
            let mut scan = page.data;
            while scan < end && tried_sweep < limit {
                let cell_ptr = scan.to_mut_ptr::<Cell>();
                let cell = CellPointer {
                    raw: crate::util::tagged::TaggedPointer::new(cell_ptr),
                };

                if self.is_minor() && cell.get().generation >= 5 {
                    add_freelist!(garbage_start, Address::from_ptr(cell_ptr));
                    scan = scan.offset(std::mem::size_of::<Cell>());
                    continue;
                }
                if self.is_dead(cell) && cell.get().generation != 127 {
                    if !garbage_start.is_non_null() {
                        garbage_start = Address::from_ptr(cell_ptr);
                    }
                    log::trace!("Sweep {:p} '{}'", cell_ptr, cell);
                    unsafe {
                        std::ptr::drop_in_place(cell_ptr);
                    }
                    cell.get_mut().generation = 127;
                    tried_sweep += 1;
                } else {
                    add_freelist!(garbage_start, Address::from_ptr(cell_ptr));
                    if !self.generational {
                        paint_partial_white(self, cell);
                    }
                }

                scan = scan.offset(std::mem::size_of::<Cell>());
            }
            add_freelist!(garbage_start, end);
        }
        self.live_after_mark -= tried_sweep;
        self.live -= tried_sweep;
        tried_sweep
    }

    fn is_minor(&self) -> bool {
        self.generational && !self.full
    }

    fn is_major(&self) -> bool {
        self.generational && self.full
    }
    fn final_mark(&mut self) {
        while let Some(value) = self.grey.pop_front() {
            if is_grey(value) {
                paint_black(value);
                self.mark_children(value);
            }
        }
    }

    fn add_freelist(&mut self, start: Address, end: Address) {
        if start.is_null() {
            return;
        }

        let size = end.offset_from(start);
        self.allocator.freelist.add(start, size);
    }
    fn mark(&mut self, obj: CellPointer) {
        if !is_white(obj) {
            return;
        }
        log::trace!("Mark {:p}", obj.raw.raw);
        paint_grey(obj);
        self.grey.push_front(obj);
    }

    fn mark_children(&mut self, obj: CellPointer) -> usize {
        paint_black(obj);
        let mut children = 0;
        obj.get().trace(|ptr| {
            let ptr = unsafe { *ptr };
            self.mark(ptr);
            children += 1;
        });

        children
    }
    fn incremental_marking_phase(&mut self, limit: usize) -> usize {
        let mut tried_marks = 0;
        while !self.grey.is_empty() && tried_marks < limit {
            let value = self.grey.pop_front().unwrap();
            log::trace!("Incremental mark {:p}", value.raw.raw);
            tried_marks += self.mark_children(value);
        }
        tried_marks
    }
    fn clear_all_old(&mut self) {
        if self.is_major() {
            self.incremental_gc_until(GcState::Root);
        }
        let tmp = self.generational;
        self.generational = false;
        self.final_mark();
        self.state = GcState::Sweep;
        self.live_after_mark = self.live;
        self.incremental_gc_until(GcState::Root);
        self.generational = tmp;
    }

    fn major(&mut self) {
        log::trace!("Full GC triggered");
        if self.generational {
            self.clear_all_old();
            self.full = true;
        } else if self.state != GcState::Root {
            // finish half baked GC cycle
            self.incremental_gc_until(GcState::Root);
        }

        self.incremental_gc_until(GcState::Root);
        self.threshold = (self.live_after_mark / 100) * self.interval_ratio;

        if self.generational {
            self.major_old_threshold = self.live_after_mark / 100 * MAJOR_GC_INC_RATIO;
            self.full = false;
        }
        log::trace!("Full GC finished");
    }

    fn minor(&mut self) {
        log::trace!("Incremental GC triggered");
        if self.is_minor() {
            self.incremental_gc_until(GcState::Root);
        } else {
            self.incremental_step();
        }

        if self.state == GcState::Root {
            assert!(self.live >= self.live_after_mark);
            self.threshold = (self.live_after_mark / 100) * self.interval_ratio;
            if self.threshold < 1024 {
                self.threshold = 1024;
            }

            if self.is_major() {
                let threshold = self.live_after_mark / 100 * MAJOR_GC_TOOMANY;
                self.full = false;
                if threshold < MAJOR_GC_TOOMANY {
                    self.major_old_threshold = threshold;
                } else {
                    self.major();
                }
            } else if self.is_minor() {
                if self.live > self.major_old_threshold {
                    self.clear_all_old();
                    self.full = true;
                }
            }
        }
        log::trace!("Incremental GC finished");
    }

    pub fn new(generational: bool, size: usize) -> Self {
        Self {
            generational,
            grey: LinkedList::new(),
            roots: Vec::new(),
            allocator: FreeListAllocator::new(Space::new(size)),
            live: 0,
            live_after_mark: 0,
            full: generational,
            step_ratio: DEFAULT_STEP_RATIO,
            interval_ratio: DEFAULT_INTERVAL_RATIO,
            current_white_part: CELL_WHITE_A,
            remembered: std::collections::HashSet::with_hasher(fxhash::FxBuildHasher::default()),
            state: GcState::Root,
            threshold: 1024,
            major_old_threshold: 0,
            process: None,
        }
    }
}
use super::*;

impl HeapTrait for IncrementalCollector {
    fn trace_process(&mut self, proc: &Arc<crate::runtime::process::Process>) {
        if let None = self.process {
            self.process = Some(proc.clone());
        }
        assert!(proc.local_data().channel.try_lock().is_some());
        let channel = proc.local_data().channel.lock();
        channel.trace(|pointer| {
            proc.local_data_mut().heap.schedule(pointer as *mut _);
        });
        proc.trace(|pointer| {
            proc.local_data_mut()
                .heap
                .schedule(pointer as *mut CellPointer);
        });
    }
    fn copy_object(&mut self, object: Value) -> Value {
        if !object.is_cell() {
            return object;
        }

        let to_copy = object.as_cell();
        if to_copy.is_permanent() {
            return object;
        }
        let to_copy = to_copy.get();
        let value_copy = match &to_copy.value {
            CellValue::None => CellValue::None,
            CellValue::Duration(d) => CellValue::Duration(d.clone()),
            CellValue::File(_) => panic!("Cannot copy file"),
            CellValue::Number(x) => CellValue::Number(*x),
            CellValue::Bool(x) => CellValue::Bool(*x),
            CellValue::String(x) => CellValue::String(x.clone()),
            CellValue::Array(values) => {
                let new_values = values
                    .iter()
                    .map(|value| self.copy_object(*value))
                    .collect();
                CellValue::Array(new_values)
            }
            CellValue::Function(function) => {
                let name = function.name.clone();
                let argc = function.argc.clone();
                let module = function.module.clone();
                let upvalues = function
                    .upvalues
                    .iter()
                    .map(|x| self.copy_object(*x))
                    .collect();
                let native = function.native;
                let code = function.code.clone();
                CellValue::Function(Arc::new(Function {
                    name,
                    argc,
                    module,
                    upvalues,
                    native,
                    code,
                }))
            }
            CellValue::ByteArray(array) => CellValue::ByteArray(array.clone()),
            CellValue::Module(module) => CellValue::Module(module.clone()),
            CellValue::Process(proc) => CellValue::Process(proc.clone()),
        };
        let mut copy = if let Some(proto_ptr) = to_copy.prototype {
            let proto_copy = self.copy_object(Value::from(proto_ptr));
            Cell::with_prototype(value_copy, proto_copy.as_cell())
        } else {
            Cell::new(value_copy)
        };
        if let Some(map) = to_copy.attributes_map() {
            let mut map_copy = AttributesMap::with_capacity(map.len());
            for (key, val) in map.iter() {
                let key_copy = key.clone();
                let val = self.copy_object(*val);
                map_copy.insert(key_copy, val);
            }

            copy.set_attributes_map(map_copy);
        }

        Value::from(self.allocate(GCType::Young, copy))
    }
    fn schedule(&mut self, ptr: *mut CellPointer) {
        unsafe {
            self.roots.push(*ptr);
        }
    }
    fn allocate(&mut self, _: GCType, cell: Cell) -> CellPointer {
        if self.threshold < self.live {
            self.minor();
        }

        self.live += 1;
        let mut ptr = self
            .allocator
            .allocate(std::mem::size_of::<Cell>())
            .to_mut_ptr::<Cell>();
        if ptr.is_null() {
            self.major();
            ptr = self
                .allocator
                .allocate(std::mem::size_of::<Cell>())
                .to_mut_ptr::<Cell>();
            if ptr.is_null() {
                panic!("OOM");
            }
        }
        unsafe {
            ptr.write(cell);
        }
        let ptr = CellPointer {
            raw: crate::util::tagged::TaggedPointer::new(ptr),
        };
        paint_partial_white(self, ptr);

        ptr
    }

    fn should_collect(&self) -> bool {
        self.threshold < self.live
    }

    fn minor_collect(&mut self) {
        self.minor();
    }

    fn major_collect(&mut self) {
        self.major();
    }

    fn collect_garbage(&mut self) {
        self.major();
    }
    /// Field write barrier.
    ///   Paint obj(Black) -> value(White) to obj(Black) -> value(Gray).
    fn field_write_barrier(&mut self, parent: CellPointer, child: Value) {
        if !child.is_cell() {
            return;
        }
        let child = child.as_cell();
        if !is_black(parent) {
            return;
        }

        if !is_white(child) {
            return;
        }

        if self.generational || self.state == GcState::Mark {
            paint_grey(child);
            self.grey.push_front(child);
        } else {
            paint_partial_white(self, parent);
        }
    }

    fn write_barrier(&mut self, obj: CellPointer) {
        if !is_black(obj) {
            return;
        }

        paint_grey(obj);
        self.grey.push_front(obj);
    }

    fn set_proc(&mut self, proc: Arc<crate::runtime::process::Process>) {
        self.process = Some(proc);
    }
}
