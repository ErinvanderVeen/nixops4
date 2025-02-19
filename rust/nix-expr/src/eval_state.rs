use crate::value::{Value, ValueType};
use anyhow::Context as _;
use anyhow::{bail, Result};
use lazy_static::lazy_static;
use nix_c_raw as raw;
use nix_store::store::Store;
use nix_util::context::Context;
use std::ffi::CString;
use std::ptr::null_mut;
use std::ptr::NonNull;

lazy_static! {
    static ref INIT: Result<()> = {
        unsafe {
            raw::GC_allow_register_threads();
        }
        let context: Context = Context::new();
        unsafe {
            raw::nix_libexpr_init(context.ptr());
        }
        context.check_err()?;
        Ok(())
    };
}
pub fn init() -> Result<()> {
    let x = INIT.as_ref();
    match x {
        Ok(_) => Ok(()),
        Err(e) => {
            // Couldn't just clone the error, so we have to print it here.
            Err(anyhow::format_err!("nix_libstore_init error: {}", e))
        }
    }
}

pub struct EvalState {
    eval_state: NonNull<raw::EvalState>,
    store: Store,
    context: Context,
}
impl EvalState {
    pub fn new(store: Store) -> Result<Self> {
        let context = Context::new();

        init()?;

        let eval_state = unsafe {
            raw::nix_state_create(
                context.ptr(),
                /* searchPath */ null_mut(),
                store.raw_ptr(),
            )
        };
        if eval_state.is_null() {
            bail!("nix_state_create returned a null pointer");
        }
        Ok(EvalState {
            eval_state: NonNull::new(eval_state).unwrap(),
            store,
            context,
        })
    }
    pub fn raw_ptr(&self) -> *mut raw::EvalState {
        self.eval_state.as_ptr()
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn eval_from_string(&self, expr: String, path: String) -> Result<Value> {
        let expr_ptr =
            CString::new(expr).with_context(|| "eval_from_string: expr contains null byte")?;
        let path_ptr =
            CString::new(path).with_context(|| "eval_from_string: path contains null byte")?;
        let value = self.new_value_uninitialized();
        unsafe {
            let ctx_ptr = self.context.ptr();
            raw::nix_expr_eval_from_string(
                ctx_ptr,
                self.raw_ptr(),
                expr_ptr.as_ptr(),
                path_ptr.as_ptr(),
                value.raw_ptr(),
            );
        };
        self.context.check_err()?;
        Ok(value)
    }
    /** Try turn any Value into a Value that isn't a Thunk. */
    pub fn force(&self, v: &Value) -> Result<()> {
        unsafe {
            raw::nix_value_force(self.context.ptr(), self.raw_ptr(), v.raw_ptr());
        }
        self.context.check_err()
    }
    pub fn value_is_thunk(&self, value: &Value) -> bool {
        let r = unsafe {
            raw::nix_get_type(self.context.ptr(), value.raw_ptr()) == raw::ValueType_NIX_TYPE_THUNK
        };
        self.context.check_err().unwrap();
        r
    }
    pub fn value_type(&self, value: &Value) -> Result<ValueType> {
        if self.value_is_thunk(value) {
            self.force(value)?;
        }
        let r = unsafe { raw::nix_get_type(self.context.ptr(), value.raw_ptr()) };
        Ok(ValueType::from_raw(r))
    }
    /// Not exposed, because the caller must always explicitly handle the context or not accept one at all.
    fn get_string(&self, value: &Value) -> Result<String> {
        let c_str_raw = unsafe { raw::nix_get_string(self.context.ptr(), value.raw_ptr()) };
        self.context.check_err()?;
        let cstring = unsafe { std::ffi::CStr::from_ptr(c_str_raw) };
        let str = cstring
            .to_str()
            .map_err(|e| anyhow::format_err!("Nix string is not valid UTF-8: {}", e))?;
        Ok(str.to_owned())
    }
    /// NOTE: this will be replaced by two methods, one that also returns the context, and one that checks that the context is empty
    pub fn require_string(&self, value: &Value) -> Result<String> {
        let t = self.value_type(value)?;
        if t != ValueType::String {
            bail!("expected a string, but got a {:?}", t);
        }
        self.get_string(value)
    }

    fn new_value_uninitialized(&self) -> Value {
        let value = unsafe { raw::nix_alloc_value(self.context.ptr(), self.raw_ptr()) };
        Value::new(value)
    }
}

pub fn gc_now() {
    unsafe {
        raw::nix_gc_now();
    }
}

/** Run a function while making sure that the current thread is registered with the GC. */
pub fn gc_registering_current_thread<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> R,
{
    init()?;
    if unsafe { raw::GC_thread_is_registered() } != 0 {
        return Ok(f());
    } else {
        gc_register_my_thread().unwrap();
        let r = f();
        unsafe {
            raw::GC_unregister_my_thread();
        }
        return Ok(r);
    }
}

pub fn gc_register_my_thread() -> Result<()> {
    unsafe {
        let already_done = raw::GC_thread_is_registered();
        if already_done != 0 {
            return Ok(());
        }
        let mut sb: raw::GC_stack_base = raw::GC_stack_base {
            mem_base: 0 as *mut _,
        };
        let r = raw::GC_get_stack_base(&mut sb);
        if r as u32 != raw::GC_SUCCESS {
            Err(anyhow::format_err!("GC_get_stack_base failed: {}", r))?;
        }
        raw::GC_register_my_thread(&sb);
        Ok(())
    }
}

impl Drop for EvalState {
    fn drop(&mut self) {
        unsafe {
            raw::nix_state_free(self.raw_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use ctor::ctor;

    use super::*;

    #[ctor]
    fn setup() {
        init().unwrap();
    }

    #[test]
    fn eval_state_new_and_drop() {
        gc_registering_current_thread(|| {
            // very basic test: make sure initialization doesn't crash
            let store = Store::open("auto").unwrap();
            let _e = EvalState::new(store).unwrap();
        })
        .unwrap();
    }

    #[test]
    fn eval_state_eval_from_string() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("1".to_string(), "<test>".to_string())
                .unwrap();
            let v2 = v.clone();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::Int);
            let t2 = es.value_type(&v2).unwrap();
            assert!(t2 == ValueType::Int);
            gc_now();
        })
        .unwrap();
    }

    #[test]
    fn eval_state_value_bool() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("true".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::Bool);
        })
        .unwrap();
    }

    #[test]
    fn eval_state_value_string() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("\"hello\"".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::String);
            let s = es.require_string(&v).unwrap();
            assert!(s == "hello");
        })
        .unwrap();
    }

    #[test]
    fn eval_state_value_string_unexpected_bool() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("true".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let r = es.require_string(&v);
            assert!(r.is_err());
            // TODO: safe print value (like Nix would)
            assert_eq!(
                r.unwrap_err().to_string(),
                "expected a string, but got a Bool"
            );
        })
        .unwrap()
    }

    #[test]
    fn eval_state_value_string_unexpected_path_value() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("/foo".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let r = es.require_string(&v);
            assert!(r.is_err());
            assert_eq!(
                r.unwrap_err().to_string(),
                "expected a string, but got a Path"
            );
        })
        .unwrap()
    }

    #[test]
    fn eval_state_value_string_bad_utf() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string(
                    "builtins.substring 0 1 \"ü\"".to_string(),
                    "<test>".to_string(),
                )
                .unwrap();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::String);
            let r = es.require_string(&v);
            assert!(r.is_err());
            assert!(r
                .unwrap_err()
                .to_string()
                .contains("Nix string is not valid UTF-8"));
        })
        .unwrap();
    }

    #[test]
    fn eval_state_value_string_unexpected_context() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("(derivation { name = \"hello\"; system = \"dummy\"; builder = \"cmd.exe\"; }).outPath".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::String);
            // TODO
            // let r = es.require_string_without_context(&v);
            // assert!(r.is_err());
            // assert!(r.unwrap_err().to_string().contains("unexpected context"));
        })
        .unwrap();
    }

    #[test]
    fn eval_state_value_attrset() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("{ }".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::AttrSet);
        })
        .unwrap();
    }

    #[test]
    fn eval_state_value_list() {
        gc_registering_current_thread(|| {
            let store = Store::open("auto").unwrap();
            let es = EvalState::new(store).unwrap();
            let v = es
                .eval_from_string("[ ]".to_string(), "<test>".to_string())
                .unwrap();
            es.force(&v).unwrap();
            let t = es.value_type(&v).unwrap();
            assert!(t == ValueType::List);
        })
        .unwrap();
    }
}
