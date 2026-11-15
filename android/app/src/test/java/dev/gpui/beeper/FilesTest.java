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

    @Test public void photosAndVideosSayOnlyTheKind() {
        assertEquals("jpg", Files.extension("Screenshot_20250101_WhatsApp.JPG"));
        assertNull(Files.extension("no extension"));
        assertNull(Files.extension("weird.ex t"));
        assertNull(Files.extension(null));
        assertEquals("image.jpg", Files.uploadName("IMG_20250101_120000.jpg", "jpg", "image/jpeg"));
        assertEquals("video.mp4", Files.uploadName("VID_20250101_120000.mp4", "mp4", "video/mp4"));
        assertEquals("image", Files.uploadName(null, null, "image/heic"));
    }

    @Test public void documentsKeepTheirNameMadeSafe() {
        assertEquals("Umowa najmu.pdf", Files.uploadName("Umowa najmu.pdf", "pdf", "application/pdf"));
        assertEquals("Nagranie.m4a", Files.uploadName("Nagranie.m4a", "m4a", "audio/mp4"));
        assertEquals(".._.._etc_passwd", Files.uploadName("../../etc/passwd", null, null));
        assertEquals("a_b_c.txt", Files.uploadName("a\\b\nc.txt", "txt", "text/plain"));
        assertEquals("invoice_fdp.exe", Files.uploadName("invoice\u202efdp.exe", "exe", null));
        assertEquals("file.pdf", Files.uploadName("  ", "pdf", "application/pdf"));
        assertEquals("file", Files.uploadName("..", null, null));
        assertEquals("file", Files.uploadName(null, null, null));
        String longName = "\u017c".repeat(300) + ".pdf";
        String cut = Files.uploadName(longName, "pdf", "application/pdf");
        assertTrue(cut.endsWith(".pdf"));
        assertTrue(cut.getBytes(java.nio.charset.StandardCharsets.UTF_8).length <= Files.MAX_NAME_BYTES);
        String emoji = Files.uploadName("\ud83d\ude00".repeat(100) + ".txt", "txt", null);
        assertTrue(emoji.getBytes(java.nio.charset.StandardCharsets.UTF_8).length <= Files.MAX_NAME_BYTES);
        assertEquals(0, emoji.codePoints().filter(c -> c == 0xfffd || Character.isSurrogate((char) c)).count());
    }
}
