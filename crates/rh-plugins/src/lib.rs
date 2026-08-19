//! rh-plugins — a dynamic plugin runtime over native dynamic libraries.
//!
//! Plugins are `cdylib`s exporting a small C ABI:
//!
//! ```c
//! // JSON array of tool descriptors: [{"name","description","schema"}]
//! extern "C" char* rh_plugin_tools(void);
//! // JSON result: {"content": "..."} or {"is_error": true, "error": "..."}
//! extern "C" char* rh_plugin_call(const char* name, const char* args_json);
//! // Free a string returned by the two functions above.
//! extern "C" void rh_plugin_free(char* ptr);
//! ```
//!
//! [`load_dir`] scans a directory for `*.so` / `*.dylib` / `*.dll` files,
//! loads each, and bridges every advertised tool into the harness's
//! [`ToolRegistry`]. See `plugins/echo-plugin` for a reference plugin.

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use libloading::Library;
use serde_json::{json, Value};

use rh_core::Disposer;
use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId, ToolRegistry};

type CallFn = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);
type ToolsFn = unsafe extern "C" fn() -> *mut c_char;

/// One tool exposed by a loaded plugin.
pub struct PluginTool {
    name: String,
    description: String,
    schema: Value,
    call: CallFn,
    free: FreeFn,
    // Keep the dynamic library loaded for the tool's lifetime.
    _lib: Arc<Library>,
}

#[async_trait]
impl Tool for PluginTool {
    fn id(&self) -> ToolId {
        self.name.clone()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(self.name.clone(), self.description.clone(), self.schema.clone())
    }

    async fn run(&self, _ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let name = self.name.clone();
        let args_json = args.to_string();
        let call = self.call;
        let free = self.free;
        // Run the (potentially slow) plugin call on the blocking pool so the
        // async runtime is never stalled by arbitrary plugin code.
        let raw = tokio::task::spawn_blocking(move || call_plugin(call, free, &name, &args_json))
            .await
            .map_err(|e| ToolError::execution(e.to_string()))?
            .map_err(ToolError::execution)?;
        serde_json::from_str(&raw).map_err(|e| ToolError::execution(e.to_string()))
    }
}

fn call_plugin(call: CallFn, free: FreeFn, name: &str, args_json: &str) -> Result<String, String> {
    let name_c = CString::new(name).map_err(|e| e.to_string())?;
    let args_c = CString::new(args_json).map_err(|e| e.to_string())?;
    // SAFETY: the plugin promises valid NUL-terminated `name`/`args_json`,
    // returns a `CString` it allocated, and `free` releases that allocation.
    let ptr = unsafe { call(name_c.as_ptr(), args_c.as_ptr()) };
    if ptr.is_null() {
        return Err("插件返回空指针".to_string());
    }
    let result = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { free(ptr) };
    Ok(result)
}

/// Read a plugin-allocated string and free it.
fn plugin_string(ptr: *mut c_char, free: FreeFn) -> Result<String> {
    if ptr.is_null() {
        return Err(anyhow!("插件返回空指针"));
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { free(ptr) };
    Ok(s)
}

/// Load one dynamic library and bridge its tools into `registry`.
pub fn load_plugin(registry: &ToolRegistry, path: &Path) -> Result<Vec<Disposer>> {
    // SAFETY: loading a shared library runs its constructors.
    let lib = unsafe { Library::new(path) }
        .map_err(|e| anyhow!("加载插件 {} 失败：{e}", path.display()))?;
    let lib = Arc::new(lib);

    // SAFETY: the symbols must match the documented ABI.
    let tools_sym: libloading::Symbol<ToolsFn> = unsafe { lib.get(b"rh_plugin_tools") }
        .map_err(|e| anyhow!("插件 {} 缺少 rh_plugin_tools：{e}", path.display()))?;
    let tools_fn: ToolsFn = *tools_sym;
    let call_sym: libloading::Symbol<CallFn> = unsafe { lib.get(b"rh_plugin_call") }
        .map_err(|e| anyhow!("插件 {} 缺少 rh_plugin_call：{e}", path.display()))?;
    let call_fn: CallFn = *call_sym;
    let free_sym: libloading::Symbol<FreeFn> = unsafe { lib.get(b"rh_plugin_free") }
        .map_err(|e| anyhow!("插件 {} 缺少 rh_plugin_free：{e}", path.display()))?;
    let free_fn: FreeFn = *free_sym;

    let tools_json = plugin_string(unsafe { tools_fn() }, free_fn)?;
    let descriptors: Vec<Value> = serde_json::from_str(&tools_json)?;

    let mut disposers = Vec::new();
    for desc in descriptors {
        let name = match desc.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let description = desc
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let schema = desc
            .get("schema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        let tool: Arc<dyn Tool> = Arc::new(PluginTool {
            name,
            description,
            schema,
            call: call_fn,
            free: free_fn,
            _lib: Arc::clone(&lib),
        });
        disposers.push(registry.register(tool));
    }
    Ok(disposers)
}

/// Load every dynamic library in `dir` (non-recursively) and bridge their
/// tools. Returns the disposers that keep the registrations alive.
pub fn load_dir(registry: &ToolRegistry, dir: &Path) -> Result<Vec<Disposer>> {
    let mut disposers = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(disposers); // a missing directory is fine
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_library(p))
        .collect();
    paths.sort();
    for path in paths {
        match load_plugin(registry, &path) {
            Ok(mut d) => disposers.append(&mut d),
            Err(e) => eprintln!("[rh-plugins] {e}"),
        }
    }
    Ok(disposers)
}

fn is_library(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(ext, "so" | "dylib" | "dll")
}
