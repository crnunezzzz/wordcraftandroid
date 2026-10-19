package ai.craft.mobile;

import android.content.Context;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.EditorInfo;
import androidx.core.graphics.Insets;
import com.google.androidgamesdk.gametextinput.Listener;
import com.google.androidgamesdk.gametextinput.State;
import org.json.JSONObject;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;
import java.util.ArrayList;
import java.util.List;
import static org.junit.Assert.*;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 28)
public class EditorInputConnectionTest {
    private static final class Sink implements EditorInputConnection.Sink {
        final List<JSONObject> events = new ArrayList<>();
        final List<KeyEvent> keys = new ArrayList<>();
        public boolean event(JSONObject event) { events.add(event); return true; }
        public boolean key(KeyEvent event) { keys.add(event); return true; }
    }
    private EditorInputConnection connection(Sink sink) {
        Context context = org.robolectric.Robolectric.buildActivity(android.app.Activity.class).setup().get();
        EditorInfo info = new EditorInfo();
        info.inputType = android.text.InputType.TYPE_CLASS_TEXT;
        EditorInputConnection input = new EditorInputConnection(context, new View(context), info, sink);
        input.setListener(new Listener() {
            public void stateChanged(State state, boolean dismissed) { }
            public void onImeInsetsChanged(Insets insets) { }
            public void onSoftwareKeyboardVisibilityChanged(boolean visible) { }
            public void onEditorAction(int action) { }
        });
        return input;
    }

    @Test public void composingSpanishCommitsOnlyOnce() {
        Sink sink = new Sink();
        EditorInputConnection input = connection(sink);
        input.setComposingText("espa", 1);
        input.setComposingText("español", 1);
        input.commitText("español", 1);
        input.finishComposingText();
        assertEquals(2, sink.events.stream().filter(e -> e.optString("type").equals("preedit")).count());
        assertEquals(1, sink.events.stream().filter(e -> e.optString("type").equals("commit")).count());
        assertEquals("español", sink.events.stream().filter(e -> e.optString("type").equals("commit")).findFirst().get().optString("text"));
    }

    @Test public void softwareDeletionWorksEvenAtAnEmptyNativeBuffer() {
        Sink sink = new Sink();
        EditorInputConnection input = connection(sink);
        input.deleteSurroundingText(1, 0);
        input.commitText("🙂a", 1);
        input.deleteSurroundingText(1, 0);
        input.deleteSurroundingTextInCodePoints(1, 0);
        List<JSONObject> deletes = new ArrayList<>();
        for (JSONObject e : sink.events) if (e.optString("type").equals("delete")) deletes.add(e);
        assertEquals(3, deletes.size());
        for (JSONObject e : deletes) assertEquals(1, e.optInt("before"));
    }

    @Test public void finishCompositionAndKeyboardActionsReachTheEditor() {
        Sink sink = new Sink();
        EditorInputConnection input = connection(sink);
        input.setComposingText("á", 1);
        input.finishComposingText();
        input.performEditorAction(EditorInfo.IME_ACTION_NEXT);
        input.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER));
        assertEquals(1, sink.events.stream().filter(e -> e.optString("type").equals("commit") && e.optString("text").equals("á")).count());
        assertEquals(3, sink.keys.size());
        assertEquals(KeyEvent.KEYCODE_TAB, sink.keys.get(0).getKeyCode());
        assertEquals(KeyEvent.ACTION_UP, sink.keys.get(1).getAction());
        assertEquals(KeyEvent.KEYCODE_ENTER, sink.keys.get(2).getKeyCode());
    }
    @Test public void switchingFieldsClearsKeyboardTextAndSetsPasswordPurpose() {
        Sink sink = new Sink();
        EditorInputConnection input = connection(sink);
        input.commitText("private-key", 1);
        input.resetContext(true);
        assertEquals("", input.getEditable().toString());
        assertEquals(android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD,
            input.getEditorInfo().inputType & android.text.InputType.TYPE_MASK_VARIATION);
        input.setComposingText("á", 1);
        input.resetContext(false);
        input.commitText("hola", 1);
        assertEquals("hola", input.getEditable().toString());
        assertEquals(2, sink.events.stream().filter(e -> e.optString("type").equals("commit")).count());
    }
}
