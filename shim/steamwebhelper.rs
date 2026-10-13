//! Shim installed in place of `steamwebhelper.exe` (see src/steam.rs).
//!
//! Relaunches the real binary, renamed `steamwebhelper_real.exe`, with
//! the flags CrossOver adds (`--no-sandbox --in-process-gpu --disable-gpu`)
//! ahead of the arguments given by Steam.
//!
//! It is a Windows exe compiled by build.rs, without std or CRT: it talks
//! directly to kernel32, and rust-lld is enough to link it.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::ptr::{addr_of_mut, null, null_mut};

type Handle = *mut c_void;

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    cb_reserved2: u16,
    reserved2: *mut u8,
    std_input: Handle,
    std_output: Handle,
    std_error: Handle,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

// `raw-dylib`: rustc generates the import libraries itself,
// no need for the Windows SDK.
#[link(name = "kernel32", kind = "raw-dylib")]
extern "system" {
    fn GetCommandLineW() -> *const u16;
    fn GetModuleFileNameW(module: Handle, filename: *mut u16, size: u32) -> u32;
    fn CreateProcessW(
        app: *const u16,
        cmdline: *mut u16,
        process_attrs: *mut c_void,
        thread_attrs: *mut c_void,
        inherit_handles: i32,
        flags: u32,
        env: *mut c_void,
        cwd: *const u16,
        startup: *mut StartupInfoW,
        info: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, ms: u32) -> u32;
    fn GetExitCodeProcess(handle: Handle, code: *mut u32) -> i32;
    fn ExitProcess(code: u32) -> !;
}

// libcore (and LLVM) call memcpy, wcslen… which the CRT usually provides.
// ntdll.dll exports them: we import them from there.
#[link(name = "ntdll", kind = "raw-dylib")]
extern "C" {
    fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void;
    fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32;
    fn wcslen(s: *const u16) -> usize;
}

/// Forces the import of the functions above even if our code doesn't mention them.
#[allow(dead_code)]
struct Keep([*const c_void; 5]);
unsafe impl Sync for Keep {}
#[used]
static KEEP: Keep = Keep([
    memcpy as *const c_void,
    memmove as *const c_void,
    memset as *const c_void,
    memcmp as *const c_void,
    wcslen as *const c_void,
]);

// Symbols expected by libcore on this target, never used with panic=abort.
#[no_mangle]
pub static _fltused: i32 = 0;
#[no_mangle]
pub extern "C" fn __CxxFrameHandler3() {}

const REAL_BINARY: &str = "steamwebhelper_real.exe";
/// CrossOver's (hack 13322 of its compatibility database): Chromium's
/// compositor runs in the process that owns the window.
const EXTRA_FLAGS: &str = "--no-sandbox --in-process-gpu --disable-gpu";

/// Fixed-size UTF-16 string, zero-terminated on demand.
struct WideBuf<const N: usize> {
    data: [u16; N],
    len: usize,
}

impl<const N: usize> WideBuf<N> {
    const fn new() -> Self {
        WideBuf { data: [0; N], len: 0 }
    }

    fn push(&mut self, c: u16) {
        // Keep one slot for the terminating zero.
        if self.len + 1 >= N {
            unsafe { ExitProcess(2) }
        }
        self.data[self.len] = c;
        self.len += 1;
    }

    fn push_ascii(&mut self, s: &str) {
        s.bytes().for_each(|b| self.push(b as u16));
    }

    fn push_wide(&mut self, s: &[u16]) {
        s.iter().for_each(|&c| self.push(c));
    }

    fn as_slice(&self) -> &[u16] {
        &self.data[..self.len]
    }

    fn as_mut_ptr(&mut self) -> *mut u16 {
        self.data[self.len] = 0;
        self.data.as_mut_ptr()
    }
}

// Static rather than on the stack: a large local array would force
// LLVM to call __chkstk, yet another CRT function.
static mut SELF_PATH: WideBuf<1024> = WideBuf::new();
static mut REAL_PATH: WideBuf<1024> = WideBuf::new();
static mut CMDLINE: WideBuf<32768> = WideBuf::new();

/// Removes the first argument (the shim's path) from a Windows command line.
fn skip_argv0(cmd: &[u16]) -> &[u16] {
    let is_blank = |c: u16| c == b' ' as u16 || c == b'\t' as u16;
    let mut i = 0;
    let mut quoted = false;
    while i < cmd.len() && (quoted || !is_blank(cmd[i])) {
        if cmd[i] == b'"' as u16 {
            quoted = !quoted;
        }
        i += 1;
    }
    while i < cmd.len() && is_blank(cmd[i]) {
        i += 1;
    }
    &cmd[i..]
}

#[no_mangle]
pub extern "system" fn shim_main() -> ! {
    unsafe {
        let self_path = &mut *addr_of_mut!(SELF_PATH);
        let real = &mut *addr_of_mut!(REAL_PATH);
        let cmdline = &mut *addr_of_mut!(CMDLINE);

        let n = GetModuleFileNameW(null_mut(), self_path.data.as_mut_ptr(), 1024) as usize;
        if n == 0 || n >= 1024 {
            ExitProcess(3);
        }
        self_path.len = n;

        // <shim folder>\steamwebhelper_real.exe
        let Some(slash) = self_path.as_slice().iter().rposition(|&c| c == b'\\' as u16) else {
            ExitProcess(4);
        };
        real.push_wide(&self_path.as_slice()[..=slash]);
        real.push_ascii(REAL_BINARY);

        // "<real>" <EXTRA_FLAGS> <original arguments>
        let raw = GetCommandLineW();
        let original = core::slice::from_raw_parts(raw, wcslen(raw));
        cmdline.push(b'"' as u16);
        cmdline.push_wide(real.as_slice());
        cmdline.push_ascii("\" ");
        cmdline.push_ascii(EXTRA_FLAGS);
        cmdline.push(b' ' as u16);
        cmdline.push_wide(skip_argv0(original));

        let mut startup: StartupInfoW = core::mem::zeroed();
        startup.cb = core::mem::size_of::<StartupInfoW>() as u32;
        let mut info: ProcessInformation = core::mem::zeroed();
        let ok = CreateProcessW(
            real.as_mut_ptr(),
            cmdline.as_mut_ptr(),
            null_mut(),
            null_mut(),
            1, // inherits the handles passed by Steam
            0,
            null_mut(),
            null(),
            &mut startup,
            &mut info,
        );
        if ok == 0 {
            ExitProcess(5);
        }

        // Steam watches this process: wait for the real webhelper and
        // return its exit code.
        WaitForSingleObject(info.process, u32::MAX);
        let mut code = 0;
        GetExitCodeProcess(info.process, &mut code);
        ExitProcess(code)
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { ExitProcess(101) }
}
