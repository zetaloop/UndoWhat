mod abi;
mod capture;
mod notify;

use std::{
    cell::RefCell,
    ffi::c_void,
    mem,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

use minhook::MinHook;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};

#[link(name = "runtimeobject")]
unsafe extern "system" {
    fn RoInitialize(init: u32) -> i32;
    fn RoUninitialize();
}

use crate::{abi::Symbols, capture::Functions, notify::Record};

type Do = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();
static ORIGINAL_DO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_UNDO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REDO: AtomicUsize = AtomicUsize::new(0);
static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

thread_local! {
    static OPERATION: RefCell<Option<Vec<Record>>> = const { RefCell::new(None) };
}

struct Runtime {
    sender: mpsc::Sender<Record>,
    notifier: JoinHandle<()>,
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
    let previous = OPERATION.with(|state| state.replace(Some(Vec::new())));
    let result = unsafe { original(manager, unit) };
    let records = OPERATION.with(|state| state.replace(previous));

    if result >= 0
        && let Some(records) = records
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
            if let Ok(runtime) = RUNTIME.lock()
                && let Some(runtime) = runtime.as_ref()
            {
                let _ = runtime.sender.send(first);
            }
        }
    }
    result
}

unsafe extern "system" fn file_do(unit: *mut c_void, manager: *mut c_void) -> i32 {
    let original: Do = unsafe { mem::transmute(ORIGINAL_DO.load(Ordering::Acquire)) };
    let record = if OPERATION.with(|state| state.borrow().is_some()) {
        FUNCTIONS
            .get()
            .and_then(|functions| unsafe { functions.snapshot(unit) })
    } else {
        None
    };
    let result = unsafe { original(unit, manager) };
    if result >= 0
        && let Some(record) = record
    {
        OPERATION.with(|state| {
            if let Some(records) = state.borrow_mut().as_mut() {
                records.push(record);
            }
        });
    }
    result
}

fn start(symbols: Symbols) -> u32 {
    let Ok(mut runtime) = RUNTIME.lock() else {
        return 1;
    };
    if runtime.is_some() {
        return 0;
    }

    let storage = module("Windows.Storage.dll");
    let shell = module("shell32.dll");
    if storage == 0 || shell == 0 {
        return 2;
    }
    let _ = FUNCTIONS.set(unsafe { Functions::new(storage, symbols) });

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
        Ok::<_, minhook::MH_STATUS>(())
    })();
    if install.is_err() {
        MinHook::uninitialize();
        return 4;
    }

    let (sender, receiver) = mpsc::channel();
    let notifier = thread::spawn(move || {
        let initialized = unsafe { RoInitialize(1) } >= 0;
        for record in receiver {
            let _ = notify::show(record);
        }
        if initialized {
            unsafe { RoUninitialize() };
        }
    });
    *runtime = Some(Runtime { sender, notifier });

    if unsafe { MinHook::enable_all_hooks() }.is_err() {
        let running = runtime.take();
        drop(runtime);
        if let Some(running) = running {
            drop(running.sender);
            let _ = running.notifier.join();
        }
        MinHook::uninitialize();
        return 5;
    }
    0
}

/// # Safety
/// The caller must pass a valid pointer to a Symbols value in the current process.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn UndoWhatStart(symbols: *const Symbols) -> u32 {
    if symbols.is_null() {
        return 87;
    }
    start(unsafe { *symbols })
}

#[unsafe(no_mangle)]
pub extern "system" fn UndoWhatStop(_: *mut c_void) -> u32 {
    let Ok(mut runtime) = RUNTIME.lock() else {
        return 1;
    };
    if runtime.is_none() {
        return 0;
    }
    if unsafe { MinHook::disable_all_hooks() }.is_err() {
        return 2;
    }
    let running = runtime.take();
    drop(runtime);
    if let Some(running) = running {
        drop(running.sender);
        let _ = running.notifier.join();
    }
    MinHook::uninitialize();
    0
}
