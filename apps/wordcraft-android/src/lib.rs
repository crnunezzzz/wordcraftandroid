//! Android host. The document engine and egui editor remain the desktop implementations.
#![deny(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::unimplemented, clippy::unreachable)]

#[cfg(target_os = "android")]
mod adapter;
pub mod input;
#[cfg(target_os = "android")]
mod mobile;
#[cfg(target_os = "android")]
mod platform;
// The panel, protocols and texts are shared with the desktop app.
pub use wordcraft_assistant::{BRANDING, assistant, attach, protocol};

// android-activity requires this exact exported Rust ABI symbol. No unsafe operations occur
// here or in the platform bridge; JNI uses checked calls and JavaVM::singleton().
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {
    mobile::start(app);
}
