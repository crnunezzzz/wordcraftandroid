# WordCraft para Android

Estado: implementación inicial, 18 de octubre de 2026. APK independiente para Android 8 (API 26) o posterior, ARM64. Conserva el motor y la interfaz egui del proyecto; no ejecuta Electron, WebView, un servidor MCP ni un escritorio remoto.

## Uso

- Teclado y mouse: utiliza la cinta y los atajos actuales del editor. El host incorpora el portapapeles Android para texto y Ctrl+V.
- Táctil: activa los controles amplios; GameActivity integra el teclado en pantalla y el IME. La cinta y los diálogos conservan su diseño de escritorio, adecuado sobre todo para tablets en horizontal.
- Abrir importa una copia al almacenamiento privado de la app. Guardar conserva esa copia. **Asistente → ⋯ → Exportar una copia** permite elegir un archivo externo mediante el selector del sistema. No se solicita acceso global al almacenamiento.
- Asistente → selector de modelo → API keys y proveedores muestra las claves de los proveedores en una sola vista, siguiendo el gestor de claves de PDF Scholar: indicador de clave guardada, campo oculto para sustituirla, eliminación, endpoint y modelo. El proveedor activo se elige en Asistente.
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

El script compila Rust con el perfil android (optimización 1, sin LTO), copia la biblioteca a jniLibs y genera una APK debug firmada para instalación local. Usa `CRAFT_ANDROID_PROFILE=release` para el motor con la optimización release habitual. La APK se encuentra en `apps/wordcraft-android/android/app/build/outputs/apk/debug/app-debug.apk`. Las bibliotecas nativas se enlazan con alineación de 16 KiB; se comprueba también el empaquetado con zipalign. Cada tag `android-v*` compila una APK release sin firmar como artefacto de Actions. Para distribuirla, se firma fuera del repositorio con una clave persistente y se adjunta manualmente a GitHub Releases. Nunca se publica el artefacto sin firmar como descarga instalable.

Para generar una release firmada, configura `CRAFT_ANDROID_VARIANT=release`, `CRAFT_ANDROID_KEYSTORE` (ruta PKCS12, alias `craft`) y `CRAFT_ANDROID_KEYSTORE_PASSWORD` en el entorno de compilación. Conserva una copia privada de esa clave para futuras actualizaciones; no la incluyas en Git ni en los artefactos públicos.

La versión android.3 usaba una firma debug temporal de Actions. Para pasar a android.4 hay que exportar primero los documentos locales, desinstalar la versión anterior e instalar la nueva. La desinstalación borra documentos privados y claves API; las copias exportadas permanecen fuera de la app. Las siguientes actualizaciones deben conservar la firma de android.4.

## Arquitectura y validación

`src/adapter.rs` conecta el editor existente; `mobile.rs` gestiona la interfaz móvil y el bucle del agente; `protocol.rs` adapta los protocolos de proveedores; `platform.rs` usa llamadas JNI comprobadas y `JavaVM::singleton()`, sin operaciones unsafe. `CraftActivity.java` implementa Android Keystore, Storage Access Framework, HTTPS, portapapeles e impresión. El único permiso unsafe del host Rust corresponde al atributo que exporta el símbolo obligatorio android_main, no a un bloque unsafe.

Las pruebas locales del protocolo se ejecutan con `cargo test -p wordcraft-android`. La compilación nativa se comprueba con `cargo check -p wordcraft-android --target aarch64-linux-android`. La comprobación del proyecto completo sigue siendo `cargo xtask ci`. La referencia de pdfscholar se utilizó para comprender la organización del menú; no se copió su implementación Electron.

## Límites que deben verificarse en un dispositivo

Esta primera integración no certifica paridad completa en Android. Se conserva lo implementado en el editor original, incluidas sus carencias documentadas. Quedan pendientes pruebas reales de teclado español, selección y arrastre con mouse, gestos, teclado en pantalla y composición IME, cambios de orientación, suspensión y recuperación, documentos grandes, impresión y proveedores de archivos en la nube. Las pruebas del protocolo no sustituyen una petición real a cada API con una clave válida.

El portapapeles Android incorpora texto y HTML; WordCraft y DeckCraft también reciben imágenes por URI. La fidelidad entre aplicaciones y los formatos específicos de escritorio (RTF y MathML separados) requieren validación adicional. El gestor de archivos del editor trabaja con copias privadas; los archivos externos se actualizan mediante exportación. La recuperación móvil de GridCraft y DeckCraft conserva periódicamente el documento activo; las sesiones con varios documentos requieren revisar las copias de cada uno antes de cerrar. La vista del presentador en otra pantalla, las integraciones de escritorio necesitan adaptaciones Android específicas. No se afirma que esas funciones estén terminadas.

WordCraft usa su servicio de recuperación existente, que evita conservar copias en claro de documentos protegidos con contraseña. Su selector Guardar como también sincroniza la copia privada con el destino Android elegido mientras la actividad sigue abierta; un fallo conserva la copia local e informa del error.

Android registra asociaciones Abrir con para los formatos de oficina principales. Se importan mediante URI con el permiso concedido por el sistema; cada importación está limitada a 128 MiB. Las imágenes del portapapeles se limitan a 32 MiB. Los enlaces web, correo y teléfono se delegan a las aplicaciones del sistema.

## Corrección Android.4 — 2026-10-18

La pestaña **Asistente**, junto a Ayuda, abre el panel lateral; el documento permanece visible. El selector reúne proveedores/modelos y acceso a sus API keys; la configuración se muestra dentro del mismo panel. Se elimina la franja externa de navegación y se usan los colores nativos en modo claro y oscuro. El icono del lanzador procede del recurso original de wordcraft.

`EditorInputConnection` traduce composición, confirmación y borrado del teclado del sistema; los eventos Unicode y modificadores del teclado externo pasan por una cola explícita hasta egui, evitando duplicados de winit. Las pruebas de entrada Rust y Java cubren estos contratos; todavía se requiere validación física en la Xiaomi Pad 6. Para las pruebas Java: `cd apps/wordcraft-android/android && ./gradlew :app:testDebugUnitTest`.

### Validación de esta actualización

Las seis pruebas Rust de entrada/protocolo y las cuatro pruebas Java de InputConnection pasan. La comprobación Clippy de Android pasa con advertencias tratadas como errores. Las APK release ARM64 están firmadas y verificadas con apksigner y zipalign (16 KiB). Se revisaron capturas del editor con la pestaña Asistente en modo claro y oscuro.

El CI general no está completamente verde en este entorno: fallan tres pruebas existentes de cobertura combinada tailandés/latín en fonts/layout. Formato, Clippy, assets, capas y WASM pasan; también pasa la prueba de posición y apertura de la pestaña Asistente. No se han cambiado esas pruebas de fuentes para ocultar el fallo.

La publicación firmada puede ejecutarse con el workflow `Publish signed Android APK`: recibe un commit inmutable de la rama `android-apks` con la APK ya firmada y su SHA-256, verifica ambos y adjunta los archivos a una release existente. Esa rama contiene únicamente binarios públicos; las claves privadas de firma nunca se suben al repositorio.
La publicación también se activa al subir la rama `android-apks`; en ese caso verifica el mismo commit recibido y publica android.5.

## Corrección Android.5 — 2026-10-19

Corrige el cierre nativo durante `CraftActivity.onCreate`: en GameActivity 4.4.0, `getGameActivityNativeHandle()` devuelve cero mientras la actividad está activa. La llamada adicional a `setInputConnectionNative` desreferenciaba ese cero. Ahora se sustituye la conexión mediante el punto de enlace original de GameActivity, conservando el identificador válido que entrega el ciclo de vida y rechazando identificadores nulos. No se modifica el diseño ni el procesamiento de texto.

La regresión Java comprueba la conexión del editor con el identificador recibido, el rechazo de cero y la conexión nueva tras recrear la actividad. La prueba sustituye únicamente la biblioteca nativa; no equivale a una ejecución en la tablet. Android.5 incrementa versionCode a 3 y conserva la firma de android.4, por lo que se instala como actualización sin desinstalar ni borrar documentos.

Validación: cinco pruebas Java por aplicación, compilación release ARM64, firma idéntica a android.4 y alineación ZIP de 16 KiB verificadas. GridCraft supera el CI completo. El CI general de WordCraft sigue bloqueado por cobertura de fuentes tailandés/latín; el de DeckCraft por la dependencia ALSA del ejecutable Linux. No se ha probado esta APK en una tablet física.
