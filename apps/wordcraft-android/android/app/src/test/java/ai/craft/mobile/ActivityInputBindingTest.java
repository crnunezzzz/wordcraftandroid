package ai.craft.mobile;

import com.google.androidgamesdk.GameActivity;
import com.google.androidgamesdk.gametextinput.InputConnection;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;
import static org.junit.Assert.*;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 28, shadows = {ActivityInputBindingTest.NativeLibrary.class, ActivityInputBindingTest.NativeBinding.class})
public class ActivityInputBindingTest {
    @Implements(CraftActivity.class)
    public static class NativeLibrary extends NativeBinding {
        @Implementation protected static void __staticInitializer__() { }
    }
    @Implements(GameActivity.class)
    public static class NativeBinding extends org.robolectric.shadows.ShadowActivity {
        static long boundHandle;
        static InputConnection boundConnection;
        @Implementation protected void setInputConnectionNative(long handle, InputConnection connection) {
            boundHandle = handle;
            boundConnection = connection;
        }
    }
    @Test public void startupUsesSuppliedHandleAndEditorConnection() throws Exception {
        CraftActivity activity = Robolectric.buildActivity(CraftActivity.class).get();
        activity.createSurfaceView();
        java.lang.reflect.Field field = CraftActivity.class.getDeclaredField("editorConnection");
        field.setAccessible(true);
        InputConnection editor = (InputConnection) field.get(activity);
        assertNotNull(editor);
        NativeBinding.boundHandle = 0;
        activity.setInputConnectionNative(1234L, null);
        assertEquals(1234L, NativeBinding.boundHandle);
        assertSame(editor, NativeBinding.boundConnection);
        activity.setInputConnectionNative(0, null);
        assertEquals(1234L, NativeBinding.boundHandle);
        // A recreated activity must bind its own editor to the new native handle.
        CraftActivity recreated = Robolectric.buildActivity(CraftActivity.class).get();
        recreated.createSurfaceView();
        recreated.setInputConnectionNative(5678L, null);
        assertEquals(5678L, NativeBinding.boundHandle);
        assertSame(field.get(recreated), NativeBinding.boundConnection);
        assertNotSame(editor, NativeBinding.boundConnection);
    }
}
