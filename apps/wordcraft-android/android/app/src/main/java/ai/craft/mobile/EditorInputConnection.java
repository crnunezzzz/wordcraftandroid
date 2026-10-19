package ai.craft.mobile;

import android.content.Context;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.CompletionInfo;
import android.view.inputmethod.EditorInfo;
import com.google.androidgamesdk.gametextinput.InputConnection;
import com.google.androidgamesdk.gametextinput.Settings;
import org.json.JSONObject;
import java.util.function.BooleanSupplier;

/** Android's regular IME connection, with explicit events for the Rust text editor. */
final class EditorInputConnection extends InputConnection {
    interface Sink {
        boolean event(JSONObject event);
        boolean key(KeyEvent event);
    }
    private final Sink sink;
    private boolean updating;
    private String composing = "";

    EditorInputConnection(Context context, View view, EditorInfo info, Sink sink) {
        super(context, view, new Settings(info, false));
        this.sink = sink;
    }

    private void emit(String type, String text) {
        try { sink.event(new JSONObject().put("type", type).put("text", text)); }
        catch (org.json.JSONException ignored) { }
    }
    private void delete(int before, int after) {
        try { sink.event(new JSONObject().put("type", "delete").put("before", Math.max(0, Math.min(8192, before))).put("after", Math.max(0, Math.min(8192, after)))); }
        catch (org.json.JSONException ignored) { }
    }
    private boolean update(BooleanSupplier action) {
        Editable editable = getEditable();
        if (Selection.getSelectionStart(editable) < 0 || Selection.getSelectionEnd(editable) < 0) {
            Selection.setSelection(editable, editable.length());
        }
        boolean previous = updating;
        updating = true;
        try { return action.getAsBoolean(); }
        finally { updating = previous; }
    }

    void resetContext(boolean password) {
        update(() -> {
            composing = "";
            super.finishComposingText();
            Editable editable = getEditable();
            editable.clear();
            BaseInputConnection.removeComposingSpans(editable);
            Selection.setSelection(editable, 0);
            return true;
        });
        EditorInfo info = getEditorInfo();
        info.inputType = InputType.TYPE_CLASS_TEXT | (password ? InputType.TYPE_TEXT_VARIATION_PASSWORD
            : InputType.TYPE_TEXT_FLAG_MULTI_LINE | InputType.TYPE_TEXT_FLAG_CAP_SENTENCES);
        info.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
        setEditorInfo(info);
    }

    @Override public boolean commitText(CharSequence text, int cursor) {
        if (updating) return super.commitText(text, cursor);
        composing = "";
        emit("commit", text.toString());
        return update(() -> super.commitText(text, cursor));
    }
    @Override public boolean setComposingText(CharSequence text, int cursor) {
        if (updating) return super.setComposingText(text, cursor);
        composing = text.toString();
        emit("preedit", composing);
        return update(() -> super.setComposingText(text, cursor));
    }
    @Override public boolean finishComposingText() {
        if (updating) return super.finishComposingText();
        if (!composing.isEmpty()) emit("commit", composing);
        else emit("cancel", "");
        composing = "";
        return update(() -> super.finishComposingText());
    }
    @Override public boolean setComposingRegion(int start, int end) {
        if (updating) return super.setComposingRegion(start, end);
        boolean hadComposition = !composing.isEmpty();
        boolean result = update(() -> super.setComposingRegion(start, end));
        composing = composition();
        if (!hadComposition && !composing.isEmpty()) delete(composing.codePointCount(0, composing.length()), 0);
        emit("preedit", composing);
        return result;
    }
    private String composition() {
        Editable editable = getEditable();
        int start = BaseInputConnection.getComposingSpanStart(editable);
        int end = BaseInputConnection.getComposingSpanEnd(editable);
        return start >= 0 && end >= start && end <= editable.length() ? editable.subSequence(start, end).toString() : "";
    }
    private int codePoints(int count, boolean before) {
        Editable text = getEditable();
        int cursor = Math.max(0, Math.min(text.length(), Selection.getSelectionEnd(text)));
        int start = before ? Math.max(0, cursor - Math.min(count, 8192)) : cursor;
        int end = before ? cursor : Math.min(text.length(), cursor + Math.min(count, 8192));
        int known = end - start;
        return Character.codePointCount(text, start, end) + Math.max(0, Math.min(count, 8192) - known);
    }
    @Override public boolean deleteSurroundingText(int before, int after) {
        if (updating) return super.deleteSurroundingText(before, after);
        int b = codePoints(Math.max(0, before), true), a = codePoints(Math.max(0, after), false);
        boolean hadComposition = !composing.isEmpty();
        boolean result = update(() -> super.deleteSurroundingText(Math.min(Math.max(0, before), 8192), Math.min(Math.max(0, after), 8192)));
        if (hadComposition) { composing = composition(); emit("preedit", composing); }
        else delete(b, a);
        return result;
    }
    @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
        if (updating) return super.deleteSurroundingTextInCodePoints(before, after);
        boolean hadComposition = !composing.isEmpty();
        boolean result = update(() -> super.deleteSurroundingTextInCodePoints(Math.min(Math.max(0, before), 8192), Math.min(Math.max(0, after), 8192)));
        if (hadComposition) { composing = composition(); emit("preedit", composing); }
        else delete(before, after);
        return result;
    }
    @Override public boolean sendKeyEvent(KeyEvent event) { return sink.key(event); }
    @Override public boolean onKey(View view, int keyCode, KeyEvent event) { return sink.key(event); }
    @Override public boolean commitCompletion(CompletionInfo completion) { return commitText(completion.getText(), 1); }
    @Override public boolean performEditorAction(int action) {
        int code = action == EditorInfo.IME_ACTION_NEXT ? KeyEvent.KEYCODE_TAB : KeyEvent.KEYCODE_ENTER;
        sink.key(new KeyEvent(KeyEvent.ACTION_DOWN, code));
        sink.key(new KeyEvent(KeyEvent.ACTION_UP, code));
        return true;
    }
}
