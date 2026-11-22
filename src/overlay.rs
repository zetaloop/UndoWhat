use std::{
    env,
    ffi::c_void,
    io,
    path::PathBuf,
    ptr,
    sync::{
        Once,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use windows::Win32::{
    Foundation::{
        CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER, S_FALSE, S_OK,
    },
    System::Com::{IClassFactory, IClassFactory_Impl},
    UI::Shell::{IShellIconOverlayIdentifier, IShellIconOverlayIdentifier_Impl},
};
use windows_core::{
    BOOL, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement,
};
use windows_sys::Win32::System::{
    Diagnostics::Debug::OutputDebugStringW,
    LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
    },
};

const CLSID: GUID = GUID::from_u128(0xdf7344c6_372c_4b87_a289_d3c9cd0bfd2b);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static START: Once = Once::new();

struct Reference;

impl Reference {
    fn new() -> Self {
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for Reference {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Release);
    }
}

#[implement(IClassFactory)]
struct Factory(Reference);

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> Result<()> {
        if out.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { out.write(ptr::null_mut()) };
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let overlay: IShellIconOverlayIdentifier = Overlay(Reference::new()).into();
        unsafe { overlay.query(iid, out).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            ACTIVE.fetch_add(1, Ordering::Relaxed);
        } else {
            ACTIVE.fetch_sub(1, Ordering::Release);
        }
        Ok(())
    }
}

#[implement(IShellIconOverlayIdentifier)]
struct Overlay(Reference);

impl IShellIconOverlayIdentifier_Impl for Overlay_Impl {
    fn IsMemberOf(&self, _: &PCWSTR, _: u32) -> Result<()> {
        Err(S_FALSE.into())
    }

    fn GetOverlayInfo(&self, _: PWSTR, _: i32, _: *mut i32, _: *mut u32) -> Result<()> {
        START.call_once(|| {
            if is_explorer()
                && let Err(error) = start()
            {
                unsafe { OutputDebugStringW(crate::wide(&format!("UndoWhat: {error}")).as_ptr()) };
            }
        });
        Err(E_FAIL.into())
    }

    fn GetPriority(&self) -> Result<i32> {
        Ok(100)
    }
}

fn start() -> io::Result<()> {
    // The Shell releases the overlay object; the callbacks live until Explorer exits.
    let mut module = ptr::null_mut();
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            start as *const u16,
            &mut module,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    thread::Builder::new().spawn(crate::initialize)?;
    Ok(())
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() || clsid.is_null() || iid.is_null() {
        return E_POINTER;
    }
    unsafe { out.write(ptr::null_mut()) };
    if unsafe { *clsid } != CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory(Reference::new()).into();
    unsafe { factory.query(iid, out) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if ACTIVE.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

fn is_explorer() -> bool {
    let (Ok(host), Some(root)) = (env::current_exe(), env::var_os("SystemRoot")) else {
        return false;
    };
    host.as_os_str()
        .eq_ignore_ascii_case(PathBuf::from(root).join("explorer.exe").as_os_str())
}
