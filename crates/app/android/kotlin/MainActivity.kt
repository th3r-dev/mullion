// Mullion's MainActivity: dx's generated one (an empty WryActivity) plus
// "Share this screen". tools/build-android.sh puts this file over dx's.
package dev.dioxus.main

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.os.Build

typealias BuildConfig = app.mullion.tablet.BuildConfig

class MainActivity : WryActivity() {
    /** Called from Rust (share.rs) on this thread: ask Android for consent. */
    fun startShare() {
        // The "Stop sharing" notification needs this on Android 13+; sharing works without it.
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        val mpm = getSystemService(MediaProjectionManager::class.java)
        @Suppress("DEPRECATION")
        startActivityForResult(mpm.createScreenCaptureIntent(), SHARE_REQUEST)
    }

    /** Called from Rust (share.rs). */
    fun stopShare() {
        ShareService.running?.stopNow() ?: MullionNative.onShare(0, 0, 0, filesDir.absolutePath)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != SHARE_REQUEST) return
        if (resultCode != Activity.RESULT_OK || data == null) {
            MullionNative.onShare(-1, 0, 0, filesDir.absolutePath)
            return
        }
        val i = Intent(this, ShareService::class.java).putExtra("code", resultCode).putExtra("data", data)
        startForegroundService(i)
    }

    companion object {
        const val SHARE_REQUEST = 4710
    }
}
