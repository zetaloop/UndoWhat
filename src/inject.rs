use std::{
    ffi::{OsStr, c_void},
    io, mem,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, FreeLibrary, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    System::{
        Diagnostics::{
            Debug::WriteProcessMemory,
            ToolHelp::{
                CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW,
                PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPMODULE,
                TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
            },
        },
        LibraryLoader::{
            DONT_RESOLVE_DLL_REFERENCES, GetModuleHandleW, GetProcAddress, LoadLibraryExW,
        },
        Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
        },
        Threading::{
            CreateRemoteThread, GetCurrentProcessId, GetExitCodeThread, INFINITE, OpenProcess,
            PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
            PROCESS_VM_WRITE, WaitForSingleObject,
        },
    },
};

use crate::abi::Symbols;

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct Module {
    name: String,
    path: PathBuf,
    base: usize,
    size: usize,
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

fn name(text: &[u16]) -> String {
    let length = text
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(text.len());
    String::from_utf16_lossy(&text[..length])
}

fn check(value: bool) -> io::Result<()> {
    if value {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn snapshot(flags: u32, pid: u32) -> io::Result<Handle> {
    let handle = unsafe { CreateToolhelp32Snapshot(flags, pid) };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(handle))
    }
}

fn processes() -> io::Result<Vec<u32>> {
    let list = snapshot(TH32CS_SNAPPROCESS, 0)?;
    let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut pids = Vec::new();
    let mut found = unsafe { Process32FirstW(list.0, &mut entry) } != 0;
    while found {
        if name(&entry.szExeFile).eq_ignore_ascii_case("explorer.exe") {
            pids.push(entry.th32ProcessID);
        }
        found = unsafe { Process32NextW(list.0, &mut entry) } != 0;
    }
    Ok(pids)
}

fn modules(pid: u32) -> io::Result<Vec<Module>> {
    let list = snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)?;
    let mut entry: MODULEENTRY32W = unsafe { mem::zeroed() };
    entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
    let mut found = unsafe { Module32FirstW(list.0, &mut entry) } != 0;
    let mut modules = Vec::new();
    while found {
        modules.push(Module {
            name: name(&entry.szModule),
            path: PathBuf::from(name(&entry.szExePath)),
            base: entry.modBaseAddr as usize,
            size: entry.modBaseSize as usize,
        });
        found = unsafe { Module32NextW(list.0, &mut entry) } != 0;
    }
    Ok(modules)
}

fn module(pid: u32, name: &str) -> io::Result<Option<Module>> {
    Ok(modules(pid)?
        .into_iter()
        .find(|item| item.name.eq_ignore_ascii_case(name)))
}

fn remote_function(pid: u32, library: &str, symbol: &[u8]) -> io::Result<usize> {
    let local = unsafe { GetModuleHandleW(wide(OsStr::new(library)).as_ptr()) };
    if local.is_null() {
        return Err(io::Error::last_os_error());
    }
    let function = unsafe { GetProcAddress(local, symbol.as_ptr()) }
        .ok_or_else(io::Error::last_os_error)? as usize;
    let current = modules(unsafe { GetCurrentProcessId() })?;
    let owner = current
        .iter()
        .find(|item| function >= item.base && function - item.base < item.size)
        .ok_or_else(|| io::Error::other("Cannot locate the module containing the function"))?;
    let remote = module(pid, &owner.name)?
        .ok_or_else(|| io::Error::other(format!("{} missing in process {pid}", owner.name)))?;
    Ok(remote.base + function - owner.base)
}

struct Allocation {
    process: HANDLE,
    pointer: *mut c_void,
}

impl Allocation {
    fn new(process: HANDLE, data: &[u8]) -> io::Result<Self> {
        let pointer = unsafe {
            VirtualAllocEx(
                process,
                ptr::null(),
                data.len(),
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if pointer.is_null() {
            return Err(io::Error::last_os_error());
        }
        let allocation = Self { process, pointer };
        check(
            unsafe {
                WriteProcessMemory(
                    process,
                    pointer,
                    data.as_ptr().cast(),
                    data.len(),
                    ptr::null_mut(),
                )
            } != 0,
        )?;
        Ok(allocation)
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe { VirtualFreeEx(self.process, self.pointer, 0, MEM_RELEASE) };
    }
}

fn execute(process: HANDLE, function: usize, parameter: *const c_void) -> io::Result<u32> {
    let start = unsafe {
        mem::transmute::<usize, Option<unsafe extern "system" fn(*mut c_void) -> u32>>(function)
    };
    let thread = unsafe {
        CreateRemoteThread(
            process,
            ptr::null(),
            0,
            start,
            parameter,
            0,
            ptr::null_mut(),
        )
    };
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    let thread = Handle(thread);
    check(unsafe { WaitForSingleObject(thread.0, INFINITE) } == WAIT_OBJECT_0)?;
    let mut exit_code = 0;
    check(unsafe { GetExitCodeThread(thread.0, &mut exit_code) } != 0)?;
    Ok(exit_code)
}

fn exported(dll: &Path, symbol: &[u8]) -> io::Result<usize> {
    let handle = unsafe {
        LoadLibraryExW(
            wide(dll.as_os_str()).as_ptr(),
            ptr::null_mut(),
            DONT_RESOLVE_DLL_REFERENCES,
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let function = unsafe { GetProcAddress(handle, symbol.as_ptr()) }
        .map(|value| value as usize - handle as usize)
        .ok_or_else(io::Error::last_os_error);
    unsafe { FreeLibrary(handle) };
    function
}

fn change(pid: u32, dll: &Path, symbols: Option<&Symbols>) -> io::Result<bool> {
    let process = unsafe {
        OpenProcess(
            PROCESS_CREATE_THREAD
                | PROCESS_QUERY_INFORMATION
                | PROCESS_VM_OPERATION
                | PROCESS_VM_WRITE,
            0,
            pid,
        )
    };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = Handle(process);
    let mut loaded = module(pid, "undowhat_hook.dll")?;
    let fresh = loaded.is_none();
    if symbols.is_none() && loaded.is_none() {
        return Ok(false);
    }
    if loaded.is_none() {
        let path = wide(dll.as_os_str());
        let data =
            unsafe { std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), path.len() * 2) };
        let allocation = Allocation::new(process.0, data)?;
        let load = remote_function(pid, "kernel32.dll", b"LoadLibraryW\0")?;
        let _ = execute(process.0, load, allocation.pointer)?;
        loaded = module(pid, "undowhat_hook.dll")?;
    }
    let loaded = loaded.ok_or_else(|| io::Error::other("Explorer could not load the hook"))?;
    let (export_name, parameter) = match symbols {
        Some(value) => (b"UndoWhatStart\0".as_slice(), {
            let data = unsafe {
                std::slice::from_raw_parts(
                    (value as *const Symbols).cast::<u8>(),
                    size_of::<Symbols>(),
                )
            };
            Some(Allocation::new(process.0, data)?)
        }),
        None => (b"UndoWhatStop\0".as_slice(), None),
    };
    let result = execute(
        process.0,
        loaded.base + exported(&loaded.path, export_name)?,
        parameter
            .as_ref()
            .map_or(ptr::null(), |value| value.pointer),
    )?;
    if result != 0 {
        if fresh && let Ok(free) = remote_function(pid, "kernel32.dll", b"FreeLibrary\0") {
            let _ = execute(process.0, free, loaded.base as *const c_void);
        }
        return Err(io::Error::other(format!("UndoWhat hook returned {result}")));
    }
    if symbols.is_none() {
        let free = remote_function(pid, "kernel32.dll", b"FreeLibrary\0")?;
        check(execute(process.0, free, loaded.base as *const c_void)? != 0)?;
    }
    Ok(true)
}

pub fn run(symbols: Option<&Symbols>) -> io::Result<()> {
    let dll = std::env::current_exe()?.with_file_name("undowhat_hook.dll");
    if symbols.is_some() && !dll.is_file() {
        return Err(io::Error::other(format!("Missing {}", dll.display())));
    }
    let pids = processes()?;
    let mut changed = 0;
    for pid in pids {
        match change(pid, &dll, symbols) {
            Ok(true) => {
                changed += 1;
                println!(
                    "Explorer {pid}: {}",
                    if symbols.is_some() {
                        "started"
                    } else {
                        "stopped"
                    }
                );
            }
            Ok(false) => (),
            Err(error) => eprintln!("Explorer {pid}: {error}"),
        }
    }
    if changed == 0 {
        Err(io::Error::other("No Explorer process was updated"))
    } else {
        Ok(())
    }
}
