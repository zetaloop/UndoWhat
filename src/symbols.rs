use crate::{abi::Symbols, download};

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

impl Symbols {
    pub fn entries(&self) -> impl Iterator<Item = (String, usize)> {
        let values = [
            self.file_do,
            self.root_source,
            self.root_destination,
            self.root_op,
            self.src_parent,
            self.dst_parent,
            self.property_info,
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
    fs::create_dir_all(&cache)?;
    let system = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Windows"))
        .join("System32");
    let path = if let Ok(path) = env::var("_NT_SYMBOL_PATH") {
        path
    } else {
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
