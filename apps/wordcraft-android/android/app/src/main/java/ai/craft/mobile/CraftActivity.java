package ai.craft.mobile;

import android.content.Intent;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.ColorFilter;
import android.graphics.Paint;
import android.graphics.PixelFormat;
import android.graphics.drawable.Drawable;
import android.os.Build;
import android.view.MotionEvent;
import android.view.View;
import android.view.Window;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;
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
import android.view.KeyEvent;
import android.view.KeyCharacterMap;
import android.view.inputmethod.EditorInfo;
import android.text.InputType;
import com.google.androidgamesdk.gametextinput.GameTextInput;
import com.google.androidgamesdk.gametextinput.InputConnection;
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
    private EditorInputConnection editorConnection;
    private int pendingAccent;
    // Keys, IME edits and pointer events, in order. Guarded by itself.
    private final ArrayDeque<JSONObject> inputEvents = new ArrayDeque<>();
    private static volatile CraftActivity current;
    // Released whenever input or a platform event is queued; the native waker thread waits on it
    // so the egui loop repaints at once instead of on its next timer tick.
    private static final Semaphore wake = new Semaphore(0);
    private final ConcurrentLinkedQueue<JSONObject> events = new ConcurrentLinkedQueue<>();
    private final BarsDrawable bars = new BarsDrawable();
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
    // Requests carry attached images and PDFs inline (base64); the providers accept 32 MB.
    private static final int HTTP_LIMIT = 32 * 1024 * 1024;
    private static final int CLIPBOARD_LIMIT = 2 * 1024 * 1024;
    private static final int RESPONSE_LIMIT = 8 * 1024 * 1024;
    private static final int INPUT_LIMIT = 4096;
    // Each provider authenticates only against its own API host.
    private static final Map<String, String> PROVIDERS = new HashMap<>();
    static {
        PROVIDERS.put("openai", "api.openai.com");
        PROVIDERS.put("anthropic", "api.anthropic.com");
    }
    static { System.loadLibrary("wordcraft_android"); }

    @Override protected void onCreate(Bundle state) {
        current = this;
        super.onCreate(state);
        getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        installWindowChrome();
        installPointerInput();
        sync.scheduleWithFixedDelay(this::flushSaves, 1, 1, TimeUnit.SECONDS);
        importIntent(getIntent());
    }

    // GameActivity supplies its live handle during onCreate. Its 4.4.0 public
    // getter returns zero while alive; passing that value to JNI dereferences null.
    // Replace the connection at the original binding point, before native input starts.
    @Override protected void setInputConnectionNative(long handle, InputConnection connection) {
        if (handle == 0) return;
        super.setInputConnectionNative(handle, editorConnection != null ? editorConnection : connection);
    }

    @Override protected InputEnabledSurfaceView createSurfaceView() {
        InputEnabledSurfaceView surface = new InputEnabledSurfaceView(this) {
            @Override public boolean onCheckIsTextEditor() { return true; }
            @Override public InputConnection onCreateInputConnection(EditorInfo out) {
                if (editorConnection == null || !editorConnection.getSoftKeyboardActive()) return null;
                if (out != null) GameTextInput.copyEditorInfo(editorConnection.getEditorInfo(), out);
                return editorConnection;
            }
        };
        EditorInfo info = getImeEditorInfo();
        info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE | InputType.TYPE_TEXT_FLAG_CAP_SENTENCES;
        info.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
        editorConnection = new EditorInputConnection(this, surface, info, new EditorInputConnection.Sink() {
            public boolean event(JSONObject event) { return queueInput(event); }
            public boolean key(KeyEvent event) { return routeKey(event); }
        });
        editorConnection.setListener(this);
        surface.setOnKeyListener(editorConnection);
        surface.setFocusable(true);
        surface.setFocusableInTouchMode(true);
        surface.requestFocus();
        return surface;
    }

    @Override public void setImeEditorInfo(EditorInfo info) {
        super.setImeEditorInfo(info);
        if (editorConnection != null) editorConnection.setEditorInfo(info);
    }
    @Override public void setImeEditorInfoFields(int inputType, int action, int options) {
        super.setImeEditorInfoFields(inputType, action, options);
        if (editorConnection != null) editorConnection.setEditorInfo(getImeEditorInfo());
    }
    @Override public boolean dispatchKeyEvent(KeyEvent event) {
        // Back (gesture, button or mouse) never finishes the activity: the editor decides whether
        // it closes a panel, a menu or sends the app to the background with the document intact.
        if (event.getKeyCode() == KeyEvent.KEYCODE_BACK) {
            if (event.getAction() == KeyEvent.ACTION_UP && !event.isCanceled()) {
                try { queueInput(new JSONObject().put("type", "back")); } catch (org.json.JSONException ignored) { }
            }
            return true;
        }
        return routeKey(event) || super.dispatchKeyEvent(event);
    }
    private boolean routeKey(KeyEvent event) {
        int code = event.getKeyCode();
        boolean handled = (code >= 7 && code <= 23) || (code >= 29 && code <= 77) || code == 81 || code == 92 || code == 93
            || (code >= 111 && code <= 124) || (code >= 131 && code <= 163);
        if (!handled) return false;
        boolean down = event.getAction() == KeyEvent.ACTION_DOWN;
        if (event.getAction() != KeyEvent.ACTION_DOWN && event.getAction() != KeyEvent.ACTION_UP) return false;
        int meta = event.getMetaState();
        boolean altgr = event.isCtrlPressed() && event.isAltPressed() && !event.isMetaPressed();
        int unicode = down ? event.getUnicodeChar(altgr ? meta & ~KeyEvent.META_CTRL_MASK : meta) : 0;
        String text = "";
        if ((unicode & KeyCharacterMap.COMBINING_ACCENT) != 0) {
            pendingAccent = unicode & KeyCharacterMap.COMBINING_ACCENT_MASK;
        } else if (unicode > 0 && Character.isValidCodePoint(unicode)) {
            if (pendingAccent != 0) {
                int combined = KeyCharacterMap.getDeadChar(pendingAccent, unicode);
                if (combined != 0) unicode = combined;
                else text = new String(Character.toChars(pendingAccent));
                pendingAccent = 0;
            }
            text += new String(Character.toChars(unicode));
        }
        try { queueInput(new JSONObject().put("type", "key").put("code", code).put("down", down)
            .put("repeat", event.getRepeatCount() > 0).put("meta", meta).put("text", text).put("altgr", altgr && !text.isEmpty())); }
        catch (org.json.JSONException ignored) { }
        // Wake the native event loop; Rust discards these duplicate native key events.
        if (down) super.onKeyDown(code, event); else super.onKeyUp(code, event);
        return true;
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
                post(new JSONObject().put("kind", "file").put("external", true).put("path", file.getAbsolutePath()));
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
        if (op.equals("input")) {
            JSONArray batch = new JSONArray();
            synchronized (inputEvents) {
                for (int i = 0; i < 1024; i++) {
                    JSONObject event = inputEvents.pollFirst();
                    if (event == null) break;
                    batch.put(event);
                }
            }
            return batch;
        }
        switch (op) {
            case "input_context": {
                boolean password = request.optBoolean("password");
                runOnUiThread(() -> {
                    if (editorConnection != null) {
                        editorConnection.resetContext(password);
                        ((android.view.inputmethod.InputMethodManager)getSystemService(INPUT_METHOD_SERVICE)).restartInput(mSurfaceView);
                    }
                });
                return new JSONObject();
            }
            case "keys": return keyFlags();
            case "chrome": {
                int top = request.optInt("top", Color.WHITE), bottom = request.optInt("bottom", top);
                boolean dark = request.optBoolean("dark");
                runOnUiThread(() -> applyChrome(top, bottom, dark));
                return new JSONObject();
            }
            case "minimize":
                runOnUiThread(() -> moveTaskToBack(true));
                return new JSONObject();
            case "clipboard_get": {
                ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
                ClipData clip = clipboard.getPrimaryClip();
                ClipData.Item item = clip == null || clip.getItemCount() == 0 ? null : clip.getItemAt(0);
                CharSequence plain = item == null ? null : item.getText();
                String text = plain == null ? "" : plain.toString();
                if (text.length() > CLIPBOARD_LIMIT) throw new IOException("Clipboard too large");
                return new JSONObject().put("text", text).put("html", item == null || item.getHtmlText() == null ? "" : item.getHtmlText());
            }
            case "clipboard_set": {
                String text = request.getString("text");
                String html = request.optString("html");
                if (text.length() > CLIPBOARD_LIMIT || html.length() > CLIPBOARD_LIMIT) throw new IOException("Clipboard too large");
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
            case "update_check": {
                // Only GitHub's public list of this app's releases: no key, nothing of the user's.
                String repo = request.getString("repo");
                if (!repo.matches("[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}") || repo.contains("..")) throw new IOException("Bad repository");
                long ticket = request.getLong("ticket");
                worker.execute(() -> updateCheck(ticket, repo));
                return new JSONObject();
            }
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
        if (!PROVIDERS.containsKey(id)) throw new IOException("Unknown provider");
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
        for (String id : PROVIDERS.keySet()) flags.put(id, getSharedPreferences("vault", MODE_PRIVATE).contains(id));
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
            if (cancelled.remove(request.getLong("ticket"))) throw new InterruptedIOException("Cancelled");
            String provider = provider(request);
            URL url = new URL(request.getString("url"));
            if (!url.getProtocol().equals("https") || url.getUserInfo() != null || url.getRef() != null
                || !url.getHost().equals(PROVIDERS.get(provider)) || (url.getPort() != -1 && url.getPort() != 443)) throw new IOException("HTTPS required");
            String value;
            try { value = key(provider); }
            catch (Exception e) { event.put("error", "missing_key"); return; }
            connection = (HttpsURLConnection) url.openConnection();
            requests.put(request.getLong("ticket"), connection);
            connection.setInstanceFollowRedirects(false);
            // Reasoning models can think for minutes before the first byte of a reply.
            connection.setConnectTimeout(20_000); connection.setReadTimeout(300_000);
            connection.setRequestMethod("POST"); connection.setDoOutput(true);
            connection.setRequestProperty("Content-Type", "application/json");
            if (provider.equals("anthropic")) {
                connection.setRequestProperty("x-api-key", value);
                connection.setRequestProperty("anthropic-version", "2023-06-01");
                String beta = request.optString("beta");
                if (beta.matches("[a-z0-9-]{1,64}(,[a-z0-9-]{1,64}){0,7}")) connection.setRequestProperty("anthropic-beta", beta);
            } else connection.setRequestProperty("Authorization", "Bearer " + value);
            byte[] bytes = request.getJSONObject("body").toString().getBytes(StandardCharsets.UTF_8);
            if (bytes.length > HTTP_LIMIT) throw new IOException("Request too large");
            connection.setFixedLengthStreamingMode(bytes.length);
            try (OutputStream out = connection.getOutputStream()) { out.write(bytes); }
            int status = connection.getResponseCode();
            event.put("status", status);
            String retry = connection.getHeaderField("retry-after");
            if (retry != null && retry.length() < 32) event.put("retry_after", retry);
            boolean ok = status >= 200 && status < 300;
            try (InputStream in = ok ? connection.getInputStream() : connection.getErrorStream()) {
                String text = in == null ? "" : new String(readLimited(in, RESPONSE_LIMIT), StandardCharsets.UTF_8);
                if (ok) event.put("body", new JSONObject(text));
                else event.put("detail", text.length() > 6000 ? text.substring(0, 6000) : text);
            }
        } catch (InterruptedIOException e) {
            try { event.put("error", e instanceof java.net.SocketTimeoutException ? "timeout" : "cancelled"); } catch (Exception ignored) { }
        } catch (java.net.UnknownHostException | java.net.ConnectException | javax.net.ssl.SSLException e) {
            try { event.put("error", "offline"); } catch (Exception ignored) { }
        } catch (Exception e) {
            try { event.put("error", "failed:" + e.getClass().getSimpleName()); } catch (Exception ignored) { }
        } finally {
            requests.remove(request.optLong("ticket")); cancelled.remove(request.optLong("ticket"));
            if (connection != null) connection.disconnect(); post(event);
        }
    }

    private void updateCheck(long ticket, String repo) {
        JSONObject event = new JSONObject();
        HttpsURLConnection connection = null;
        try {
            event.put("kind", "update").put("ticket", ticket);
            connection = (HttpsURLConnection) new URL("https://api.github.com/repos/" + repo + "/releases?per_page=20").openConnection();
            connection.setInstanceFollowRedirects(false);
            connection.setConnectTimeout(15_000); connection.setReadTimeout(30_000);
            connection.setRequestProperty("Accept", "application/vnd.github+json");
            connection.setRequestProperty("User-Agent", "craft-ia-updates");
            int status = connection.getResponseCode();
            event.put("status", status);
            if (status == 200) {
                try (InputStream in = connection.getInputStream()) {
                    event.put("body", new JSONArray(new String(readLimited(in, 4 * 1024 * 1024), StandardCharsets.UTF_8)));
                }
            }
        } catch (Exception e) {
            event.remove("status");
        } finally {
            if (connection != null) connection.disconnect();
            post(event);
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
            try { post(new JSONObject().put("kind", "file").put("ticket", request.optLong("ticket")).put("cancelled", true)); } catch (Exception ignored) { }
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
                post(event);
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
        try { post(new JSONObject().put("kind", "file").put("ticket", request.optLong("ticket")).put("error", message)); } catch (Exception ignored) { }
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
                post(new JSONObject().put("kind", "saved").put("exported", true));
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
    private void post(JSONObject event) {
        events.add(event);
        wake.release();
    }

    /** Called from the native waker thread: blocks until input or an event arrives (or timeout). */
    public static boolean awaitEvents(int millis) {
        try {
            boolean woke = wake.tryAcquire(Math.max(1, Math.min(millis, 5000)), TimeUnit.MILLISECONDS);
            wake.drainPermits();
            return woke;
        } catch (InterruptedException e) {
            return false;
        }
    }

    /** Queues an input event for Rust. Consecutive moves of the same gesture collapse into the
     *  latest one: their positions are absolute, so nothing is lost and a slow frame never
     *  backs the queue up behind stale moves. */
    private boolean queueInput(JSONObject event) {
        synchronized (inputEvents) {
            JSONObject last = inputEvents.peekLast();
            if (last != null && event.optBoolean("coalesce") && last.optBoolean("coalesce")
                && last.optInt("action") == event.optInt("action") && last.optInt("source") == event.optInt("source")
                && last.optInt("count") == event.optInt("count") && last.optInt("cls") == event.optInt("cls")
                && last.optInt("buttons") == event.optInt("buttons")) {
                inputEvents.pollLast();
            }
            if (inputEvents.size() >= INPUT_LIMIT) return false;
            inputEvents.addLast(event);
        }
        wake.release();
        return true;
    }

    /** Draw edge to edge on every Android version and pad the editor by the system bars, the
     *  display cutout and the keyboard. Android 15 enforces edge to edge for targetSdk 35, which
     *  otherwise leaves the status and navigation bars over the ribbon and the status bar. */
    private void installWindowChrome() {
        Window window = getWindow();
        WindowCompat.setDecorFitsSystemWindows(window, false);
        window.setStatusBarColor(Color.TRANSPARENT);
        window.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= 29) {
            window.setStatusBarContrastEnforced(false);
            window.setNavigationBarContrastEnforced(false);
        }
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams attributes = window.getAttributes();
            attributes.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(attributes);
        }
        if (mSurfaceView == null || !(mSurfaceView.getParent() instanceof View)) return;
        View container = (View) mSurfaceView.getParent();
        container.setBackground(bars);
        ViewCompat.setOnApplyWindowInsetsListener(container, (view, insets) -> {
            Insets system = insets.getInsets(WindowInsetsCompat.Type.systemBars() | WindowInsetsCompat.Type.displayCutout());
            Insets ime = insets.getInsets(WindowInsetsCompat.Type.ime());
            view.setPadding(system.left, system.top, system.right, Math.max(system.bottom, ime.bottom));
            bars.setTopInset(system.top);
            // Not consumed: GameActivity still reads the keyboard's visibility from these insets.
            return insets;
        });
        ViewCompat.requestApplyInsets(container);
    }

    private void applyChrome(int top, int bottom, boolean dark) {
        bars.setColors(top | 0xff000000, bottom | 0xff000000);
        getWindow().getDecorView().setBackgroundColor(bottom | 0xff000000);
        WindowInsetsControllerCompat controller = WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
        controller.setAppearanceLightStatusBars(!dark);
        controller.setAppearanceLightNavigationBars(!dark);
    }

    /** Touch, stylus, mouse and touchpad reach Rust through the input queue; Rust recognises
     *  taps, pans, long presses and pinches and turns them into editor events. GameActivity's
     *  own listeners forwarded raw touches, which egui read as multi-touch zoom (the sudden
     *  zooms) and as text selection instead of scrolling. */
    private void installPointerInput() {
        if (mSurfaceView == null) return;
        mSurfaceView.setOnTouchListener((view, event) -> queuePointer(event));
        mSurfaceView.setOnGenericMotionListener((view, event) -> queuePointer(event));
    }

    private boolean queuePointer(MotionEvent e) {
        try {
            int action = e.getActionMasked();
            int count = Math.min(e.getPointerCount(), 10);
            JSONArray pointers = new JSONArray();
            for (int i = 0; i < count; i++) {
                float x = e.getX(i), y = e.getY(i);
                if (!Float.isFinite(x) || !Float.isFinite(y)) return true;
                pointers.put(new JSONArray().put(e.getPointerId(i)).put((double) x).put((double) y).put(e.getToolType(i)));
            }
            int index = Math.min(e.getActionIndex(), Math.max(0, count - 1));
            int classification = Build.VERSION.SDK_INT >= 29 ? e.getClassification() : 0;
            JSONObject event = new JSONObject().put("type", "motion").put("action", action)
                .put("id", count > 0 ? e.getPointerId(index) : 0).put("source", e.getSource())
                .put("tool", count > 0 ? e.getToolType(index) : 0).put("buttons", e.getButtonState())
                .put("action_button", e.getActionButton()).put("meta", e.getMetaState())
                .put("time", e.getEventTime()).put("count", count).put("cls", classification).put("p", pointers);
            if (action == MotionEvent.ACTION_SCROLL) {
                float v = e.getAxisValue(MotionEvent.AXIS_VSCROLL), h = e.getAxisValue(MotionEvent.AXIS_HSCROLL);
                event.put("vs", Float.isFinite(v) ? v : 0.0).put("hs", Float.isFinite(h) ? h : 0.0);
            }
            if (Build.VERSION.SDK_INT >= 34 && classification == MotionEvent.CLASSIFICATION_PINCH) {
                float scale = e.getAxisValue(MotionEvent.AXIS_GESTURE_PINCH_SCALE_FACTOR);
                event.put("pinch", Float.isFinite(scale) ? scale : 1.0);
            }
            event.put("coalesce", (action == MotionEvent.ACTION_MOVE || action == MotionEvent.ACTION_HOVER_MOVE)
                && classification != 5);
            queueInput(event);
        } catch (org.json.JSONException ignored) { }
        return true;
    }

    /** The window's background behind the status bar (top) and the navigation bar (bottom). */
    private static final class BarsDrawable extends Drawable {
        private final Paint paint = new Paint();
        private int top = Color.WHITE, bottom = Color.WHITE, topInset;
        void setColors(int top, int bottom) { this.top = top; this.bottom = bottom; invalidateSelf(); }
        void setTopInset(int inset) { topInset = inset; invalidateSelf(); }
        @Override public void draw(Canvas canvas) {
            android.graphics.Rect r = getBounds();
            paint.setColor(bottom);
            canvas.drawRect(r, paint);
            paint.setColor(top);
            canvas.drawRect(r.left, r.top, r.right, r.top + Math.max(topInset, 0), paint);
        }
        @Override public void setAlpha(int alpha) { }
        @Override public void setColorFilter(ColorFilter filter) { }
        @Override public int getOpacity() { return PixelFormat.OPAQUE; }
    }

    private static JSONObject error(String message) {
        JSONObject value = new JSONObject(); try { value.put("error", message); } catch (Exception ignored) { } return value;
    }
}
