use crate::download;

use std::{env, ffi::c_void, fs, io, path::PathBuf, ptr};

use windows_sys::Win32::System::Diagnostics::Debug::{
    SYMBOL_INFOW, SYMOPT_UNDNAME, SymCleanup, SymFromNameW, SymGetOptions, SymInitializeW,
    SymLoadModuleExW, SymSetOptions,
};

const NAMES: [(&str, &str); 9] = [
    ("Windows.Storage", "CFileOperationUndoUnit::Do"),
    ("Windows.Storage", "CUndoRoot::GetSourceItem"),
    ("Windows.Storage", "CUndoRoot::GetDestinationItem"),
    ("Windows.Storage", "CUndoRoot::GetOp"),
    ("Windows.Storage", "CUndoRoot::GetSrcParent"),
    ("Windows.Storage", "CUndoRoot::GetDstParent"),
    ("Windows.Storage", "CApplyPropertiesUndoUnit::SetUndoInfo"),
    ("shell32", "CUndoManager::UndoTo"),
    ("shell32", "CUndoManager::RedoTo"),
];

pub struct Symbols {
    pub file_do: usize,
    pub root_source: usize,
    pub root_destination: usize,
    pub root_op: usize,
    pub src_parent: usize,
    pub dst_parent: usize,
    pub property_info: usize,
    pub undo_to: usize,
    pub redo_to: usize,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

struct Session(*mut c_void, u32);

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            SymCleanup(self.0);
            SymSetOptions(self.1);
        }
    }
}

pub enum Preparation {
    Cached,
    Download(String),
    Resolve,
    Activate,
}

pub fn resolve(preparation: &mut Preparation) -> io::Result<Symbols> {
    let cache = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("UndoWhat")
        .join("symbols");
    let system = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Windows"))
        .join("System32");
    let path = if let Ok(path) = env::var("_NT_SYMBOL_PATH") {
        path
    } else {
        fs::create_dir_all(&cache)?;
        // Explorer processes share the same PDB downloads.
        let lock = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache.join(".lock"))?;
        lock.lock()?;
        let mut paths = Vec::new();
        for module in ["Windows.Storage", "shell32"] {
            let folder =
                download::fetch(&system.join(format!("{module}.dll")), &cache, preparation)?;
            paths.push(folder.to_string_lossy().into_owned());
        }
        paths.join(";")
    };

    let path = wide(&path);
    let mut resolved = [0usize; NAMES.len()];
    for module in ["Windows.Storage", "shell32"] {
        // The two on-disk images have the same preferred base.
        let mut token = 0u8;
        let session = (&raw mut token).cast();
        if unsafe { SymInitializeW(session, path.as_ptr(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let options = unsafe { SymGetOptions() };
        let _session = Session(session, options);
        unsafe { SymSetOptions(options | SYMOPT_UNDNAME) };
        let dll = system.join(format!("{module}.dll"));
        let dll_wide = wide(&dll.to_string_lossy());
        let base = unsafe {
            SymLoadModuleExW(
                session,
                ptr::null_mut(),
                dll_wide.as_ptr(),
                wide(module).as_ptr(),
                0,
                0,
                ptr::null_mut(),
                0,
            )
        };
        if base == 0 {
            return Err(io::Error::other(format!(
                "{module}: {}",
                io::Error::last_os_error()
            )));
        }
        for (index, (_, name)) in NAMES.iter().enumerate().filter(|(_, (m, _))| *m == module) {
            let mut buffer = [0u64; 256];
            let info = buffer.as_mut_ptr().cast::<SYMBOL_INFOW>();
            unsafe {
                (*info).SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
                (*info).MaxNameLen =
                    ((size_of_val(&buffer) - size_of::<SYMBOL_INFOW>()) / size_of::<u16>()) as u32;
            }
            let qualified = wide(&format!("{module}!{name}"));
            if unsafe { SymFromNameW(session, qualified.as_ptr(), info) } == 0 {
                return Err(io::Error::other(format!(
                    "{module}!{name}: {}",
                    io::Error::last_os_error()
                )));
            }
            let address = unsafe { (*info).Address };
            resolved[index] = usize::try_from(address - base).unwrap();
        }
    }
    let [
        file_do,
        root_source,
        root_destination,
        root_op,
        src_parent,
        dst_parent,
        property_info,
        undo_to,
        redo_to,
    ] = resolved;
    Ok(Symbols {
        file_do,
        root_source,
        root_destination,
        root_op,
        src_parent,
        dst_parent,
        property_info,
        undo_to,
        redo_to,
    })
}
