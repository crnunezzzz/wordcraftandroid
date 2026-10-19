# WordCraft para Android

Estado: implementación inicial, 18 de octubre de 2026. APK independiente para Android 8 (API 26) o posterior, ARM64. Conserva el motor y la interfaz egui del proyecto; no ejecuta Electron, WebView, un servidor MCP ni un escritorio remoto.

## Uso

- Teclado y mouse: utiliza la cinta y los atajos actuales del editor. El host incorpora el portapapeles Android para texto y Ctrl+V.
- Táctil: un dedo desplaza la página (con inercia), tocar hace clic, mantener abre el menú contextual, mantener y arrastrar selecciona, y dos dedos hacen zoom. El teclado en pantalla y el IME están integrados. La cinta y los diálogos conservan su diseño de escritorio, adecuado sobre todo para tablets en horizontal.
- Ratón, lápiz y trackpad: puntero preciso con hover, clic derecho, rueda, deslizamiento con dos dedos y pellizco del trackpad. Atrás cierra paneles y menús; dos veces seguidas envía la app a segundo plano sin cerrar el documento.
- Abrir importa una copia al almacenamiento privado de la app. Guardar conserva esa copia. **Asistente → ⋯ → Exportar una copia** permite elegir un archivo externo mediante el selector del sistema. No se solicita acceso global al almacenamiento.
- El botón flotante ✦ (o la pestaña Asistente) abre el panel lateral. Su cabecera tiene el selector de modelo, nueva conversación, ajustes y cerrar; en pantallas estrechas ocupa todo el ancho.
- Ajustes del asistente: acceso a la IA, claves de OpenAI y Anthropic (indicador de clave guardada, sustituir, eliminar, enlaces para crear la clave y fijar un límite de gasto), modelo, razonamiento y tamaño de la interfaz.
- Escribe una pregunta o un cambio y envía (Intro en un teclado físico; Mayús+Intro añade una línea). El modelo descubre las órdenes reales y sus parámetros y las ejecuta en el documento abierto; la conversación conserva el contexto hasta «Nueva conversación».

## IA y credenciales

El asistente admite **OpenAI** (API Responses) y **Anthropic** (API Messages), con el catálogo actual de PDF Scholar: Claude Opus 5.5, Sonnet 5.5, Haiku 5.5 y Fable 5.1; GPT-6 Sol, GPT-6.1 Sol, GPT-6 Luna y GPT-6 Astra. Cada proveedor conserva su historial nativo y de solo-añadir: los elementos de razonamiento cifrados de OpenAI y los bloques de pensamiento de Claude se reenvían sin cambios entre rondas de herramientas. El razonamiento (bajo, medio, alto) se envía como `reasoning.effort` u `output_config.effort`; si un modelo rechaza un parámetro opcional con un 400, la petición se repite sin él. Opus 5.5, Sonnet 5.5 y Fable 5.1 usan el respaldo del servidor ante un rechazo de seguridad (`fallbacks: "default"`).

Las claves se cifran con AES-256-GCM, AAD por proveedor y una clave no exportable de Android Keystore. No se guardan en archivos de ajustes, documentos, mensajes, URLs ni registros. Java solo envía cada clave a su host (`api.openai.com` o `api.anthropic.com`, HTTPS, sin redirecciones). Las copias de seguridad de la app están deshabilitadas. Cada APK tiene su propio almacén de claves.

Cada mensaje envía la petición y una instantánea del documento (como máximo 96 KiB; el resto se lee con órdenes de inspección) junto con los resultados de las herramientas. «Acceso a la IA: Desactivado» impide cualquier petición. El asistente puede guardar la copia local cuando se le pide; la ruta la elige el host, nunca el modelo. No dispone de shell, lectura de claves, apertura o borrado de archivos ni control de ventanas. Las peticiones tienen límites de tamaño, tiempo (5 min por respuesta), rondas y herramientas. Las ediciones ya ejecutadas permanecen si se detiene una petición; se deshacen con el editor. Los errores muestran el código HTTP y el mensaje del proveedor.

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

## Corrección Android.6 — 2026-10-19

**Ventana nativa.** Android 15 impone el modo edge-to-edge con targetSdk 35 y GameActivity no aplica los insets, así que las barras de estado y navegación tapaban la cinta y la barra de estado del editor. `CraftActivity` dibuja edge-to-edge en todas las versiones y aplica a la superficie un padding igual a las barras del sistema, el recorte de pantalla y el teclado; las franjas usan el color del tema del editor y los iconos del sistema se adaptan al modo claro u oscuro. Atrás ya no termina la actividad.

**Entrada.** GameActivity entregaba los toques crudos a egui, que interpretaba dos punteros (dedos del trackpad o la palma) como pellizco: de ahí el zoom repentino. Además, arrastrar un dedo seleccionaba texto en vez de desplazar. Ahora Java entrega cada `MotionEvent` a una cola y `input.rs` reconoce gestos: arrastrar desplaza con fases de rueda (el lienzo de WordCraft añade su inercia; GridCraft y DeckCraft reciben la del host), tocar = clic, mantener = menú contextual, mantener y arrastrar = selección, dos dedos = zoom y desplazamiento. Ratón, lápiz y trackpad son punteros precisos; el trackpad desplaza con dos dedos (`ACTION_SCROLL` o `CLASSIFICATION_TWO_FINGER_SWIPE`) y hace zoom con pellizco. Un hilo nativo espera en `awaitEvents` y pide un repintado al llegar entrada o respuestas, sin sondeos de 150 ms. El zoom de interfaz de egui con Ctrl +/- queda desactivado y hay un ajuste de tamaño de interfaz (80–150 %).

**Asistente.** Rediseñado según el asistente de PDF Scholar: selector de modelo en una píldora, conversación con burbujas, Markdown sencillo, acciones agrupadas y plegables, compositor redondeado con enviar/detener y tiempo transcurrido, sugerencias propias de cada app y ajustes en el mismo panel. Solo OpenAI y Anthropic. El HTTP 400 anterior se debía a que GPT-6 se pedía por Chat Completions; ahora se usa la API Responses. El panel (`src/assistant.rs`) compila también en escritorio: `cargo run -p wordcraft-android --example assistant_preview -- /ruta/salida` lo renderiza junto al editor real en seis estados.

**Validación.** Pruebas Rust de gestos, teclado y protocolo (`cargo test -p wordcraft-android`) y Clippy con `-D warnings` para `aarch64-linux-android`. La APK se compila con el motor release (opt-level 3) y se firma con v2 y v3; zipalign comprueba la alineación de 16 KiB. No se ha probado en una tablet física: el comportamiento del trackpad del teclado Xiaomi y las barras del sistema de HyperOS deben verificarse en el dispositivo.

**Firma.** Las APK de android.6 se firmaron con una clave nueva, porque la clave de android.4/5 no estaba disponible en el entorno de compilación. Android no permite actualizar sobre una firma distinta: exporta tus documentos, desinstala la versión anterior e instala android.6. La desinstalación borra los documentos privados y las claves API de la app.
