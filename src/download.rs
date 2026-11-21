use std::{
    ffi::c_void,
    fs, io,
    path::{Path, PathBuf},
};

use windows_sys::Win32::System::Diagnostics::Debug::{SYMSRV_INDEX_INFOW, SymSrvGetFileIndexInfoW};

#[link(name = "urlmon")]
unsafe extern "system" {
    fn URLDownloadToFileW(
        caller: *mut c_void,
        url: *const u16,
        filename: *const u16,
        reserved: u32,
        callback: *mut c_void,
    ) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn string(buffer: &[u16]) -> String {
    String::from_utf16_lossy(
        &buffer[..buffer
            .iter()
            .position(|&value| value == 0)
            .unwrap_or(buffer.len())],
    )
}

pub fn fetch(
    dll: &Path,
    cache: &Path,
    preparation: &mut crate::symbols::Preparation,
) -> io::Result<PathBuf> {
    let mut info: SYMSRV_INDEX_INFOW = unsafe { std::mem::zeroed() };
    info.sizeofstruct = size_of::<SYMSRV_INDEX_INFOW>() as u32;
    if unsafe { SymSrvGetFileIndexInfoW(wide(&dll.to_string_lossy()).as_ptr(), &mut info, 0) } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let pdb = string(&info.pdbfile);
    let name = Path::new(&pdb)
        .file_name()
        .ok_or_else(|| io::Error::other("Missing PDB name"))?;
    let name = name.to_string_lossy();
    let guid = info.guid;
    let mut key = format!("{:08X}{:04X}{:04X}", guid.data1, guid.data2, guid.data3);
    for byte in guid.data4 {
        key.push_str(&format!("{byte:02X}"));
    }
    key.push_str(&format!("{:X}", info.age));

    let folder = cache.join(name.as_ref()).join(&key);
    let destination = folder.join(name.as_ref());
    if destination.is_file() {
        return Ok(folder);
    }
    *preparation = crate::symbols::Preparation::Download(name.to_string());
    fs::create_dir_all(&folder)?;
    let partial = destination.with_extension("part");
    let url = format!("https://msdl.microsoft.com/download/symbols/{name}/{key}/{name}");
    println!("Downloading {name}...");
    let status = unsafe {
        URLDownloadToFileW(
            std::ptr::null_mut(),
            wide(&url).as_ptr(),
            wide(&partial.to_string_lossy()).as_ptr(),
            0,
            std::ptr::null_mut(),
        )
    };
    if status < 0 {
        let _ = fs::remove_file(&partial);
        return Err(io::Error::other(format!("HRESULT {status:#010x}")));
    }
    fs::rename(partial, destination)?;
    *preparation = crate::symbols::Preparation::Resolve;
    Ok(folder)
}
