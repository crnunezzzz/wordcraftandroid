//! The AI assistant panel shared by WordCraft's hosts: the panel and the provider protocols
//! compile everywhere; [`desktop`] runs them on Windows, macOS and Linux (the Android host is
//! `wordcraft-android`).
#![deny(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::unimplemented, clippy::unreachable)]

pub mod agent;
pub mod assistant;
pub mod attach;
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub mod desktop;
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
