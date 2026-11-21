use std::{
    ffi::c_void,
    mem,
    path::{Path, PathBuf},
    ptr,
};

use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        UI::Shell::{
            IShellItem, IShellItem2,
            PropertiesSystem::{IPropertyDescription, PSGetPropertyDescription},
            SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
        },
    },
    core::{GUID, Interface},
};
use windows_sys::Win32::System::Com::CoTaskMemFree;

use crate::{abi::Symbols, notify::Record};

type Item = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
type GetOp = unsafe extern "system" fn(*mut c_void, bool, *mut u32) -> i32;
type Name = unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32;

#[derive(Clone, Copy)]
pub struct Functions {
    op: GetOp,
    source: Item,
    destination: Item,
    src_parent: Item,
    dst_parent: Item,
}

impl Functions {
    pub unsafe fn new(base: usize, symbols: Symbols) -> Self {
        Self {
            op: unsafe { mem::transmute::<usize, GetOp>(base + symbols.root_op) },
            source: unsafe { mem::transmute::<usize, Item>(base + symbols.root_source) },
            destination: unsafe { mem::transmute::<usize, Item>(base + symbols.root_destination) },
            src_parent: unsafe { mem::transmute::<usize, Item>(base + symbols.src_parent) },
            dst_parent: unsafe { mem::transmute::<usize, Item>(base + symbols.dst_parent) },
        }
    }

    pub unsafe fn snapshot(&self, unit: *mut c_void, redo: bool) -> Option<Record> {
        if unit.is_null() {
            return None;
        }

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
        let mut kind = None;
        for index in 0..count as usize {
            let root = unsafe { *roots.add(index) };
            if root.is_null() {
                continue;
            }
            let mut op = 0;
            if unsafe { (self.op)(root, false, &mut op) } < 0 {
                op = 0;
            }
            kind = Some(match kind {
                Some(previous) if previous != op => 0,
                _ => op,
            });
            let parents = unsafe {
                [
                    get_item(self.src_parent, root),
                    get_item(self.dst_parent, root),
                ]
            };
            let items = unsafe {
                [
                    get_item(self.source, root),
                    get_item(self.destination, root),
                ]
            };
            let names = unsafe { [get_name(root, 13), get_name(root, 14)] };
            let [source, destination] = [0, 1].map(|i| {
                let parent = parents[i].as_deref().map(Path::new).or_else(|| {
                    (op == 4)
                        .then(|| items.iter().flatten().find_map(|s| Path::new(s).parent()))
                        .flatten()
                });
                let name = names[i].as_deref().map(Path::new);
                parent
                    .zip(name)
                    .map(|(p, n)| p.join(n))
                    .or_else(|| items[i].as_ref().map(PathBuf::from))
                    .or_else(|| name.map(PathBuf::from))
                    .filter(|path| !recycled(path))
                    .or_else(|| {
                        items[i]
                            .as_ref()
                            .map(PathBuf::from)
                            .filter(|p| !recycled(p))
                    })
                    .map(|path| path.to_string_lossy().into_owned())
            });
            if let Some(line) = change(op, redo, source, destination) {
                lines.push(line);
            }
        }
        let description = match kind? {
            1 => "移动",
            2 => "复制",
            3 => "删除",
            4 => "重命名",
            5 => "创建链接",
            6 => "属性更改",
            7 => "新建",
            8 => "删除云文件",
            _ => "文件操作",
        };
        Some(Record {
            description: description.to_owned(),
            count: count as usize,
            lines,
        })
    }
}

fn change(
    op: u32,
    redo: bool,
    source: Option<String>,
    destination: Option<String>,
) -> Option<String> {
    match op {
        1 | 4 => {
            let (from, to) = if redo {
                (source, destination)
            } else {
                (destination, source)
            };
            match (from, to) {
                (Some(a), Some(b)) if a != b => Some(format!("{a} → {b}")),
                (a, b) => a.or(b),
            }
        }
        2 if redo => match (source, destination) {
            (Some(a), Some(b)) => Some(format!("{a} → {b}")),
            (_, Some(b)) => Some(format!("+ {b}")),
            _ => None,
        },
        2 | 5 | 7 => destination
            .or(source.filter(|_| op == 7))
            .map(|p| format!("{} {p}", if redo { "+" } else { "−" })),
        3 => source
            .or(destination)
            .map(|p| format!("{} {p}", if redo { "−" } else { "+" })),
        _ => source.or(destination),
    }
}

unsafe fn get_name(root: *mut c_void, slot: usize) -> Option<String> {
    let table = unsafe { *root.cast::<*const usize>() };
    let method: Name = unsafe { mem::transmute(*table.add(slot)) };
    let mut text = ptr::null_mut();
    let result = unsafe { method(root, &mut text) };
    unsafe { take_string(result, text) }
}

unsafe fn take_string(result: i32, text: *mut u16) -> Option<String> {
    if text.is_null() {
        return None;
    }
    let value = (result >= 0)
        .then(|| unsafe { utf16(text) })
        .filter(|s| !s.is_empty());
    unsafe { CoTaskMemFree(text.cast()) };
    value
}

#[link(name = "propsys")]
unsafe extern "system" {
    fn PSFormatForDisplayAlloc(
        key: *const c_void,
        value: *const c_void,
        flags: u32,
        text: *mut *mut u16,
    ) -> i32;
}

pub unsafe fn property(
    item: *mut c_void,
    key: *const c_void,
    from: *const c_void,
    to: *const c_void,
) -> Record {
    let mut lines: Vec<_> = unsafe { item_path(item) }.into_iter().collect();
    let mut description = ptr::null_mut();
    if unsafe { PSGetPropertyDescription(key.cast(), &IPropertyDescription::IID, &mut description) }
        .is_ok()
        && let Ok(name) = unsafe { IPropertyDescription::from_raw(description).GetDisplayName() }
        && let Some(name) = unsafe { take_string(0, name.0) }
    {
        let [a, b] = [from, to].map(|value| {
            if unsafe { *value.cast::<u16>() } == 0 {
                return Some("无".to_owned());
            }
            let mut text = ptr::null_mut();
            let hr = unsafe { PSFormatForDisplayAlloc(key, value, 0, &mut text) };
            unsafe { take_string(hr, text) }
        });
        lines.push(match (a, b) {
            (Some(a), Some(b)) => format!("{name}「{a}」→「{b}」"),
            _ => name,
        });
    }
    Record {
        description: "属性更改".to_owned(),
        count: 1,
        lines,
    }
}

unsafe fn get_item(method: Item, object: *mut c_void) -> Option<String> {
    let mut item = ptr::null_mut();
    if unsafe { method(object, &mut item) } < 0 {
        return None;
    }
    if item.is_null() {
        return None;
    }
    let owned = unsafe { IShellItem::from_raw(item) };
    unsafe { item_path(owned.as_raw()) }
}

fn recycled(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("$Recycle.Bin"))
}

unsafe fn item_path(item: *mut c_void) -> Option<String> {
    let item = unsafe { IShellItem::from_raw_borrowed(&item) }?;
    let name = unsafe {
        item.GetDisplayName(SIGDN_FILESYSPATH)
            .or_else(|_| item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING))
    }
    .ok()?;
    let path = unsafe { take_string(0, name.0) }?;
    if !recycled(Path::new(&path)) {
        return Some(path);
    }
    let item: IShellItem2 = item.cast().ok()?;
    let string = |guid, pid| {
        let value = unsafe {
            item.GetString(&PROPERTYKEY {
                fmtid: GUID::from_u128(guid),
                pid,
            })
        }
        .ok()?;
        unsafe { take_string(0, value.0) }
    };
    let parent = string(0x9b174b33_40ff_11d2_a27e_00c04fc30871, 2)?;
    let name = string(0x6b8da074_3b5c_43bc_886f_0a2cdce00b6f, 100)?;
    Some(Path::new(&parent).join(name).to_string_lossy().into_owned())
}

unsafe fn utf16(ptr: *const u16) -> String {
    let mut len = 0;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}
