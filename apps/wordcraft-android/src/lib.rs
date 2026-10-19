//! Android host. The document engine and egui editor remain the desktop implementations.
#![deny(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::unimplemented, clippy::unreachable)]

#[cfg(target_os = "android")]
mod adapter;
pub mod assistant;
pub mod attach;
pub mod input;
#[cfg(target_os = "android")]
mod mobile;
#[cfg(target_os = "android")]
mod platform;
pub mod protocol;

/// The assistant's texts for this app.
pub const BRANDING: assistant::Branding = assistant::Branding {
    name: "WordCraft",
    noun: "el documento",
    welcome: "Pregunta sobre el documento o pide cambios. El asistente lo lee y lo edita con las mismas órdenes que la línea de comandos.",
    suggestions: &[
        ("Resumen estructurado", "Haz un resumen estructurado de este documento: idea principal, secciones y conclusiones."),
        ("Corrige ortografía y gramática", "Revisa la ortografía y la gramática del documento y corrige los errores directamente."),
        ("Mejora el formato", "Mejora el formato del documento: aplica estilos de título coherentes y un espaciado uniforme."),
        ("Explica el contenido de forma sencilla", "Explica de forma sencilla de qué trata este documento."),
    ],
};

// android-activity requires this exact exported Rust ABI symbol. No unsafe operations occur
// here or in the platform bridge; JNI uses checked calls and JavaVM::singleton().
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {
    mobile::start(app);
}
