// "Share this screen": MediaProjection into a hardware H.264 encoder, run by a
// foreground service (Android requires one for screen capture). Every encoder
// output goes to Rust (crates/app/src/share.rs), which streams it to receivers.
// tools/build-android.sh copies this file into dx's Gradle project.
package dev.dioxus.main

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.Looper
import android.util.DisplayMetrics
import android.util.Log
import android.view.Display
import java.nio.ByteBuffer

/** Rust: crates/app/src/share.rs. */
object MullionNative {
    /** One encoder output buffer. Returns true when Rust wants a key frame next. */
    @JvmStatic external fun onH264(buf: ByteBuffer, offset: Int, size: Int, ptsUs: Long, flags: Int): Boolean

    /** 1 sharing at width x height, 0 stopped, -1 the person said no, -2 capture failed. */
    @JvmStatic external fun onShare(state: Int, width: Int, height: Int, dir: String)
}

class ShareService : Service() {
    private var projection: MediaProjection? = null
    private var codec: MediaCodec? = null
    private var display: VirtualDisplay? = null
    private var thread: HandlerThread? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) {
            stopNow()
            return START_NOT_STICKY
        }
        if (intent == null || projection != null) return START_NOT_STICKY
        // Android 14: the service must be in the foreground before it may
        // turn the consent into a MediaProjection.
        showNotification()
        try {
            begin(intent)
        } catch (e: Exception) {
            Log.e(TAG, "sharing could not start", e)
            stopNow()
            MullionNative.onShare(-2, 0, 0, filesDir.absolutePath)
        }
        return START_NOT_STICKY
    }

    private fun showNotification() {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Screen sharing", NotificationManager.IMPORTANCE_LOW))
        val stop = PendingIntent.getService(this, 0, Intent(this, ShareService::class.java).setAction(STOP), PendingIntent.FLAG_IMMUTABLE)
        val open = packageManager.getLaunchIntentForPackage(packageName)?.let { PendingIntent.getActivity(this, 1, it, PendingIntent.FLAG_IMMUTABLE) }
        val n = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setContentTitle("Mullion is sharing this screen")
            .setContentText("Anyone you paired can see it.")
            .setOngoing(true)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null as Icon?, "Stop sharing", stop).build())
            .build()
        if (Build.VERSION.SDK_INT >= 29) startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION) else startForeground(1, n)
    }

    private fun begin(intent: Intent) {
        val code = intent.getIntExtra("code", 0)
        @Suppress("DEPRECATION")
        val data = intent.getParcelableExtra<Intent>("data") ?: error("no consent")
        val p = getSystemService(MediaProjectionManager::class.java).getMediaProjection(code, data) ?: error("no projection")
        projection = p
        val t = HandlerThread("mullion-share").also { it.start() }
        thread = t
        val h = Handler(t.looper)
        // Android 14: a callback before the virtual display. The person can
        // also stop sharing from the system's own controls.
        p.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() { onMain { stopNow() } }
        }, h)

        // The real screen, long side at most 1920, in multiples of 16 (encoders want it).
        val m = DisplayMetrics()
        @Suppress("DEPRECATION")
        getSystemService(DisplayManager::class.java).getDisplay(Display.DEFAULT_DISPLAY).getRealMetrics(m)
        val scale = minOf(1.0, 1920.0 / maxOf(m.widthPixels, m.heightPixels))
        val w = (m.widthPixels * scale).toInt() / 16 * 16
        val ht = (m.heightPixels * scale).toInt() / 16 * 16

        val c = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        codec = c
        val cb = object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(c: MediaCodec, i: Int) {}
            override fun onOutputFormatChanged(c: MediaCodec, f: MediaFormat) {}
            override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
                Log.e(TAG, "encoder", e)
                onMain { stopNow() }
            }
            override fun onOutputBufferAvailable(c: MediaCodec, i: Int, info: MediaCodec.BufferInfo) {
                var key = false
                try {
                    val buf = c.getOutputBuffer(i)
                    if (buf != null && info.size > 0) key = MullionNative.onH264(buf, info.offset, info.size, info.presentationTimeUs, info.flags)
                    c.releaseOutputBuffer(i, false)
                    if (key) c.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
                } catch (e: IllegalStateException) {
                    // Stopping: the codec went away under this callback.
                }
            }
        }
        c.setCallback(cb, h)
        try {
            c.configure(format(w, ht, true), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (e: Exception) {
            // Some encoders refuse an explicit profile or low latency: take their defaults.
            c.reset()
            c.setCallback(cb, h)
            c.configure(format(w, ht, false), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        }
        val surface = c.createInputSurface()
        c.start()
        display = p.createVirtualDisplay("Mullion", w, ht, m.densityDpi, DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, surface, null, null)
        running = this
        MullionNative.onShare(1, w, ht, filesDir.absolutePath)
    }

    private fun format(w: Int, h: Int, tuned: Boolean) = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h).apply {
        setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
        setInteger(MediaFormat.KEY_BIT_RATE, 6_000_000)
        setInteger(MediaFormat.KEY_FRAME_RATE, 30)
        setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 10)
        // A still screen sends no frames; repeat the last one so a receiver
        // that joins (or loses a packet) still gets a picture.
        setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 100_000)
        if (tuned) {
            setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
            setInteger(MediaFormat.KEY_LEVEL, MediaCodecInfo.CodecProfileLevel.AVCLevel41)
            if (Build.VERSION.SDK_INT >= 30) setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        }
    }

    // Stopping waits on the encoder, whose callbacks run on `thread`: always stop from the main thread.
    private fun onMain(f: () -> Unit) { Handler(Looper.getMainLooper()).post(f) }

    /** Stop capture and encoding, leave the foreground, tell Rust. Main thread; safe to call twice. */
    @Synchronized
    fun stopNow() {
        val wasRunning = running != null
        running = null
        try { display?.release() } catch (_: Exception) {}
        try { codec?.stop() } catch (_: Exception) {}
        try { codec?.release() } catch (_: Exception) {}
        val p = projection
        display = null; codec = null; projection = null
        try { p?.stop() } catch (_: Exception) {}
        thread?.quitSafely(); thread = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        if (wasRunning) MullionNative.onShare(0, 0, 0, filesDir.absolutePath)
    }

    override fun onDestroy() {
        stopNow()
        super.onDestroy()
    }

    companion object {
        const val STOP = "app.mullion.tablet.STOP_SHARING"
        const val CHANNEL = "share"
        const val TAG = "Mullion"
        @Volatile var running: ShareService? = null
    }
}
