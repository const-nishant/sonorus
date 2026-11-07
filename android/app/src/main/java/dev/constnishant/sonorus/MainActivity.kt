package dev.constnishant.sonorus

import android.content.ComponentName
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.media3.common.util.UnstableApi
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.ListenableFuture
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.playback.PlaybackService
import dev.constnishant.sonorus.ui.SonorusRoot
import dev.constnishant.sonorus.ui.theme.SonorusTheme
import dev.constnishant.sonorus.upload.UploadService
import kotlinx.coroutines.launch

/** Entry activity. Hosts the Sonorus navigation shell. */
class MainActivity : ComponentActivity() {
    /**
     * Controller connection held while the activity is started. The UI uses the repository
     * directly; the connection only binds [PlaybackService] so playback can run in the background.
     */
    private var controllerFuture: ListenableFuture<MediaController>? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        installSplashScreen()
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            SonorusTheme {
                SonorusRoot()
            }
        }
    }

    @UnstableApi
    override fun onStart() {
        super.onStart()
        val token = SessionToken(this, ComponentName(this, PlaybackService::class.java))
        controllerFuture = MediaController.Builder(this, token).buildAsync()
        resumeUploads()
    }

    /**
     * Restarts the upload service when the queue has work left, for example after the process was
     * killed mid-upload. Starting is only allowed while the activity is in the foreground.
     */
    private fun resumeUploads() {
        lifecycleScope.launch {
            val pending = repository.hasPendingUploads()
            if (pending && lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) {
                UploadService.start(this@MainActivity)
            }
        }
    }

    override fun onStop() {
        controllerFuture?.let { MediaController.releaseFuture(it) }
        controllerFuture = null
        super.onStop()
    }
}
