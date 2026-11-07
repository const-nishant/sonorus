package dev.constnishant.sonorus

import android.content.ComponentName
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.media3.common.util.UnstableApi
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.ListenableFuture
import dev.constnishant.sonorus.playback.PlaybackService
import dev.constnishant.sonorus.ui.SonorusRoot
import dev.constnishant.sonorus.ui.theme.SonorusTheme

/** Entry activity. Hosts the Sonorus navigation shell. */
class MainActivity : ComponentActivity() {
    /**
     * Controller connection held while the activity is started. The UI uses the repository
     * directly; the connection only binds [PlaybackService] so playback can run in the background.
     */
    private var controllerFuture: ListenableFuture<MediaController>? = null

    override fun onCreate(savedInstanceState: Bundle?) {
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
    }

    override fun onStop() {
        controllerFuture?.let { MediaController.releaseFuture(it) }
        controllerFuture = null
        super.onStop()
    }
}
