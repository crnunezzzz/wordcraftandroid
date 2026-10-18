//! Checked JNI calls only. Java owns Android lifecycle, SAF, HTTPS and the Keystore.
#![forbid(unsafe_code)]

use jni::{
    JavaVM, jni_sig, jni_str,
    objects::{JClass, JString, JValue},
    refs::Global,
};
use serde_json::Value;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct Platform(Rc<RefCell<Bridge>>);
struct Bridge {
    vm: JavaVM,
    class: Global<JClass<'static>>,
}

impl Platform {
    pub fn clipboard(&self, field: &str) -> Option<String> {
        self.exchange(serde_json::json!({"op":"clipboard_get"})).ok()?.get(field)?.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
    }
    // Some editors expose image pasting; others expose only the text/HTML clipboard.
    #[allow(dead_code)]
    pub fn clipboard_image(&self) -> Option<Vec<u8>> {
        let value = self.exchange(serde_json::json!({"op":"clipboard_image"})).ok()?;
        std::fs::read(value["path"].as_str()?).ok()
    }
    pub fn copy_html(&self, html: &str, text: &str) -> Result<(), String> {
        self.exchange(serde_json::json!({"op":"clipboard_set","html":html,"text":text})).map(|_| ())
    }
    pub fn new() -> Result<Self, String> {
        let vm = JavaVM::singleton().map_err(|e| e.to_string())?;
        let class = vm
            .attach_current_thread(|env| -> jni::errors::Result<_> {
                let thread =
                    env.call_static_method(jni_str!("java/lang/Thread"), jni_str!("currentThread"), jni_sig!(() -> java.lang.Thread), &[])?.l()?;
                let loader = env.call_method(&thread, jni_str!("getContextClassLoader"), jni_sig!(() -> java.lang.ClassLoader), &[])?.l()?;
                let name = env.new_string("ai.craft.mobile.CraftActivity")?;
                let class = env.call_method(&loader, jni_str!("loadClass"), jni_sig!((JString) -> java.lang.Class), &[JValue::Object(&name)])?.l()?;
                let class = env.cast_local::<JClass>(class)?;
                env.new_global_ref(class)
            })
            .map_err(|e| e.to_string())?;
        Ok(Self(Rc::new(RefCell::new(Bridge { vm, class }))))
    }

    pub fn exchange(&self, request: Value) -> Result<Value, String> {
        let bridge = self.0.borrow();
        let response = bridge
            .vm
            .attach_current_thread(|env| -> jni::errors::Result<String> {
                let arg = env.new_string(request.to_string())?;
                let out =
                    env.call_static_method(&bridge.class, jni_str!("exchange"), jni_sig!((JString) -> JString), &[JValue::Object(&arg)])?.l()?;
                env.cast_local::<JString>(out)?.mutf8_chars(env).map(|s| s.to_string())
            })
            .map_err(|e| format!("Android: {e}"))?;
        let response: Value = serde_json::from_str(&response).map_err(|_| "Respuesta Android inválida")?;
        if let Some(error) = response["error"].as_str() {
            return Err(error.into());
        }
        Ok(response)
    }
}
