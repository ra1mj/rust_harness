//! Reference plugin for the `rh-plugins` dylib runtime.
//!
//! Exports the three required ABI symbols and advertises one `echo` tool.
//! Build with `cargo build -p echo-plugin`; the resulting `libecho_plugin.so`
//! (or `.dylib` / `.dll`) is dropped into `plugins/` for `rh web` to load.

use std::ffi::{c_char, CStr, CString};

fn to_c_string(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new(String::new()).unwrap())
        .into_raw()
}

/// Advertised tools as a JSON array of `{ name, description, schema }`.
///
/// # Safety
/// The returned pointer must be released exactly once with [`rh_plugin_free`].
#[no_mangle]
pub unsafe extern "C" fn rh_plugin_tools() -> *mut c_char {
    to_c_string(
        serde_json::json!([{
            "name": "echo",
            "description": "Echo the `text` argument back (plugin demo tool).",
            "schema": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }
        }])
        .to_string(),
    )
}

/// Invoke one tool; returns a JSON object `{ "content": ... }`.
///
/// # Safety
/// `name` and `args_json` must be valid, NUL-terminated C strings. The
/// returned pointer must be released exactly once with [`rh_plugin_free`].
#[no_mangle]
pub unsafe extern "C" fn rh_plugin_call(name: *const c_char, args_json: *const c_char) -> *mut c_char {
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    let args: serde_json::Value = unsafe { CStr::from_ptr(args_json) }
        .to_string_lossy()
        .parse()
        .unwrap_or(serde_json::json!({}));

    let result = match name.as_str() {
        "echo" => serde_json::json!({
            "content": args.get("text").and_then(|v| v.as_str()).unwrap_or("")
        }),
        other => serde_json::json!({ "is_error": true, "error": format!("unknown tool: {other}") }),
    };
    to_c_string(result.to_string())
}

/// Free a string previously returned by this plugin.
///
/// # Safety
/// `ptr` must be a pointer previously returned by this plugin (via
/// [`rh_plugin_tools`] or [`rh_plugin_call`]) that has not been freed yet.
#[no_mangle]
pub unsafe extern "C" fn rh_plugin_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` came from `to_c_string` and is handed back exactly once.
    unsafe { drop(CString::from_raw(ptr)) };
}
