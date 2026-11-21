//! `fxc.exe` for machines without the Windows SDK.
//!
//! gpui_windows' release build precompiles its HLSL with the SDK's fxc
//! (`build.rs`: `fxc /T <target> /E <entry> /Fh <out.h> /Vn <name> /O3
//! <file>`) and reads only the `const BYTE <name>[] = { … };` array out of
//! the header. fxc is a thin front end over `D3DCompile` in
//! `d3dcompiler_47.dll`, which every Windows 10/11 has in System32; this
//! does the same for that one command line, so a release build needs no
//! Visual Studio or SDK. `make dist` builds it and points `GPUI_FXC_PATH`
//! at it when no real fxc is found. No dependencies: the DLL is loaded at
//! runtime and its blob interface is called through its vtable.

use std::ffi::{c_void, CString};
use std::process::ExitCode;

type Hresult = i32;
type D3DCompile = unsafe extern "system" fn(
    src: *const c_void,
    len: usize,
    name: *const u8,
    defines: *const c_void,
    include: *mut c_void,
    entry: *const u8,
    target: *const u8,
    flags1: u32,
    flags2: u32,
    code: *mut *mut Blob,
    errors: *mut *mut Blob,
) -> Hresult;

/// `ID3DBlob`: IUnknown's three methods, then GetBufferPointer/Size.
#[repr(C)]
struct Blob {
    vtbl: *const BlobVtbl,
}
#[repr(C)]
struct BlobVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut Blob) -> u32,
    pointer: unsafe extern "system" fn(*mut Blob) -> *const u8,
    size: unsafe extern "system" fn(*mut Blob) -> usize,
}

impl Blob {
    unsafe fn bytes<'a>(this: *mut Blob) -> &'a [u8] {
        let v = &*(*this).vtbl;
        std::slice::from_raw_parts((v.pointer)(this), (v.size)(this))
    }
    unsafe fn release(this: *mut Blob) {
        ((*(*this).vtbl).release)(this);
    }
}

extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

const D3DCOMPILE_OPTIMIZATION_LEVEL3: u32 = 1 << 15;
/// `D3D_COMPILE_STANDARD_FILE_INCLUDE`: resolve `#include` next to the file.
const STANDARD_INCLUDE: *mut c_void = 1 as *mut c_void;

struct Args {
    target: String,
    entry: String,
    header: String,
    var: String,
    optimize: bool,
    file: String,
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut target, mut entry, mut header, mut var, mut file) = (None, None, None, None, None);
    let mut optimize = false;
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().ok_or(format!("{what} needs a value"));
        match arg.to_ascii_uppercase().as_str() {
            "/T" | "-T" => target = Some(value("/T")?),
            "/E" | "-E" => entry = Some(value("/E")?),
            "/FH" | "-FH" => header = Some(value("/Fh")?),
            "/VN" | "-VN" => var = Some(value("/Vn")?),
            "/O3" | "-O3" => optimize = true,
            "/NOLOGO" | "-NOLOGO" => {}
            _ if arg.starts_with('/') || arg.starts_with('-') => {
                return Err(format!("unsupported option {arg} (this is a minimal fxc)"))
            }
            _ => file = Some(arg),
        }
    }
    Ok(Args {
        target: target.ok_or("missing /T")?,
        entry: entry.ok_or("missing /E")?,
        header: header.ok_or("missing /Fh")?,
        var: var.unwrap_or_else(|| "g_main".into()),
        optimize,
        file: file.ok_or("missing the shader file")?,
    })
}

/// The header fxc writes for `/Fh`, as far as anyone reads it.
fn header(var: &str, code: &[u8]) -> String {
    let mut out = format!("const BYTE {var}[] =\n{{\n");
    for row in code.chunks(16) {
        let line: Vec<String> = row.iter().map(|b| format!("{b:4}")).collect();
        out.push_str(&line.join(","));
        out.push_str(",\n");
    }
    out.push_str("};\n");
    out
}

fn compile(args: &Args) -> Result<Vec<u8>, String> {
    let source = std::fs::read(&args.file).map_err(|e| format!("{}: {e}", args.file))?;
    let dll: Vec<u16> = "d3dcompiler_47.dll\0".encode_utf16().collect();
    let compile: D3DCompile = unsafe {
        let module = LoadLibraryW(dll.as_ptr());
        if module.is_null() {
            return Err("d3dcompiler_47.dll not found".into());
        }
        let f = GetProcAddress(module, b"D3DCompile\0".as_ptr());
        if f.is_null() {
            return Err("d3dcompiler_47.dll has no D3DCompile".into());
        }
        std::mem::transmute::<*mut c_void, D3DCompile>(f)
    };
    let c = |s: &str| CString::new(s).map_err(|_| format!("NUL in {s:?}"));
    let (name, entry, target) = (c(&args.file)?, c(&args.entry)?, c(&args.target)?);
    let flags = if args.optimize { D3DCOMPILE_OPTIMIZATION_LEVEL3 } else { 0 };
    let (mut code, mut errors) = (std::ptr::null_mut(), std::ptr::null_mut());
    let hr = unsafe {
        compile(
            source.as_ptr().cast(),
            source.len(),
            name.as_ptr().cast(),
            std::ptr::null(),
            STANDARD_INCLUDE,
            entry.as_ptr().cast(),
            target.as_ptr().cast(),
            flags,
            0,
            &mut code,
            &mut errors,
        )
    };
    let messages = (!errors.is_null()).then(|| unsafe {
        let text = String::from_utf8_lossy(Blob::bytes(errors)).into_owned();
        Blob::release(errors);
        text
    });
    if hr < 0 || code.is_null() {
        return Err(messages.unwrap_or_else(|| format!("D3DCompile failed: 0x{hr:08X}")));
    }
    if let Some(warnings) = messages.filter(|m| !m.trim().is_empty()) {
        eprint!("{warnings}");
    }
    let bytes = unsafe {
        let bytes = Blob::bytes(code).to_vec();
        Blob::release(code);
        bytes
    };
    Ok(bytes)
}

fn main() -> ExitCode {
    let result = parse(std::env::args().skip(1)).and_then(|args| {
        let code = compile(&args)?;
        std::fs::write(&args.header, header(&args.var, &code))
            .map_err(|e| format!("{}: {e}", args.header))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fxc: {e}");
            ExitCode::FAILURE
        }
    }
}
