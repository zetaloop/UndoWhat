use std::{env, ffi::c_void, fs, io, path::PathBuf, ptr};

use windows_sys::Win32::System::{
    Diagnostics::Debug::{
        SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS, SYMOPT_FAIL_CRITICAL_ERRORS, SYMOPT_UNDNAME,
        SymCleanup, SymFromNameW, SymInitializeW, SymLoadModuleExW, SymSetOptions,
    },
    LibraryLoader::LoadLibraryW,
    Threading::GetCurrentProcess,
};

const NAMES: [(&str, &str); 9] = [
    ("Windows.Storage", "CFileOperationUndoUnit::Do"),
    ("Windows.Storage", "CUndoRoot::GetSourceItem"),
    ("Windows.Storage", "CUndoRoot::GetDestinationItem"),
    ("Windows.Storage", "CUndoRoot::GetOp"),
    ("Windows.Storage", "CUndoNode::GetChildCount"),
    ("Windows.Storage", "CUndoNode::GetChild"),
    ("Windows.Storage", "CUndoNode::GetShellItem"),
    ("shell32", "CUndoManager::UndoTo"),
    ("shell32", "CUndoManager::RedoTo"),
];

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Symbols {
    pub file_do: usize,
    pub root_source: usize,
    pub root_destination: usize,
    pub root_op: usize,
    pub node_count: usize,
    pub node_child: usize,
    pub node_item: usize,
    pub undo_to: usize,
    pub redo_to: usize,
}

impl Symbols {
    pub fn entries(&self) -> impl Iterator<Item = (String, usize)> {
        let values = [
            self.file_do,
            self.root_source,
            self.root_destination,
            self.root_op,
            self.node_count,
            self.node_child,
            self.node_item,
            self.undo_to,
            self.redo_to,
        ];
        NAMES
            .into_iter()
            .zip(values)
            .map(|((module, name), rva)| (format!("{module}!{name}"), rva))
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

struct Session(*mut c_void);

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { SymCleanup(self.0) };
    }
}

pub fn resolve() -> io::Result<Symbols> {
    let cache = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("UndoWhat")
        .join("symbols");
    fs::create_dir_all(&cache)?;
    let mut path = env::var("_NT_SYMBOL_PATH").unwrap_or_default();
    if !path.is_empty() {
        path.push(';');
    }
    path.push_str(&format!(
        "srv*{}*https://msdl.microsoft.com/download/symbols",
        cache.display()
    ));

    unsafe {
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_FAIL_CRITICAL_ERRORS);
    }
    let process = unsafe { GetCurrentProcess() };
    if unsafe { SymInitializeW(process, wide(&path).as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _session = Session(process);

    let system = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Windows"))
        .join("System32");

    let mut bases = [0u64; 2];
    for (index, name) in ["Windows.Storage", "shell32"].into_iter().enumerate() {
        let dll = system.join(format!("{name}.dll"));
        let dll_wide = wide(&dll.to_string_lossy());
        let handle = unsafe { LoadLibraryW(dll_wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::other(format!(
                "{}: {}",
                dll.display(),
                io::Error::last_os_error()
            )));
        }
        let address = unsafe {
            SymLoadModuleExW(
                process,
                ptr::null_mut(),
                dll_wide.as_ptr(),
                wide(name).as_ptr(),
                handle as u64,
                0,
                ptr::null_mut(),
                0,
            )
        };
        if address == 0 {
            return Err(io::Error::other(format!(
                "{name}: {}",
                io::Error::last_os_error()
            )));
        }
        bases[index] = address;
    }

    let mut resolved = [0usize; NAMES.len()];
    for (index, (module, name)) in NAMES.into_iter().enumerate() {
        let mut buffer = [0u64; 256];
        let info = buffer.as_mut_ptr().cast::<SYMBOL_INFOW>();
        unsafe {
            (*info).SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
            (*info).MaxNameLen = 1024;
        }
        let qualified = wide(&format!("{module}!{name}"));
        if unsafe { SymFromNameW(process, qualified.as_ptr(), info) } == 0 {
            return Err(io::Error::other(format!(
                "{module}!{name}: {}",
                io::Error::last_os_error()
            )));
        }
        let base = if module == "shell32" {
            bases[1]
        } else {
            bases[0]
        };
        let address = unsafe { (*info).Address };
        resolved[index] = usize::try_from(address - base).unwrap();
    }
    let [
        file_do,
        root_source,
        root_destination,
        root_op,
        node_count,
        node_child,
        node_item,
        undo_to,
        redo_to,
    ] = resolved;
    Ok(Symbols {
        file_do,
        root_source,
        root_destination,
        root_op,
        node_count,
        node_child,
        node_item,
        undo_to,
        redo_to,
    })
}
