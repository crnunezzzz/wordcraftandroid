package ai.craft.mobile;

import android.content.Intent;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.database.Cursor;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import android.view.WindowManager;
import android.print.PrintManager;
import android.print.PrintDocumentAdapter;
import android.print.PrintDocumentInfo;
import android.print.PrintAttributes;
import android.print.PageRange;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import com.google.androidgamesdk.GameActivity;
import org.json.JSONArray;
import org.json.JSONObject;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;
import javax.net.ssl.HttpsURLConnection;
import java.io.*;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.security.KeyStore;
import java.util.*;
import java.util.concurrent.*;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/** Platform capabilities only; the editor and document tools run in Rust, in this process. */
public final class CraftActivity extends GameActivity {
    private static volatile CraftActivity current;
    private final ConcurrentLinkedQueue<JSONObject> events = new ConcurrentLinkedQueue<>();
    private final ExecutorService worker = Executors.newFixedThreadPool(2);
    private final ConcurrentHashMap<Long, HttpsURLConnection> requests = new ConcurrentHashMap<>();
    private final Set<Long> cancelled = ConcurrentHashMap.newKeySet();
    private JSONObject pendingPicker;
    private final ScheduledExecutorService sync = Executors.newSingleThreadScheduledExecutor();
    private final ConcurrentHashMap<File, Uri> saves = new ConcurrentHashMap<>();
    private final Map<File, Long> savedTimes = new ConcurrentHashMap<>();
    private final ConcurrentHashMap<File, String> autoExports = new ConcurrentHashMap<>();
    private final ConcurrentLinkedQueue<JSONObject> exportDialogs = new ConcurrentLinkedQueue<>();
    private static final int PICK = 4101;
    private static final int LIMIT = 128 * 1024 * 1024;
    private static final int HTTP_LIMIT = 2 * 1024 * 1024;
    private static final Set<String> PROVIDERS = new HashSet<>(Arrays.asList("openai", "anthropic", "gemini", "openrouter", "xai", "mistral", "groq", "azure", "custom"));
    static { System.loadLibrary("wordcraft_android"); }

    @Override protected void onCreate(Bundle state) {
        current = this;
        super.onCreate(state);
        getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        sync.scheduleWithFixedDelay(this::flushSaves, 1, 1, TimeUnit.SECONDS);
        importIntent(getIntent());
    }

    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent); setIntent(intent); importIntent(intent);
    }

    private void importIntent(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction()) || intent.getData() == null) return;
        Uri uri = intent.getData();
        if (!Arrays.asList("content", "file").contains(uri.getScheme())) return;
        worker.execute(() -> {
            try {
                File folder = new File(new File(getFilesDir(), "documents"), UUID.randomUUID().toString());
                if (!folder.mkdirs()) throw new IOException("No workspace");
                File file = new File(folder, displayName(uri));
                try (InputStream in = getContentResolver().openInputStream(uri)) {
                    if (in == null) throw new IOException("No input stream");
                    Files.write(file.toPath(), readLimited(in, LIMIT));
                }
                events.add(new JSONObject().put("kind", "file").put("external", true).put("path", file.getAbsolutePath()));
            } catch (Exception e) { emitFileError(new JSONObject(), "No se pudo importar el archivo compartido por Android"); }
        });
    }

    @Override protected void onDestroy() {
        // Keep the Activity alive until Rust's android_main returns (super waits for its loop).
        super.onDestroy();
        worker.shutdownNow();
        sync.shutdown();
        if (current == this) current = null;
    }

    public static String exchange(String source) {
        CraftActivity self = current;
        if (self == null) return "{\"error\":\"Actividad Android no disponible\"}";
        try { return self.dispatch(new JSONObject(source)).toString(); }
        catch (Exception e) { return error("No se pudo completar la operación Android (" + e.getClass().getSimpleName() + ")").toString(); }
    }

    private Object dispatch(JSONObject request) throws Exception {
        String op = request.getString("op");
        switch (op) {
            case "keys": return keyFlags();
            case "clipboard_get": {
                ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
                ClipData clip = clipboard.getPrimaryClip();
                ClipData.Item item = clip == null || clip.getItemCount() == 0 ? null : clip.getItemAt(0);
                CharSequence plain = item == null ? null : item.getText();
                String text = plain == null ? "" : plain.toString();
                if (text.length() > HTTP_LIMIT) throw new IOException("Clipboard too large");
                return new JSONObject().put("text", text).put("html", item == null || item.getHtmlText() == null ? "" : item.getHtmlText());
            }
            case "clipboard_set": {
                String text = request.getString("text");
                String html = request.optString("html");
                if (text.length() > HTTP_LIMIT || html.length() > HTTP_LIMIT) throw new IOException("Clipboard too large");
                runOnUiThread(() -> ((ClipboardManager) getSystemService(CLIPBOARD_SERVICE)).setPrimaryClip(html.isEmpty() ? ClipData.newPlainText("Craft", text) : ClipData.newHtmlText("Craft", text, html)));
                return new JSONObject();
            }
            case "clipboard_image": {
                ClipData clip = ((ClipboardManager) getSystemService(CLIPBOARD_SERVICE)).getPrimaryClip();
                if (clip == null || clip.getItemCount() == 0) return new JSONObject();
                Uri uri = clip.getItemAt(0).getUri();
                String type = uri == null ? null : getContentResolver().getType(uri);
                if (type == null || !type.startsWith("image/")) return new JSONObject();
                File file = new File(getCacheDir(), "clipboard-image");
                try (InputStream in = getContentResolver().openInputStream(uri)) {
                    if (in == null) throw new IOException("No clipboard image");
                    Files.write(file.toPath(), readLimited(in, 32 * 1024 * 1024));
                }
                return new JSONObject().put("path", file.getAbsolutePath());
            }
            case "open_url": {
                Uri uri = Uri.parse(request.getString("url"));
                if (!Arrays.asList("https", "http", "mailto", "tel").contains(uri.getScheme())) throw new IOException("Unsupported link");
                runOnUiThread(() -> {
                    try { startActivity(new Intent(Intent.ACTION_VIEW, uri)); }
                    catch (Exception e) { emitFileError(new JSONObject(), "No hay una aplicación disponible para abrir este enlace"); }
                });
                return new JSONObject();
            }
            case "print": {
                File pdf = privateFile(request.getString("path"));
                runOnUiThread(() -> printPdf(pdf));
                return new JSONObject();
            }
            case "set_key": {
                String provider = provider(request);
                String key = request.getString("key").trim();
                if (key.isEmpty() || key.length() > 8192 || key.contains("\n") || key.contains("\r")) throw new IOException("Invalid key");
                Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
                cipher.init(Cipher.ENCRYPT_MODE, secret());
                cipher.updateAAD(provider.getBytes(StandardCharsets.UTF_8));
                String stored = Base64.encodeToString(cipher.getIV(), Base64.NO_WRAP) + ":" + Base64.encodeToString(cipher.doFinal(key.getBytes(StandardCharsets.UTF_8)), Base64.NO_WRAP);
                if (!getSharedPreferences("vault", MODE_PRIVATE).edit().putString(provider, stored).commit()) throw new IOException("Key storage failed");
                return keyFlags();
            }
            case "delete_key":
                if (!getSharedPreferences("vault", MODE_PRIVATE).edit().remove(provider(request)).commit()) throw new IOException("Key deletion failed");
                return keyFlags();
            case "poll": {
                runOnUiThread(() -> { if (pendingPicker == null) { JSONObject next = exportDialogs.poll(); if (next != null) showPicker(next); } });
                JSONArray result = new JSONArray();
                for (int n = 0; n < 16; n++) { JSONObject event = events.poll(); if (event == null) break; result.put(event); }
                return result;
            }
            case "local_save": {
                File folder = new File(new File(getFilesDir(), "documents"), UUID.randomUUID().toString());
                if (!folder.mkdirs()) throw new IOException("No workspace");
                File file = new File(folder, safeName(request.getString("name")));
                if (request.optBoolean("export_after_write")) autoExports.put(file, file.getName());
                return new JSONObject().put("path", file.getAbsolutePath());
            }
            case "frame_done": {
                // Engine file commands are synchronous; the frame ending marks their outputs ready.
                for (Map.Entry<File, String> entry : autoExports.entrySet()) {
                    File file = entry.getKey(); File parent = file.getParentFile();
                    File[] outputs = parent == null ? null : parent.listFiles(File::isFile);
                    if (outputs != null && outputs.length > 0 && autoExports.remove(file, entry.getValue())) {
                        worker.execute(() -> queueExport(file, entry.getValue(), outputs));
                    }
                }
                return new JSONObject();
            }
            case "open": case "save_picker": case "export": {
                runOnUiThread(() -> showPicker(request));
                return new JSONObject();
            }
            case "http":
                provider(request);
                worker.execute(() -> http(request));
                return new JSONObject();
            case "cancel_http": {
                long ticket = request.getLong("ticket");
                cancelled.add(ticket);
                HttpsURLConnection active = requests.remove(ticket);
                if (active != null) active.disconnect();
                return new JSONObject();
            }
            default: throw new IOException("Unknown operation");
        }
    }

    private String provider(JSONObject request) throws Exception {
        String id = request.getString("provider");
        if (!PROVIDERS.contains(id)) throw new IOException("Unknown provider");
        return id;
    }

    private SecretKey secret() throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore"); store.load(null);
        if (store.containsAlias("craft-api-keys")) return ((KeyStore.SecretKeyEntry) store.getEntry("craft-api-keys", null)).getSecretKey();
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder("craft-api-keys", KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setKeySize(256).build());
        return generator.generateKey();
    }

    private JSONObject keyFlags() throws Exception {
        JSONObject flags = new JSONObject();
        for (String id : PROVIDERS) flags.put(id, getSharedPreferences("vault", MODE_PRIVATE).contains(id));
        return flags;
    }

    private String key(String id) throws Exception {
        String stored = getSharedPreferences("vault", MODE_PRIVATE).getString(id, "");
        if (stored.isEmpty()) throw new IOException("Missing API key");
        String[] parts = stored.split(":", 2);
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, secret(), new GCMParameterSpec(128, Base64.decode(parts[0], Base64.NO_WRAP)));
        cipher.updateAAD(id.getBytes(StandardCharsets.UTF_8));
        return new String(cipher.doFinal(Base64.decode(parts[1], Base64.NO_WRAP)), StandardCharsets.UTF_8);
    }

    private void http(JSONObject request) {
        JSONObject event = new JSONObject();
        HttpsURLConnection connection = null;
        try {
            event.put("kind", "http").put("ticket", request.getLong("ticket"));
            if (cancelled.remove(request.getLong("ticket"))) throw new IOException("Cancelled");
            URL url = new URL(request.getString("url"));
            if (!url.getProtocol().equals("https") || url.getUserInfo() != null || url.getRef() != null) throw new IOException("HTTPS required");
            connection = (HttpsURLConnection) url.openConnection();
            requests.put(request.getLong("ticket"), connection);
            connection.setInstanceFollowRedirects(false);
            connection.setConnectTimeout(20_000); connection.setReadTimeout(90_000);
            connection.setRequestMethod("POST"); connection.setDoOutput(true);
            connection.setRequestProperty("Content-Type", "application/json");
            String api = request.getString("api");
            String value = key(provider(request));
            if (api.equals("anthropic")) {
                connection.setRequestProperty("x-api-key", value);
                connection.setRequestProperty("anthropic-version", "2023-06-01");
            } else if (api.equals("gemini")) connection.setRequestProperty("x-goog-api-key", value);
            else if (api.equals("azure")) connection.setRequestProperty("api-key", value);
            else connection.setRequestProperty("Authorization", "Bearer " + value);
            byte[] bytes = request.getJSONObject("body").toString().getBytes(StandardCharsets.UTF_8);
            if (bytes.length > HTTP_LIMIT) throw new IOException("Request too large");
            connection.setFixedLengthStreamingMode(bytes.length);
            try (OutputStream out = connection.getOutputStream()) { out.write(bytes); }
            int status = connection.getResponseCode();
            if (status < 200 || status >= 300) {
                String message = status == 401 || status == 403 ? "El proveedor rechazó la clave o su acceso al modelo" : status == 429 ? "Límite de uso o cuota del proveedor" : "El proveedor respondió HTTP " + status;
                event.put("error", message);
            } else {
                try (InputStream in = connection.getInputStream()) { event.put("body", new JSONObject(new String(readLimited(in, HTTP_LIMIT), StandardCharsets.UTF_8))); }
            }
        } catch (Exception e) {
            try { event.put("error", "No se pudo consultar la API. Revisa la conexión, la clave, el endpoint y el modelo (" + e.getClass().getSimpleName() + ")."); } catch (Exception ignored) { }
        } finally {
            requests.remove(request.optLong("ticket")); cancelled.remove(request.optLong("ticket"));
            if (connection != null) connection.disconnect(); events.add(event);
        }
    }

    private void showPicker(JSONObject request) {
        try {
            if (pendingPicker != null) throw new IOException("Ya hay un selector de archivos abierto");
            String op = request.getString("op");
            Intent intent = new Intent(op.equals("open") ? Intent.ACTION_OPEN_DOCUMENT : Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE); intent.setType("*/*");
            if (!op.equals("open")) intent.putExtra(Intent.EXTRA_TITLE, safeName(request.optString("name", "document")));
            pendingPicker = request;
            startActivityForResult(intent, PICK);
        } catch (Exception e) { emitFileError(request, "No se pudo abrir el selector de Android"); pendingPicker = null; }
    }

    @Override protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != PICK || pendingPicker == null) return;
        JSONObject request = pendingPicker; pendingPicker = null;
        if (resultCode != RESULT_OK || data == null || data.getData() == null) {
            try { events.add(new JSONObject().put("kind", "file").put("ticket", request.optLong("ticket")).put("cancelled", true)); } catch (Exception ignored) { }
            return;
        }
        Uri uri = data.getData();
        worker.execute(() -> {
            try {
                String op = request.getString("op");
                JSONObject event = new JSONObject().put("kind", "file").put("ticket", request.optLong("ticket")).put("purpose", request.optString("purpose"));
                if (op.equals("export")) {
                    File source = privateFile(request.getString("path"));
                    try (InputStream in = new FileInputStream(source); OutputStream out = getContentResolver().openOutputStream(uri, "wt")) {
                        if (out == null) throw new IOException("No output stream");
                        byte[] buffer = new byte[32 * 1024]; int size;
                        while ((size = in.read(buffer)) != -1) out.write(buffer, 0, size);
                        out.flush();
                    }
                    event.put("exported", true);
                } else {
                    File root = new File(getFilesDir(), "documents");
                    File folder = new File(root, UUID.randomUUID().toString());
                    if (!folder.mkdirs()) throw new IOException("No workspace");
                    String name = op.equals("open") ? displayName(uri) : request.optString("name", "document");
                    File file = new File(folder, safeName(name));
                    if (op.equals("open")) {
                        try (InputStream in = getContentResolver().openInputStream(uri)) {
                            if (in == null) throw new IOException("No input stream");
                            Files.write(file.toPath(), readLimited(in, LIMIT));
                        }
                    } else {
                        // The engine writes this private path after receiving the event. Mirror only
                        // completed writes; errors keep the private copy and are visible in the UI.
                        saves.put(file, uri);
                    }
                    event.put("path", file.getAbsolutePath()).put("name", file.getName());
                }
                events.add(event);
            } catch (Exception e) { emitFileError(request, "No se pudo leer o exportar el archivo. Se conserva la copia local."); }
        });
    }

    private File privateFile(String path) throws IOException {
        File file = new File(path).getCanonicalFile();
        String root = getFilesDir().getCanonicalPath() + File.separator;
        if (!file.getPath().startsWith(root) || !file.isFile()) throw new IOException("Not a private document");
        return file;
    }

    private void queueExport(File expected, String name, File[] outputs) {
        try {
            File result = expected;
            if (!expected.isFile() || outputs.length > 1) {
                result = new File(expected.getParentFile(), "export.zip");
                try (ZipOutputStream zip = new ZipOutputStream(new FileOutputStream(result))) {
                    byte[] buffer = new byte[32 * 1024];
                    for (File output : outputs) {
                        zip.putNextEntry(new ZipEntry(output.getName()));
                        try (InputStream in = new FileInputStream(output)) { int count; while ((count = in.read(buffer)) != -1) zip.write(buffer, 0, count); }
                        zip.closeEntry();
                    }
                }
                name += ".zip";
            }
            exportDialogs.add(new JSONObject().put("op", "export").put("path", result.getAbsolutePath()).put("name", name).put("ticket", 9001));
        } catch (Exception e) { emitFileError(new JSONObject(), "No se pudo preparar la exportación. Se conservan los archivos locales."); }
    }

    private String displayName(Uri uri) {
        try (Cursor cursor = getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) return safeName(cursor.getString(0));
        } catch (Exception ignored) { }
        return "document";
    }

    private static String safeName(String name) {
        String clean = name.replaceAll("[\\\\/\\p{Cntrl}]", "_").replace("..", "_");
        return clean.trim().isEmpty() ? "document" : clean.substring(0, Math.min(180, clean.length()));
    }

    private static byte[] readLimited(InputStream in, int limit) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[32 * 1024];
        int size;
        while ((size = in.read(buffer)) != -1) {
            if (size > limit - out.size()) throw new IOException("File too large");
            out.write(buffer, 0, size);
        }
        return out.toByteArray();
    }

    private void emitFileError(JSONObject request, String message) {
        try { events.add(new JSONObject().put("kind", "file").put("ticket", request.optLong("ticket")).put("error", message)); } catch (Exception ignored) { }
    }
    private void flushSaves() {
        for (Map.Entry<File, Uri> entry : saves.entrySet()) {
            File file = entry.getKey();
            if (!file.isFile()) continue;
            long version = file.lastModified() ^ file.length();
            if (Objects.equals(savedTimes.get(file), version) || System.currentTimeMillis() - file.lastModified() < 800) continue;
            try (InputStream in = new FileInputStream(file); OutputStream out = getContentResolver().openOutputStream(entry.getValue(), "wt")) {
                if (out == null) throw new IOException("No output stream");
                byte[] buffer = new byte[32 * 1024]; int size;
                while ((size = in.read(buffer)) != -1) out.write(buffer, 0, size);
                out.flush(); savedTimes.put(file, version);
                events.add(new JSONObject().put("kind", "saved").put("exported", true));
            } catch (Exception e) {
                saves.remove(file);
                emitFileError(new JSONObject(), "No se pudo actualizar el destino Android. Tu archivo está guardado localmente; usa Exportar a Android.");
            }
        }
    }
    private void printPdf(File pdf) {
        PrintDocumentAdapter adapter = new PrintDocumentAdapter() {
            @Override public void onLayout(PrintAttributes oldAttrs, PrintAttributes newAttrs, CancellationSignal cancel, LayoutResultCallback result, Bundle extras) {
                if (cancel.isCanceled()) result.onLayoutCancelled();
                else result.onLayoutFinished(new PrintDocumentInfo.Builder(pdf.getName()).setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT).build(), true);
            }
            @Override public void onWrite(PageRange[] pages, ParcelFileDescriptor destination, CancellationSignal cancel, WriteResultCallback result) {
                worker.execute(() -> {
                    try (InputStream in = new FileInputStream(pdf); OutputStream out = new FileOutputStream(destination.getFileDescriptor())) {
                        byte[] buffer = new byte[32 * 1024]; int size;
                        while ((size = in.read(buffer)) != -1) { if (cancel.isCanceled()) { result.onWriteCancelled(); return; } out.write(buffer, 0, size); }
                        out.flush(); result.onWriteFinished(new PageRange[]{PageRange.ALL_PAGES});
                    } catch (IOException e) { result.onWriteFailed("No se pudo imprimir el documento"); }
                });
            }
        };
        ((PrintManager) getSystemService(PRINT_SERVICE)).print("Craft", adapter, null);
    }
    private static JSONObject error(String message) {
        JSONObject value = new JSONObject(); try { value.put("error", message); } catch (Exception ignored) { } return value;
    }
}
