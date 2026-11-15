package dev.gpui.beeper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class FilesTest {
    private static final String PKG = "dev.gpui.beeper";

    @Test public void refusesRawPathsAndOwnProvider() {
        assertTrue(Files.refused("file", null, PKG));
        assertTrue(Files.refused("FILE", "", PKG));
        assertTrue(Files.refused(null, null, PKG));
        assertTrue(Files.refused("content", null, PKG));
        assertTrue(Files.refused("content", "dev.gpui.beeper.files", PKG));
        assertTrue(Files.refused("content", "0@dev.gpui.beeper.files", PKG));
        assertTrue(Files.refused("content", "Dev.Gpui.Beeper.Files", PKG));
        assertTrue(Files.refused("android.resource", PKG, PKG));
        assertFalse(Files.refused("content", "media", PKG));
        assertFalse(Files.refused("content", "0@media", PKG));
        assertFalse(Files.refused("content", "com.android.providers.downloads.documents", PKG));
        assertFalse(Files.refused("content", "dev.gpui.beeperx.files", PKG));
    }

    @Test public void findsOwnStorage() {
        assertTrue(Files.insideOwn("/data/data/dev.gpui.beeper/files/beeper/session", PKG));
        assertTrue(Files.insideOwn("/data/user/0/dev.gpui.beeper/cache/x.jpg", PKG));
        assertTrue(Files.insideOwn("/storage/emulated/0/Android/data/dev.gpui.beeper/files/a", PKG));
        assertFalse(Files.insideOwn("/storage/emulated/0/DCIM/Camera/IMG_1.jpg", PKG));
        assertFalse(Files.insideOwn("/data/data/dev.gpui.beeperx/files/a", PKG));
        assertFalse(Files.insideOwn("pipe:[1234]", PKG));
        assertFalse(Files.insideOwn(null, PKG));
    }

    @Test public void namesSayOnlyTheKind() {
        assertEquals("jpg", Files.extension("Screenshot_20250101_WhatsApp.JPG"));
        assertNull(Files.extension("no extension"));
        assertNull(Files.extension("weird.ex t"));
        assertNull(Files.extension(null));
        assertEquals("image.jpg", Files.neutralName("jpg", "image/jpeg"));
        assertEquals("video.mp4", Files.neutralName("mp4", "video/mp4"));
        assertEquals("file.pdf", Files.neutralName("pdf", "application/pdf"));
        assertEquals("file.m4a", Files.neutralName("m4a", "audio/mp4"));
        assertEquals("file", Files.neutralName(null, null));
        assertEquals("image", Files.neutralName(null, "image/heic"));
    }
}
