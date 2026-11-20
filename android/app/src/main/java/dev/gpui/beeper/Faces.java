package dev.gpui.beeper;

import android.graphics.Bitmap;
import android.graphics.PointF;
import android.media.FaceDetector;
import java.nio.ByteBuffer;

/**
 * Faces in a photo for the editor's "Blur faces", found on the device by Android's own detector
 * (the one Signal uses), which sees faces turned towards the camera. Called from Rust over JNI,
 * off the UI thread. Memory: the RGB 565 pixels as given and one bitmap of them.
 */
public final class Faces {
    private static final int MAX = 20;

    private Faces() {}

    /** Each face as its eyes' midpoint x, y and the distance between the eyes, in px of the {@code w} (even) by {@code h} RGB 565 pixels. */
    public static float[] find(byte[] rgb565, int w, int h) {
        Bitmap bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.RGB_565);
        try {
            bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(rgb565));
            FaceDetector.Face[] faces = new FaceDetector.Face[MAX];
            int found = new FaceDetector(w, h, MAX).findFaces(bitmap, faces);
            float[] out = new float[found * 3];
            PointF mid = new PointF();
            for (int i = 0; i < found; i++) {
                faces[i].getMidPoint(mid);
                out[i * 3] = mid.x;
                out[i * 3 + 1] = mid.y;
                out[i * 3 + 2] = faces[i].eyesDistance();
            }
            return out;
        } finally {
            bitmap.recycle();
        }
    }
}
