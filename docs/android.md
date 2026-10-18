# WordCraft para Android

Estado: implementación inicial, 18 de octubre de 2026. APK independiente para Android 8 (API 26) o posterior, ARM64. Conserva el motor y la interfaz egui del proyecto; no ejecuta Electron, WebView, un servidor MCP ni un escritorio remoto.

## Uso

- Teclado y mouse: utiliza la cinta y los atajos actuales del editor. El host incorpora el portapapeles Android para texto y Ctrl+V.
- Táctil: activa los controles amplios; GameActivity integra el teclado en pantalla y el IME. La cinta y los diálogos conservan su diseño de escritorio, adecuado sobre todo para tablets en horizontal.
- Abrir importa una copia al almacenamiento privado de la app. Guardar conserva esa copia. **Exportar a Android** permite elegir un archivo externo mediante el selector del sistema. No se solicita acceso global al almacenamiento.
- Configurar IA muestra las claves de los proveedores en una sola vista, siguiendo el gestor de claves de PDF Scholar: indicador de clave guardada, campo oculto para sustituirla, eliminación, endpoint y modelo. El proveedor activo se elige en Asistente.
- Activa el asistente, configura una clave propia y un modelo con herramientas, escribe la petición y pulsa Ejecutar. El modelo descubre los comandos reales y sus parámetros y los ejecuta en el documento abierto. Puedes detener la operación y deshacer los cambios con las herramientas del editor.

## IA y credenciales

OpenAI, Anthropic y Gemini usan sus protocolos propios; OpenRouter, xAI, Mistral, Groq y endpoints compatibles usan Chat Completions; Azure usa el endpoint del recurso y un deployment. Los modelos de ejemplo son editables, no un catálogo completo ni una garantía de acceso en la cuenta.

Las claves se cifran con AES-256-GCM, AAD por proveedor y una clave no exportable de Android Keystore. No se guardan en mobile.json, archivos de oficina, mensajes de chat, URLs ni registros. La pantalla recibe indicadores de existencia; una clave guardada solo se descifra para autenticar una petición HTTPS dentro del puente Android. Las copias de seguridad de la app están deshabilitadas. Cada APK tiene su propio almacén de claves.

El asistente envía la instrucción, una inspección inicial y los resultados de herramientas al proveedor elegido. Se activa expresamente desde su configuración. Puede guardar la copia local actual cuando se lo pides; la ruta la elige el host, nunca el modelo. No dispone de shell, lectura de claves, apertura o borrado de archivos ni control de ventanas. Las peticiones tienen límites de tamaño, tiempo, rondas y herramientas. Una respuesta tardía se descarta si cambió el documento o su selección. Las ediciones ya ejecutadas permanecen si se cancela una petición o falla la red; el historial de la interfaz muestra lo aplicado.

## Compilación

Requisitos: Rust estable >= 1.95, JDK 17 o 21 **completo**, Android SDK Platform 35, Build Tools 35.0.0, NDK 28.2.13676358. En Linux o macOS:

```sh
rustup target add aarch64-linux-android
sdkmanager 'platforms;android-35' 'build-tools;35.0.0' 'ndk;28.2.13676358'
export ANDROID_HOME=/ruta/al/android-sdk
./apps/wordcraft-android/build-apk.sh
```

El script compila Rust con el perfil android (optimización 1, sin LTO), copia la biblioteca a jniLibs y genera una APK debug firmada para instalación local. Usa `CRAFT_ANDROID_PROFILE=release` para el motor con la optimización release habitual. La APK se encuentra en `apps/wordcraft-android/android/app/build/outputs/apk/debug/app-debug.apk`. Las bibliotecas nativas se enlazan con alineación de 16 KiB; se comprueba también el empaquetado con zipalign. La publicación de cada tag `android-v*` compila y adjunta la APK a GitHub Releases. Es una firma de desarrollo para instalación local, no una firma para Play Store.

## Arquitectura y validación

`src/adapter.rs` conecta el editor existente; `mobile.rs` gestiona la interfaz móvil y el bucle del agente; `protocol.rs` adapta los protocolos de proveedores; `platform.rs` usa llamadas JNI comprobadas y `JavaVM::singleton()`, sin operaciones unsafe. `CraftActivity.java` implementa Android Keystore, Storage Access Framework, HTTPS, portapapeles e impresión. El único permiso unsafe del host Rust corresponde al atributo que exporta el símbolo obligatorio android_main, no a un bloque unsafe.

Las pruebas locales del protocolo se ejecutan con `cargo test -p wordcraft-android`. La compilación nativa se comprueba con `cargo check -p wordcraft-android --target aarch64-linux-android`. La comprobación del proyecto completo sigue siendo `cargo xtask ci`. La referencia de pdfscholar se utilizó para comprender la organización del menú; no se copió su implementación Electron.

## Límites que deben verificarse en un dispositivo

Esta primera integración no certifica paridad completa en Android. Se conserva lo implementado en el editor original, incluidas sus carencias documentadas. Quedan pendientes pruebas reales de teclado español, selección y arrastre con mouse, gestos, teclado en pantalla y composición IME, cambios de orientación, suspensión y recuperación, documentos grandes, impresión y proveedores de archivos en la nube. Las pruebas del protocolo no sustituyen una petición real a cada API con una clave válida.

El portapapeles Android incorpora texto y HTML; WordCraft y DeckCraft también reciben imágenes por URI. La fidelidad entre aplicaciones y los formatos específicos de escritorio (RTF y MathML separados) requieren validación adicional. El gestor de archivos del editor trabaja con copias privadas; los archivos externos se actualizan mediante exportación. La recuperación móvil de GridCraft y DeckCraft conserva periódicamente el documento activo; las sesiones con varios documentos requieren revisar las copias de cada uno antes de cerrar. La vista del presentador en otra pantalla, las integraciones de escritorio necesitan adaptaciones Android específicas. No se afirma que esas funciones estén terminadas.

WordCraft usa su servicio de recuperación existente, que evita conservar copias en claro de documentos protegidos con contraseña. Su selector Guardar como también sincroniza la copia privada con el destino Android elegido mientras la actividad sigue abierta; un fallo conserva la copia local e informa del error.

Android registra asociaciones Abrir con para los formatos de oficina principales. Se importan mediante URI con el permiso concedido por el sistema; cada importación está limitada a 128 MiB. Las imágenes del portapapeles se limitan a 32 MiB. Los enlaces web, correo y teléfono se delegan a las aplicaciones del sistema.
