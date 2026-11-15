package dev.gpui.mobile;

import android.app.NativeActivity;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.text.TextWatcher;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputConnectionWrapper;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;

/** NativeActivity with a UI-thread InputConnection for multistage IMEs. */
public class GpuiInputActivity extends NativeActivity {
    // Context kept before the caret, in UTF-16 units: src/ime.rs sends 512.
    private static final int TRIM_AT = 1024;
    private static final int REFILL_BELOW = 64;

    private InputProxy input;
    private int keyboardType;
    // The followed field (src/ime.rs) as last published; null while none has focus.
    private Field latest;
    // Last mirror edit sent; Rust publishes the last one it applied.
    private long seq;

    private static final class Field {
        final long id, applied;
        final int start, selStart, selEnd, length;
        final String text;

        Field(long id, long applied, int start, int selStart, int selEnd, int length, String text) {
            this.id = id;
            this.applied = applied;
            this.start = start;
            this.selStart = selStart;
            this.selEnd = selEnd;
            this.length = length;
            this.text = text;
        }
    }

    @Override protected void onCreate(Bundle state) {
        // NativeActivity's dlopen alone does not register JNI native methods.
        try {
            String library = getPackageManager().getActivityInfo(getComponentName(),
                    PackageManager.GET_META_DATA).metaData.getString("android.app.lib_name");
            if (library != null) System.loadLibrary(library);
        } catch (PackageManager.NameNotFoundException error) {
            throw new IllegalStateException(error);
        }
        super.onCreate(state);
    }

    public void gpuiShowKeyboard(int keyboardType, long session) {
        runOnUiThread(() -> {
            if (input == null) {
                input = new InputProxy();
                input.setAlpha(0f);
                input.setPadding(0, 0, 0, 0);
                // The proxy is invisible and holds message text: keep autofill services out of it.
                input.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
                addContentView(input, new ViewGroup.LayoutParams(1, 1));
            }
            this.keyboardType = keyboardType;
            input.reset(session);
            if (keyboardType == 0 && latest != null) input.seed(latest);
            input.shown = true;
            configure();
            input.requestFocus();
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            imm.restartInput(input);
            imm.showSoftInput(input, InputMethodManager.SHOW_IMPLICIT);
            // A recreated Activity has no copy of the field yet.
            if (keyboardType == 0 && latest == null) nativeImeResync();
        });
    }

    private void configure() {
        int type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE | InputType.TYPE_TEXT_FLAG_AUTO_CORRECT;
        // Sentence capitals need the text before the caret, which only a mirrored field has.
        if (input.mirror) type |= InputType.TYPE_TEXT_FLAG_CAP_SENTENCES;
        switch (keyboardType) {
            case 1: type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS; break;
            case 2: type = InputType.TYPE_CLASS_PHONE; break;
            case 3: type = InputType.TYPE_CLASS_NUMBER; break;
            // Murmur maps its secrets (recovery keys, codes, passwords, cookies) to 4: no suggestions, nothing learned.
            case 4: type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                    | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS; break;
            case 5: type = InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL; break;
        }
        input.setInputType(type);
        // Incognito keyboard for every field: the IME must not learn from what is typed here.
        input.setImeOptions(EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING);
    }

    public void gpuiHideKeyboard(long session) {
        runOnUiThread(() -> {
            if (input == null) return;
            input.reset(session);
            input.shown = false;
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            imm.hideSoftInputFromWindow(input.getWindowToken(), 0);
            input.clearFocus();
        });
    }

    public void gpuiResetComposition(long session) {
        runOnUiThread(() -> {
            if (input == null) return;
            input.reset(session);
            if (keyboardType == 0 && latest != null && input.shown) {
                input.seed(latest);
                configure();
            }
            ((InputMethodManager) getSystemService(INPUT_METHOD_SERVICE)).restartInput(input);
        });
    }

    /** Called by src/ime.rs on the GPUI thread; field 0 means no followed field has focus. */
    public void gpuiImeSync(long field, long applied, int start, int selStart, int selEnd, int length, String text) {
        Field next = field == 0 ? null : new Field(field, applied, start, selStart, selEnd, length, text);
        runOnUiThread(() -> {
            latest = next;
            if (input != null) input.sync();
        });
    }

    private final class InputProxy extends EditText {
        private long session;
        // Bumped when the proxy is reseeded, so the replaced InputConnection goes stale.
        private int generation;
        private int depth;
        private boolean marked;
        boolean shown;
        // Mirror mode: the proxy holds the followed field's text from UTF-16 offset `base` on, so
        // the IME sees the words around the caret. Otherwise every commit is forwarded and cleared.
        boolean mirror;
        private long field;
        private int base;
        private boolean syncLater;
        // The proxy as Rust was last told.
        private String sentText = "";
        private int sentStart, sentEnd, sentComp0 = -1, sentComp1 = -1;

        InputProxy() {
            super(GpuiInputActivity.this);
            addTextChangedListener(new TextWatcher() {
                public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
                public void onTextChanged(CharSequence s, int start, int before, int count) {}
                public void afterTextChanged(Editable text) {
                    // Hardware keyboards edit the widget directly, outside its
                    // InputConnection. IME mutations are batched by depth below.
                    if (depth == 0) { depth++; endEdit(); }
                }
            });
        }

        @Override public boolean onKeyDown(int code, KeyEvent event) {
            if (code == KeyEvent.KEYCODE_DEL && forwardDelete()) {
                nativeIme(session, 3, "", 1, 0);
                return true;
            }
            return super.onKeyDown(code, event);
        }

        /** Whether a deletion before the caret must go to the field because the proxy has nothing there. */
        private boolean forwardDelete() {
            Editable text = getText();
            if (!mirror) return text.length() == 0 && !marked;
            return base > 0 && Selection.getSelectionEnd(text) == 0 && BaseInputConnection.getComposingSpanStart(text) < 0;
        }

        void reset(long nextSession) {
            // The field keeps no composition the keyboard has stopped showing.
            if (mirror && sentComp0 >= 0) send(-1, -1, null, -1, -1, -1, -1);
            depth++;
            getText().clear();
            marked = false;
            mirror = false;
            syncLater = false;
            session = nextSession;
            depth = 0;
        }

        /** Mirror the field as published; the caller restarts the input. */
        void seed(Field f) {
            depth++;
            Editable text = getText();
            BaseInputConnection.removeComposingSpans(text);
            text.replace(0, text.length(), f.text);
            int s = Math.max(0, Math.min(f.selStart - f.start, f.text.length()));
            int e = Math.max(s, Math.min(f.selEnd - f.start, f.text.length()));
            Selection.setSelection(text, s, e);
            depth--;
            mirror = true;
            marked = false;
            field = f.id;
            base = f.start;
            seq = f.applied;
            remember(f.text, s, e, -1, -1);
        }

        private void restart() {
            generation++;
            ((InputMethodManager) getSystemService(INPUT_METHOD_SERVICE)).restartInput(this);
        }

        /** The field changed: follow it unless the change is the echo of edits still on their way. */
        void sync() {
            Field f = latest;
            if (f == null || !shown || keyboardType != 0) return;
            if (depth > 0) { syncLater = true; return; }
            if (mirror && f.id == field && f.applied != seq) return;
            Editable text = getText();
            boolean composing = BaseInputConnection.getComposingSpanStart(text) >= 0;
            if (!mirror) {
                // Switch to mirroring once nothing is composed through the forwarding path.
                if (composing || marked) return;
                seed(f);
                configure();
                restart();
                return;
            }
            if (f.id == field && agrees(f)) {
                int s = f.selStart - base, e = f.selEnd - base;
                boolean moved = s != Selection.getSelectionStart(text) || e != Selection.getSelectionEnd(text);
                boolean bounded = s <= TRIM_AT && (base == 0 || s >= REFILL_BELOW);
                // Trimming or refilling the window waits for the word being composed.
                if (!moved && (bounded || composing)) return;
                if (moved && bounded) {
                    // The app moved the caret: the selection change reaches the IME as in any EditText.
                    depth++;
                    BaseInputConnection.removeComposingSpans(text);
                    Selection.setSelection(text, s, e);
                    depth--;
                    flush();
                    return;
                }
            }
            boolean unmark = sentComp0 >= 0;
            seed(f);
            restart();
            if (unmark) send(-1, -1, null, -1, -1, -1, -1);
        }

        /** Whether the proxy holds the published text where both have it, and no text past the field's end. */
        private boolean agrees(Field f) {
            String now = getText().toString();
            int end = base + now.length(), fEnd = f.start + f.text.length();
            int lo = Math.max(base, f.start), hi = Math.min(end, fEnd);
            return hi >= lo && end >= fEnd && end <= f.length && f.selStart >= base && f.selEnd <= end
                    && now.regionMatches(lo - base, f.text, lo - f.start, hi - lo);
        }

        private void remember(String text, int s, int e, int c0, int c1) {
            sentText = text;
            sentStart = s;
            sentEnd = e;
            sentComp0 = c0;
            sentComp1 = c1;
        }

        /** Send what changed since the last edit as one replacement, in field offsets. */
        private void flush() {
            Editable text = getText();
            String now = text.toString();
            int s = Math.max(0, Selection.getSelectionStart(text)), e = Math.max(0, Selection.getSelectionEnd(text));
            if (s > e) { int t = s; s = e; e = t; }
            int c0 = BaseInputConnection.getComposingSpanStart(text), c1 = BaseInputConnection.getComposingSpanEnd(text);
            if (c0 > c1) { int t = c0; c0 = c1; c1 = t; }
            if (c0 < 0 || c0 == c1) { c0 = -1; c1 = -1; }
            if (now.equals(sentText) && s == sentStart && e == sentEnd && c0 == sentComp0 && c1 == sentComp1) return;
            int n = Math.min(now.length(), sentText.length());
            int a = 0;
            while (a < n && now.charAt(a) == sentText.charAt(a)) a++;
            if (a > 0 && Character.isHighSurrogate(now.charAt(a - 1))) a--;
            int z = 0;
            while (z < n - a && now.charAt(now.length() - 1 - z) == sentText.charAt(sentText.length() - 1 - z)) z++;
            if (z > 0 && Character.isLowSurrogate(now.charAt(now.length() - z))) z--;
            int oldEnd = sentText.length() - z, newEnd = now.length() - z;
            String insert = null;
            if (a < oldEnd || a < newEnd) {
                // Inside the composing word, send the whole word: Rust replaces its marked text in one step.
                if (c0 >= 0 && c0 <= a && newEnd <= c1) { oldEnd += c1 - newEnd; a = c0; newEnd = c1; }
                insert = now.substring(a, newEnd);
            }
            remember(now, s, e, c0, c1);
            send(insert == null ? -1 : a, oldEnd, insert, s, e, c0, c1);
        }

        private void send(int start, int end, String insert, int s, int e, int c0, int c1) {
            long next = seq + 1;
            if (nativeImeEdit(field, next, start < 0 ? -1 : base + start, base + end, insert,
                    s < 0 ? -1 : base + s, base + e,
                    c0 < 0 ? -1 : base + c0, c0 < 0 ? -1 : base + c1)) {
                seq = next;
            } else {
                // Rust's queue is full: its own state, published next, replaces this copy.
                nativeImeResync();
            }
        }

        private void endEdit() {
            if (--depth != 0) return;
            if (mirror) {
                flush();
            } else {
                Editable text = getText();
                boolean composing = BaseInputConnection.getComposingSpanStart(text) >= 0;
                if (composing || marked || text.length() > 0) {
                    nativeIme(session, composing ? 0 : 1, text.toString(),
                            Math.max(0, Selection.getSelectionStart(text)),
                            Math.max(0, Selection.getSelectionEnd(text)));
                    marked = composing;
                    if (!composing) {
                        depth++;
                        text.clear();
                        depth--;
                    }
                }
            }
            if (syncLater) {
                syncLater = false;
                sync();
            }
        }

        @Override public boolean onKeyPreIme(int code, KeyEvent event) {
            if (code == KeyEvent.KEYCODE_BACK && event.getAction() == KeyEvent.ACTION_UP) {
                nativeIme(session, 4, "", 0, 0);
            }
            return super.onKeyPreIme(code, event);
        }

        @Override public InputConnection onCreateInputConnection(EditorInfo info) {
            InputConnection connection = super.onCreateInputConnection(info);
            if (connection == null) return null;
            final long connectionSession = session;
            final int connectionGeneration = generation;
            return new InputConnectionWrapper(connection, false) {
                private boolean stale() {
                    return connectionSession != session || connectionGeneration != generation;
                }
                @Override public boolean beginBatchEdit() {
                    if (stale()) return false;
                    depth++;
                    return super.beginBatchEdit();
                }
                @Override public boolean endBatchEdit() {
                    if (stale()) return false;
                    boolean result = super.endBatchEdit();
                    if (depth > 0) endEdit();
                    return result;
                }
                @Override public boolean setComposingText(CharSequence text, int cursor) {
                    if (stale()) return false;
                    depth++;
                    try { return super.setComposingText(text, cursor); }
                    finally { endEdit(); }
                }
                @Override public boolean setComposingRegion(int start, int end) {
                    if (stale()) return false;
                    depth++;
                    try { return super.setComposingRegion(start, end); }
                    finally { endEdit(); }
                }
                @Override public boolean finishComposingText() {
                    if (stale()) return false;
                    depth++;
                    try { return super.finishComposingText(); }
                    finally { endEdit(); }
                }
                @Override public boolean commitText(CharSequence text, int cursor) {
                    if (stale()) return false;
                    depth++;
                    try { return super.commitText(text, cursor); }
                    finally { endEdit(); }
                }
                @Override public boolean setSelection(int start, int end) {
                    if (stale()) return false;
                    depth++;
                    try { return super.setSelection(start, end); }
                    finally { endEdit(); }
                }
                @Override public boolean deleteSurroundingText(int before, int after) {
                    if (stale()) return false;
                    if (forwardDelete()) {
                        nativeIme(session, 2, "", before, after);
                        return true;
                    }
                    depth++;
                    try { return super.deleteSurroundingText(before, after); }
                    finally { endEdit(); }
                }
                @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
                    if (stale()) return false;
                    if (forwardDelete()) {
                        nativeIme(session, 3, "", before, after);
                        return true;
                    }
                    depth++;
                    try { return super.deleteSurroundingTextInCodePoints(before, after); }
                    finally { endEdit(); }
                }
                @Override public boolean sendKeyEvent(KeyEvent event) {
                    if (stale()) return false;
                    if (event.getKeyCode() == KeyEvent.KEYCODE_DEL) {
                        if (event.getAction() == KeyEvent.ACTION_DOWN) deleteSurroundingText(1, 0);
                        return true;
                    }
                    if (event.getKeyCode() == KeyEvent.KEYCODE_ENTER) {
                        if (event.getAction() == KeyEvent.ACTION_DOWN) commitText("\n", 1);
                        return true;
                    }
                    return super.sendKeyEvent(event);
                }
                @Override public boolean performEditorAction(int action) {
                    if (stale()) return false;
                    if (action == EditorInfo.IME_ACTION_DONE) {
                        finishComposingText();
                        nativeIme(session, 4, "", 0, 0);
                        return true;
                    }
                    return commitText("\n", 1);
                }
            };
        }
    }

    private static native void nativeIme(long session, int kind, String text, int start, int end);
    // src/ime.rs: one proxy edit in field offsets (start -1: no text change, selStart -1: keep it, c0 -1: nothing composed).
    private static native boolean nativeImeEdit(long field, long seq, int start, int end, String text,
            int selStart, int selEnd, int c0, int c1);
    // src/ime.rs: publish the focused followed field again.
    private static native void nativeImeResync();
}
