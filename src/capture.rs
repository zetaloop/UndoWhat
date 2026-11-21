use std::{ffi::c_void, mem, ptr};

use windows_sys::Win32::System::Com::CoTaskMemFree;

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SysFreeString(bstr: *mut u16);
}

use crate::{abi::Symbols, notify::Record};

type Item = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
type ChildCount = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
type Child = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
type ShellItem = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;

#[derive(Clone, Copy)]
pub struct Functions {
    source: Item,
    destination: Item,
    count: ChildCount,
    child: Child,
    item: ShellItem,
}

impl Functions {
    pub unsafe fn new(base: usize, symbols: Symbols) -> Self {
        Self {
            source: unsafe { mem::transmute::<usize, Item>(base + symbols.root_source) },
            destination: unsafe { mem::transmute::<usize, Item>(base + symbols.root_destination) },
            count: unsafe { mem::transmute::<usize, ChildCount>(base + symbols.node_count) },
            child: unsafe { mem::transmute::<usize, Child>(base + symbols.node_child) },
            item: unsafe { mem::transmute::<usize, ShellItem>(base + symbols.node_item) },
        }
    }

    pub unsafe fn snapshot(&self, unit: *mut c_void) -> Option<Record> {
        if unit.is_null() {
            return None;
        }

        let vtable = unsafe { *(unit as *const *const usize) };
        type GetDescription = unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32;
        let describe: GetDescription = unsafe { mem::transmute(*vtable.add(4)) };
        let mut description_ptr: *mut u16 = ptr::null_mut();
        let description =
            if unsafe { describe(unit, &mut description_ptr) } >= 0 && !description_ptr.is_null() {
                let value = unsafe { utf16(description_ptr) };
                unsafe { SysFreeString(description_ptr) };
                value
            } else {
                "撤销文件操作".to_owned()
            };

        let dpa = unsafe { *((unit as *const u8).add(0x20) as *const *const u8) };
        if dpa.is_null() {
            return None;
        }
        let count = unsafe { *(dpa as *const i32) };
        if count <= 0 {
            return None;
        }
        let roots = unsafe { *(dpa.add(8) as *const *const *mut c_void) };
        if roots.is_null() {
            return None;
        }
        let mut lines = Vec::new();
        for index in 0..count as usize {
            let root = unsafe { *roots.add(index) };
            if root.is_null() {
                continue;
            }
            let source = unsafe { get_item(self.source, root) };
            let destination = unsafe { get_item(self.destination, root) };
            match (source.as_deref(), destination.as_deref()) {
                (Some(from), Some(to)) => lines.push(format!("{from} → {to}")),
                (Some(from), None) => lines.push(from.to_owned()),
                (None, Some(to)) => lines.push(to.to_owned()),
                (None, None) => (),
            }

            // CUndoRoot's IOperationNode interface is at +0x10.
            unsafe { self.children(root.byte_add(0x10), &mut lines) };
        }
        Some(Record { description, lines })
    }

    unsafe fn children(&self, parent: *mut c_void, lines: &mut Vec<String>) {
        let mut count = 0;
        if unsafe { (self.count)(parent, &mut count) } < 0 {
            return;
        }
        for index in 0..count {
            let mut child = ptr::null_mut();
            if unsafe { (self.child)(parent, index, &mut child) } < 0 || child.is_null() {
                continue;
            }
            let mut item = ptr::null_mut();
            if unsafe { (self.item)(child, ptr::null_mut(), &mut item) } >= 0
                && let Some(path) = unsafe { item_path(item) }
            {
                lines.push(path);
            }
            unsafe { self.children(child, lines) };
            unsafe { release(child) };
        }
    }
}

unsafe fn get_item(method: Item, object: *mut c_void) -> Option<String> {
    let mut item = ptr::null_mut();
    if unsafe { method(object, &mut item) } < 0 {
        return None;
    }
    unsafe { item_path(item) }
}

unsafe fn item_path(item: *mut c_void) -> Option<String> {
    if item.is_null() {
        return None;
    }
    type GetDisplayName = unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16) -> i32;
    let vtable = unsafe { *(item as *const *const usize) };
    let get_display_name: GetDisplayName = unsafe { mem::transmute(*vtable.add(5)) };
    let mut name = ptr::null_mut();
    let mut result = unsafe { get_display_name(item, 0x80058000, &mut name) };
    if result < 0 {
        result = unsafe { get_display_name(item, 0x80028000, &mut name) };
    }
    let path = if result >= 0 && !name.is_null() {
        let path = unsafe { utf16(name) };
        unsafe { CoTaskMemFree(name.cast()) };
        Some(path)
    } else {
        None
    };
    unsafe { release(item) };
    path
}

unsafe fn release(object: *mut c_void) {
    if !object.is_null() {
        type Release = unsafe extern "system" fn(*mut c_void) -> u32;
        let vtable = unsafe { *(object as *const *const usize) };
        let release: Release = unsafe { mem::transmute(*vtable.add(2)) };
        unsafe { release(object) };
    }
}

unsafe fn utf16(ptr: *const u16) -> String {
    let mut len = 0;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}
