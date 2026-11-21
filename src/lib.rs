mod capture;
mod download;
mod notify;
mod symbols;
mod verifier;

use std::{
    cell::RefCell,
    ffi::c_void,
    io, mem, ptr,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

use minhook::MinHook;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HMODULE},
    System::{
        Diagnostics::Debug::OutputDebugStringW,
        LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
            GetModuleHandleExW, GetModuleHandleW, LoadLibraryW,
        },
        SystemServices::DLL_PROCESS_ATTACH,
        Threading::CreateThread,
    },
};

use crate::{
    capture::{Functions, Record},
    symbols::{Preparation, Symbols},
};

type Do = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();
static ORIGINAL_DO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_UNDO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REDO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_PROPERTY: AtomicUsize = AtomicUsize::new(0);
static RECORDS: OnceLock<mpsc::Sender<Record>> = OnceLock::new();

thread_local! {
    static OPERATION: RefCell<Option<(bool, Vec<Record>)>> = const { RefCell::new(None) };
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn module(name: &str) -> usize {
    let name = wide(name);
    let loaded = unsafe { GetModuleHandleW(name.as_ptr()) };
    if !loaded.is_null() {
        loaded as usize
    } else {
        unsafe { LoadLibraryW(name.as_ptr()) as usize }
    }
}

unsafe extern "system" fn undo(manager: *mut c_void, unit: *mut c_void) -> i32 {
    unsafe { run(&ORIGINAL_UNDO, "撤销", manager, unit) }
}

unsafe extern "system" fn redo(manager: *mut c_void, unit: *mut c_void) -> i32 {
    unsafe { run(&ORIGINAL_REDO, "重做", manager, unit) }
}

unsafe fn run(target: &AtomicUsize, action: &str, manager: *mut c_void, unit: *mut c_void) -> i32 {
    let original: Do = unsafe { mem::transmute(target.load(Ordering::Acquire)) };
    let previous = OPERATION.with(|state| state.replace(Some((action == "重做", Vec::new()))));
    let result = unsafe { original(manager, unit) };
    let records = OPERATION.with(|state| state.replace(previous));

    if result >= 0
        && let Some((_, records)) = records
    {
        let mut records = records.into_iter();
        if let Some(mut first) = records.next() {
            for record in records {
                if record.description != first.description {
                    first.description = "文件操作".to_owned();
                }
                first.count += record.count;
                first.lines.extend(record.lines);
            }
            first.description.insert_str(0, action);
            if first.count > 1 {
                first.description.push_str(&format!(" {} 项", first.count));
            }
            if let Some(sender) = RECORDS.get() {
                let _ = sender.send(first);
            }
        }
    }
    result
}

unsafe extern "system" fn file_do(unit: *mut c_void, manager: *mut c_void) -> i32 {
    let original: Do = unsafe { mem::transmute(ORIGINAL_DO.load(Ordering::Acquire)) };
    let record = if let Some(redo) = OPERATION.with(|state| state.borrow().as_ref().map(|s| s.0)) {
        FUNCTIONS
            .get()
            .and_then(|functions| unsafe { functions.snapshot(unit, redo) })
    } else {
        None
    };
    let result = unsafe { original(unit, manager) };
    if result >= 0
        && let Some(record) = record
    {
        OPERATION.with(|state| {
            if let Some((_, records)) = state.borrow_mut().as_mut() {
                records.push(record);
            }
        });
    }
    result
}

type Property = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const c_void,
    *const c_void,
    *const c_void,
    i32,
) -> i32;

unsafe extern "system" fn property(
    unit: *mut c_void,
    item: *mut c_void,
    key: *const c_void,
    first: *const c_void,
    second: *const c_void,
    inverse: i32,
) -> i32 {
    let original: Property = unsafe { mem::transmute(ORIGINAL_PROPERTY.load(Ordering::Acquire)) };
    let hr = unsafe { original(unit, item, key, first, second, inverse) };
    if hr >= 0 && OPERATION.with(|state| state.borrow().is_some()) {
        // Do builds the opposite unit: its target value is this operation's starting value.
        let (from, to) = if inverse != 0 {
            (first, second)
        } else {
            (second, first)
        };
        let record = unsafe { capture::property(item, key, from, to) };
        OPERATION.with(|state| {
            if let Some((_, records)) = state.borrow_mut().as_mut() {
                records.push(record);
            }
        });
    }
    hr
}

fn install(symbols: Symbols) -> io::Result<mpsc::Receiver<Record>> {
    let storage = module("Windows.Storage.dll");
    let shell = module("shell32.dll");
    if storage == 0 || shell == 0 {
        return Err(io::Error::last_os_error());
    }
    let _ = FUNCTIONS.set(unsafe { Functions::new(storage, &symbols) });

    let file_target = (storage + symbols.file_do) as *mut c_void;
    let undo_target = (shell + symbols.undo_to) as *mut c_void;
    let install = (|| {
        let original = unsafe { MinHook::create_hook(file_target, file_do as *mut c_void) }?;
        ORIGINAL_DO.store(original as usize, Ordering::Release);
        let original = unsafe { MinHook::create_hook(undo_target, undo as *mut c_void) }?;
        ORIGINAL_UNDO.store(original as usize, Ordering::Release);
        let redo_target = (shell + symbols.redo_to) as *mut c_void;
        let original = unsafe { MinHook::create_hook(redo_target, redo as *mut c_void) }?;
        ORIGINAL_REDO.store(original as usize, Ordering::Release);
        let target = (storage + symbols.property_info) as *mut c_void;
        let original = unsafe { MinHook::create_hook(target, property as *mut c_void) }?;
        ORIGINAL_PROPERTY.store(original as usize, Ordering::Release);
        Ok::<_, minhook::MH_STATUS>(())
    })();
    if let Err(error) = install {
        MinHook::uninitialize();
        return Err(io::Error::other(format!("MinHook: {error:?}")));
    }

    let (sender, receiver) = mpsc::channel::<Record>();
    let _ = RECORDS.set(sender);
    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        MinHook::uninitialize();
        return Err(io::Error::other(format!("MinHook: {error:?}")));
    }
    Ok(receiver)
}

unsafe extern "system" fn initialize(_: *mut c_void) -> u32 {
    notify::with_apartment(|| {
        let mut phase = Preparation::Cached;
        let result = symbols::resolve(&mut phase).and_then(|symbols| {
            if matches!(phase, Preparation::Resolve) {
                phase = Preparation::Activate;
            }
            install(symbols)
        });
        let notice = match (&phase, &result) {
            (Preparation::Cached, _) => None,
            (_, Ok(_)) => Some(("UndoWhat 已就绪", String::new())),
            (Preparation::Download(file), Err(e)) => {
                Some(("UndoWhat 符号下载失败", format!("{file}\n{e}")))
            }
            (Preparation::Resolve, Err(e)) => Some(("UndoWhat 符号解析失败", e.to_string())),
            (Preparation::Activate, Err(e)) => Some(("UndoWhat 启动失败", e.to_string())),
        };
        if let Some((title, body)) = notice {
            let _ = notify::show(title, &body);
        }
        match result {
            Ok(receiver) => {
                for record in receiver {
                    let _ = notify::show(&record.description, &record.lines.join("\n"));
                }
            }
            Err(error) => unsafe {
                OutputDebugStringW(wide(&format!("UndoWhat: {error}")).as_ptr())
            },
        }
    });
    0
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, reserved: *mut c_void) -> i32 {
    if reason == verifier::DLL_PROCESS_VERIFIER {
        return unsafe { verifier::describe(reserved) };
    }
    if reason == DLL_PROCESS_ATTACH {
        // The worker and active hooks live until the host process exits.
        let mut pinned = ptr::null_mut();
        if unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                module.cast(),
                &mut pinned,
            )
        } == 0
        {
            return 0;
        }
        let worker = unsafe {
            CreateThread(
                ptr::null(),
                0,
                Some(initialize),
                ptr::null(),
                0,
                ptr::null_mut(),
            )
        };
        if worker.is_null() {
            return 0;
        }
        unsafe { CloseHandle(worker) };
    }
    1
}
