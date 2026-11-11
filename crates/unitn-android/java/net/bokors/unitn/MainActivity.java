package net.bokors.unitn;

import android.Manifest;
import android.app.NativeActivity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.View;

import java.util.ArrayList;

/**
 * The app's only screen. The UI is drawn by the native library
 * (libunitn_android.so); this class adds what needs Java: permission
 * questions, opening the browser and starting the playback service.
 * The methods are called from Rust (crates/unitn-android/src/lib.rs).
 */
public class MainActivity extends NativeActivity {
    static final String TAG = "unitn";

    // Event codes, as in crates/unitn/src/android.rs.
    static final int PLAY = 0, PAUSE = 1, TOGGLE = 2, NEXT = 3, PREVIOUS = 4, STOP = 5, SEEK = 6;
    static final int PERMISSIONS = 10;

    /** Implemented in Rust (registered when the native library starts). */
    static native void nativeCommand(int code, long arg);

    static void command(int code, long arg) {
        try {
            nativeCommand(code, arg);
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "native side not ready for event " + code);
        }
    }

    private final Handler main = new Handler(Looper.getMainLooper());

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        requestAppPermissions();
    }

    /** Asks for access to the music files and for notifications, if missing. */
    public void requestAppPermissions() {
        main.post(new Runnable() {
            @Override
            public void run() {
                ArrayList<String> wanted = new ArrayList<String>();
                if (Build.VERSION.SDK_INT >= 33) {
                    wanted.add(Manifest.permission.READ_MEDIA_AUDIO);
                    wanted.add(Manifest.permission.POST_NOTIFICATIONS);
                } else {
                    wanted.add(Manifest.permission.READ_EXTERNAL_STORAGE);
                }
                ArrayList<String> missing = new ArrayList<String>();
                for (String permission : wanted) {
                    if (checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
                        missing.add(permission);
                    }
                }
                if (!missing.isEmpty()) {
                    requestPermissions(missing.toArray(new String[0]), 1);
                }
            }
        });
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        command(PERMISSIONS, 0);
    }

    /** Dark icons in the status and navigation bars when the app's theme is light. */
    @SuppressWarnings("deprecation")
    public void setLightSystemBars(final boolean light) {
        main.post(new Runnable() {
            @Override
            public void run() {
                View decor = getWindow().getDecorView();
                int mask = View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                int flags = decor.getSystemUiVisibility();
                decor.setSystemUiVisibility(light ? (flags | mask) : (flags & ~mask));
                // The app's own background and tab bar colors (ui/theme.slint).
                getWindow().setStatusBarColor(light ? 0xFFF6F6F9 : 0xFF0F0F13);
                getWindow().setNavigationBarColor(light ? 0xFFEDEDF2 : 0xFF15151B);
            }
        });
    }

    /** Opens a web page in the browser (Plex sign-in). */
    public boolean openUrl(String url) {
        try {
            Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            startActivity(intent);
            return true;
        } catch (Exception e) {
            Log.w(TAG, "cannot open " + url + ": " + e);
            return false;
        }
    }

    /** Shows or updates the playback notification; starts the service that keeps the music playing. */
    public void updatePlayback(final String title, final String artist, final String album,
            final long positionMs, final long durationMs, final boolean playing) {
        main.post(new Runnable() {
            @Override
            public void run() {
                PlaybackService service = PlaybackService.instance;
                if (service != null) {
                    service.update(title, artist, album, positionMs, durationMs, playing);
                    return;
                }
                Intent intent = new Intent(MainActivity.this, PlaybackService.class)
                        .setAction(PlaybackService.ACTION_UPDATE)
                        .putExtra("title", title)
                        .putExtra("artist", artist)
                        .putExtra("album", album)
                        .putExtra("position", positionMs)
                        .putExtra("duration", durationMs)
                        .putExtra("playing", playing);
                try {
                    if (Build.VERSION.SDK_INT >= 26) {
                        startForegroundService(intent);
                    } else {
                        startService(intent);
                    }
                } catch (Exception e) {
                    Log.w(TAG, "cannot start the playback service: " + e);
                }
            }
        });
    }

    /** Nothing is playing any more: removes the notification. */
    public void stopPlayback() {
        main.post(new Runnable() {
            @Override
            public void run() {
                stopService(new Intent(MainActivity.this, PlaybackService.class));
            }
        });
    }

    /** Back on the first page leaves the app open in the background, so the music goes on. */
    @Override
    public void onBackPressed() {
        moveTaskToBack(true);
    }
}
