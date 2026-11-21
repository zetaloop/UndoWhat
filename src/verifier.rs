use std::{ffi::c_void, ptr};

pub const DLL_PROCESS_VERIFIER: u32 = 4;

#[repr(C)]
struct DllDescriptor {
    name: *mut u16,
    flags: u32,
    address: *mut c_void,
    thunks: *mut c_void,
}

#[repr(C)]
struct Provider {
    length: u32,
    dlls: *mut DllDescriptor,
    load: *mut c_void,
    unload: *mut c_void,
    image: *mut u16,
    flags: u32,
    debug: u32,
    stack_trace: *mut c_void,
    heap_create: *mut c_void,
    heap_destroy: *mut c_void,
    heap_free: *mut c_void,
}

static mut DLLS: DllDescriptor = DllDescriptor {
    name: ptr::null_mut(),
    flags: 0,
    address: ptr::null_mut(),
    thunks: ptr::null_mut(),
};

static mut PROVIDER: Provider = Provider {
    length: size_of::<Provider>() as u32,
    dlls: &raw mut DLLS,
    load: ptr::null_mut(),
    unload: ptr::null_mut(),
    image: ptr::null_mut(),
    flags: 0,
    debug: 0,
    stack_trace: ptr::null_mut(),
    heap_create: ptr::null_mut(),
    heap_destroy: ptr::null_mut(),
    heap_free: ptr::null_mut(),
};

pub unsafe fn describe(reserved: *mut c_void) -> i32 {
    let Some(slot) = (unsafe { reserved.cast::<*mut Provider>().as_mut() }) else {
        return 0;
    };
    *slot = &raw mut PROVIDER;
    1
}
