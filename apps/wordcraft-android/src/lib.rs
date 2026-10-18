//! Android host. The document engine and egui editor remain the desktop implementations.
#![deny(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::unimplemented, clippy::unreachable)]

#[cfg(target_os = "android")]
mod adapter;
#[cfg(target_os = "android")]
mod mobile;
#[cfg(target_os = "android")]
mod platform;
pub mod protocol;

// android-activity requires this exact exported Rust ABI symbol. No unsafe operations occur
// here or in the platform bridge; JNI uses checked calls and JavaVM::singleton().
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {
    mobile::start(app);
}
