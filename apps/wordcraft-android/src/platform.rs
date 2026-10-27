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

/// The activity class, found through the main thread's context class loader (a natively
/// attached thread only sees the system class loader).
fn load_class(vm: &JavaVM) -> Result<Global<JClass<'static>>, String> {
    vm.attach_current_thread(|env| -> jni::errors::Result<_> {
        let thread = env.call_static_method(jni_str!("java/lang/Thread"), jni_str!("currentThread"), jni_sig!(() -> java.lang.Thread), &[])?.l()?;
        let loader = env.call_method(&thread, jni_str!("getContextClassLoader"), jni_sig!(() -> java.lang.ClassLoader), &[])?.l()?;
        let name = env.new_string("ai.craft.mobile.CraftActivity")?;
        let class = env.call_method(&loader, jni_str!("loadClass"), jni_sig!((JString) -> java.lang.Class), &[JValue::Object(&name)])?.l()?;
        let class = env.cast_local::<JClass>(class)?;
        env.new_global_ref(class)
    })
    .map_err(|e| e.to_string())
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
        let class = load_class(&vm)?;
        Ok(Self(Rc::new(RefCell::new(Bridge { vm, class }))))
    }

    /// Repaint as soon as Android queues input or a reply, instead of on the next timer tick.
    /// The thread blocks in Java (`awaitEvents`) and never touches egui state beyond the waker.
    pub fn spawn_waker(&self, ctx: egui::Context) -> Result<(), String> {
        let class = load_class(&self.0.borrow().vm)?;
        std::thread::Builder::new()
            .name("craft-input-waker".into())
            .spawn(move || {
                let Ok(vm) = JavaVM::singleton() else { return };
                loop {
                    let woke = vm.attach_current_thread(|env| -> jni::errors::Result<bool> {
                        env.call_static_method(&class, jni_str!("awaitEvents"), jni_sig!((jint) -> jboolean), &[JValue::Int(1000)])?.z()
                    });
                    match woke {
                        Ok(true) => ctx.request_repaint(),
                        Ok(false) => {}
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(250)),
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
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
