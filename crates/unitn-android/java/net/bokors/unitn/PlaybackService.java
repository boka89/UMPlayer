package net.bokors.unitn;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.drawable.Icon;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.util.Log;

/**
 * Keeps Unitn playing while it is in the background: a foreground service
 * with the playback notification, a media session (lock screen, headset and
 * Bluetooth buttons), audio focus (pause for calls) and a wake lock.
 * The audio itself is played by the native library.
 */
public class PlaybackService extends Service {
    static final String ACTION_UPDATE = "net.bokors.unitn.UPDATE";
    static final String ACTION_PREVIOUS = "net.bokors.unitn.PREVIOUS";
    static final String ACTION_TOGGLE = "net.bokors.unitn.TOGGLE";
    static final String ACTION_NEXT = "net.bokors.unitn.NEXT";
    static final String CHANNEL = "playback";
    static final int NOTIFICATION = 1;

    /** The running service, if any (main thread only). */
    static volatile PlaybackService instance;

    private MediaSession session;
    private PowerManager.WakeLock wakeLock;
    private AudioManager audio;
    private boolean hasFocus;
    private boolean noisyRegistered;
    private boolean playing;

    private final AudioManager.OnAudioFocusChangeListener focusListener = new AudioManager.OnAudioFocusChangeListener() {
        @Override
        public void onAudioFocusChange(int change) {
            // A call, another player, navigation prompts: pause.
            if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
                if (change == AudioManager.AUDIOFOCUS_LOSS) {
                    hasFocus = false;
                }
                MainActivity.command(MainActivity.PAUSE, 0);
            }
        }
    };

    /** Headphones unplugged or Bluetooth disconnected: pause instead of blaring from the speaker. */
    private final BroadcastReceiver noisyReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            MainActivity.command(MainActivity.PAUSE, 0);
        }
    };

    @Override
    public void onCreate() {
        super.onCreate();
        audio = (AudioManager) getSystemService(Context.AUDIO_SERVICE);
        NotificationManager notifications = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        NotificationChannel channel = new NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("What is playing, with play, pause and skip buttons");
        channel.setShowBadge(false);
        notifications.createNotificationChannel(channel);

        session = new MediaSession(this, "Unitn");
        session.setCallback(new MediaSession.Callback() {
            @Override
            public void onPlay() {
                MainActivity.command(MainActivity.PLAY, 0);
            }

            @Override
            public void onPause() {
                MainActivity.command(MainActivity.PAUSE, 0);
            }

            @Override
            public void onSkipToNext() {
                MainActivity.command(MainActivity.NEXT, 0);
            }

            @Override
            public void onSkipToPrevious() {
                MainActivity.command(MainActivity.PREVIOUS, 0);
            }

            @Override
            public void onStop() {
                MainActivity.command(MainActivity.PAUSE, 0);
            }

            @Override
            public void onSeekTo(long positionMs) {
                MainActivity.command(MainActivity.SEEK, positionMs);
            }
        });
        session.setSessionActivity(openApp());
        session.setActive(true);

        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "unitn:playback");
        wakeLock.setReferenceCounted(false);
        instance = this;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (ACTION_UPDATE.equals(action)) {
            update(intent.getStringExtra("title"), intent.getStringExtra("artist"), intent.getStringExtra("album"),
                    intent.getLongExtra("position", 0), intent.getLongExtra("duration", -1),
                    intent.getBooleanExtra("playing", false));
        } else if (ACTION_PREVIOUS.equals(action)) {
            MainActivity.command(MainActivity.PREVIOUS, 0);
        } else if (ACTION_TOGGLE.equals(action)) {
            MainActivity.command(MainActivity.TOGGLE, 0);
        } else if (ACTION_NEXT.equals(action)) {
            MainActivity.command(MainActivity.NEXT, 0);
        }
        return START_NOT_STICKY;
    }

    /** New song, play/pause or a seek. */
    void update(String title, String artist, String album, long positionMs, long durationMs, boolean playing) {
        this.playing = playing;
        if (title == null) title = "";
        if (artist == null) artist = "";
        if (album == null) album = "";

        MediaMetadata.Builder metadata = new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, artist)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, album);
        if (durationMs > 0) {
            metadata.putLong(MediaMetadata.METADATA_KEY_DURATION, durationMs);
        }
        session.setMetadata(metadata.build());

        long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                | PlaybackState.ACTION_SKIP_TO_NEXT | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_STOP;
        if (durationMs > 0) {
            actions |= PlaybackState.ACTION_SEEK_TO;
        }
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(actions)
                .setState(playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        Math.max(0, positionMs), playing ? 1f : 0f)
                .build());

        Notification notification = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.ic_media_play)
                .setContentTitle(title)
                .setContentText(artist)
                .setContentIntent(openApp())
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOnlyAlertOnce(true)
                .setShowWhen(false)
                .setOngoing(playing)
                .addAction(action(android.R.drawable.ic_media_previous, "Previous", ACTION_PREVIOUS))
                .addAction(action(playing ? android.R.drawable.ic_media_pause : android.R.drawable.ic_media_play,
                        playing ? "Pause" : "Play", ACTION_TOGGLE))
                .addAction(action(android.R.drawable.ic_media_next, "Next", ACTION_NEXT))
                .setStyle(new Notification.MediaStyle()
                        .setMediaSession(session.getSessionToken())
                        .setShowActionsInCompactView(0, 1, 2))
                .build();
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
            } else {
                startForeground(NOTIFICATION, notification);
            }
        } catch (Exception e) {
            Log.w(MainActivity.TAG, "cannot run in the foreground: " + e);
        }

        if (playing) {
            if (!hasFocus) {
                hasFocus = audio.requestAudioFocus(focusListener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN)
                        == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
            }
            if (!noisyRegistered) {
                IntentFilter noisy = new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY);
                if (Build.VERSION.SDK_INT >= 33) {
                    registerReceiver(noisyReceiver, noisy, Context.RECEIVER_NOT_EXPORTED);
                } else {
                    registerReceiver(noisyReceiver, noisy);
                }
                noisyRegistered = true;
            }
            if (!wakeLock.isHeld()) {
                wakeLock.acquire();
            }
        } else {
            if (noisyRegistered) {
                unregisterReceiver(noisyReceiver);
                noisyRegistered = false;
            }
            if (wakeLock.isHeld()) {
                wakeLock.release();
            }
        }
    }

    private Notification.Action action(int icon, String title, String action) {
        Intent intent = new Intent(this, PlaybackService.class).setAction(action);
        PendingIntent pending = PendingIntent.getService(this, 0, intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(Icon.createWithResource(this, icon), title, pending).build();
    }

    private PendingIntent openApp() {
        Intent intent = new Intent(this, MainActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(this, 0, intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    /** The app was swiped away from the recent apps: stop the music with it. */
    @Override
    public void onTaskRemoved(Intent rootIntent) {
        MainActivity.command(MainActivity.STOP, 0);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        instance = null;
        if (noisyRegistered) {
            unregisterReceiver(noisyReceiver);
            noisyRegistered = false;
        }
        if (hasFocus) {
            audio.abandonAudioFocus(focusListener);
            hasFocus = false;
        }
        if (wakeLock.isHeld()) {
            wakeLock.release();
        }
        session.setActive(false);
        session.release();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
