# WordCraft IA para Windows

> Estado · 2026-10-20: primera versión de escritorio del asistente (Windows x64; también compila en
> macOS y Linux). Las pruebas automáticas pasan; falta probarla a mano en un PC con Windows.

WordCraft IA es WordCraft con el asistente de IA de la versión Android, en el escritorio. El
asistente se abre desde la pestaña **Asistente** de la cinta. Lee el documento abierto y lo edita
con las mismas órdenes que la línea de comandos.

## Conectar la IA

En el asistente › engranaje › **Conexión con la IA**, cada proveedor se conecta de una de dos formas:

| Forma | Qué hace | Qué necesitas |
|---|---|---|
| **Clave de API** (OpenAI o Anthropic) | La app envía las peticiones directamente a la API del proveedor. | Una clave con saldo. Se guarda en el Administrador de credenciales de Windows y solo viaja a su proveedor. |
| **Plan de ChatGPT** | La app usa **Codex**, la herramienta oficial de OpenAI, con tu sesión de ChatGPT. | Un plan de ChatGPT que incluya Codex. |
| **Plan de Claude** | La app usa **Claude Code**, la herramienta oficial de Anthropic, con tu sesión de Claude. | Un plan de Claude que incluya Claude Code (Pro, Max, Team o Enterprise). |

Con un plan, **la app nunca ve tu contraseña ni tus credenciales**. «Instalar» abre una ventana
con el instalador oficial del proveedor, e «Iniciar sesión» abre su inicio de sesión, que sigue
en el navegador. La app arranca la herramienta tal como la instala el proveedor y lee sus
respuestas. Las peticiones cuentan en el uso de tu plan.

### Qué puede hacer la IA con un plan

La herramienta se arranca sin herramientas propias: ni terminal, ni archivos, ni web, ni
parches. Solo recibe las dos herramientas del documento (`list_commands` y `execute_command`), a
través de un servidor MCP que es la propia app (`wordcraft.exe --assistant-mcp`). Ese servidor
habla con la ventana abierta por un socket local (127.0.0.1) protegido con un token aleatorio. Se
aplican la misma lista de órdenes permitidas y los mismos límites que con la clave de API: sin
rutas de archivo, credenciales, macros ni procesos.

### Diferencias con la clave de API

- **Codex (plan de ChatGPT)** lee imágenes y el texto de los documentos adjuntos, pero no PDF.
  Con la clave de API de OpenAI, el PDF se envía entero.
- Con un plan, los adjuntos de un mensaje pueden ocupar hasta 9 MB (con la clave de API, 28 MB).
- «Razonamiento» (bajo, medio, alto) se aplica a Codex y a la API. Claude Code usa su propio ajuste.

### Por qué no hay un «Iniciar sesión con Claude» dentro de la app

Anthropic no permite que otras aplicaciones ofrezcan el inicio de sesión de Claude.ai ni que
guarden o reutilicen sus tokens. Por eso la app no lo hace: usa tu Claude Code sin modificar, con
la sesión que tú iniciaste en él. Con ChatGPT pasa lo mismo: la app no reutiliza el inicio de
sesión de Codex, sino que usa tu Codex con tu propia sesión.

## Instalar y actualizar sin que el launcher lo rompa

El launcher de Craft Apps instala y actualiza el **WordCraft oficial**, que no trae el asistente.
Si se reemplazaran los archivos del WordCraft oficial, la siguiente actualización del launcher
los sobrescribiría. Por eso WordCraft IA:

1. **Se instala aparte**, en `%LOCALAPPDATA%\Programs\WordCraft IA`, con su propio acceso
   directo «WordCraft IA». El launcher no toca esa carpeta, y WordCraft IA tampoco toca la del
   oficial. Los dos pueden convivir y comparten ajustes y documentos recientes.
2. **Recibe las mejoras del proyecto original** con el workflow
   [`ia-windows.yml`](../.github/workflows/ia-windows.yml). Cada día fusiona
   `storytold/wordcraft` en `main`, prueba el asistente sobre el código fusionado y publica una
   versión nueva en *Releases*. Si el proyecto original cambia las mismas líneas que este fork,
   no se publica nada: el workflow abre un issue y la última versión sigue disponible.
3. **Toca lo mínimo del código original**, para que esas fusiones casi nunca choquen:
   - todo el asistente vive en `apps/wordcraft-assistant` (compartido con Android);
   - la app de escritorio solo añade `apps/wordcraft/src/assistant_host.rs` y unas pocas líneas
     en `main.rs`.

Para actualizar: descarga el zip de la última versión en *Releases* y vuelve a ejecutar
`install-ia.ps1`.

### Ajustes del repositorio

- El workflow necesita que `main` contenga este trabajo: fusiona el pull request.
- Recomendado: crea el secreto **SYNC_TOKEN**, un token personal con los permisos `repo` y
  `workflow`. Sin él, GitHub impide que el workflow suba una fusión en la que el proyecto
  original cambió sus propios workflows. En ese caso se abre un issue avisándolo.
- Para publicar una versión al momento: *Actions* › «IA Windows» › *Run workflow*.

## Desarrollo

- Pruebas: `cargo test -p wordcraft-assistant --lib`. Incluyen una prueba de extremo a extremo
  de la vía con plan, con una CLI falsa que edita el documento a través del puente.
- Vista previa del panel sin ventana:
  `cargo run -p wordcraft-assistant --example assistant_preview -- <carpeta> desktop-plans desktop-chat`.
- Compilación cruzada desde Linux:
  `cargo build --release -p wordcraft -p wordcraft-cli --target x86_64-pc-windows-gnu`.
  Necesita MinGW. Las versiones publicadas usan MSVC en `windows-latest`.
