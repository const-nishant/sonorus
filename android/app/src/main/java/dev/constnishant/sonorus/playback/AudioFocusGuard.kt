package dev.constnishant.sonorus.playback

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.core.content.ContextCompat
import dev.constnishant.sonorus.core.PlaybackState
import dev.constnishant.sonorus.core.SonorusCore
import dev.constnishant.sonorus.data.PlaybackUi
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

private const val TAG = "Sonorus"

/** How long focus is kept after the user pauses, so a quick resume does not release and re-take it. */
private const val ABANDON_DELAY_MS = 30_000L

/**
 * Audio focus and headphone-unplug handling for the core's playback. It watches the core's state
 * rather than the media session, so it also covers play started from the app's own screens.
 *
 * While the core plays it holds audio focus and listens for [AudioManager.ACTION_AUDIO_BECOMING_NOISY].
 * Loss of focus pauses playback; a transient loss remembers to resume on gain. A transient loss that
 * allows ducking is left to the system. After the user pauses, focus is released [ABANDON_DELAY_MS]
 * later unless a resume is pending.
 *
 * All methods run on the main thread (state is collected there, and focus callbacks are delivered on
 * the main looper).
 *
 * @param playback the core's playback state.
 * @param send runs a core command, fire-and-forget (the repository's `command`).
 */
class AudioFocusGuard(
    context: Context,
    playback: StateFlow<PlaybackUi>,
    private val send: (block: (SonorusCore) -> Unit) -> Unit,
) {
    private val appContext = context.applicationContext
    private val audioManager = appContext.getSystemService(AudioManager::class.java)
    private val handler = Handler(Looper.getMainLooper())
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** The focus grant held, or null when focus is not held. */
    private var request: AudioFocusRequest? = null

    /** True while the core is playing or about to play. */
    private var playing = false

    /** Set when playback was paused by a transient focus loss; a gain resumes it. */
    private var resumeOnGain = false

    /** True while the becoming-noisy receiver is registered. */
    private var noisyRegistered = false

    private val abandonLater = Runnable { abandon("paused for ${ABANDON_DELAY_MS / 1000} s") }

    private val focusListener = AudioManager.OnAudioFocusChangeListener { change -> onFocusChange(change) }

    private val noisyReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) {
                Log.i(TAG, "noisy: output became noisy, pausing")
                send { it.pause() }
            }
        }
    }

    init {
        scope.launch {
            playback.map { it.state }.distinctUntilChanged().collect { onState(it) }
        }
    }

    private fun onState(state: PlaybackState) {
        playing = state.wantsPlay()
        if (playing) {
            // A new play by the user replaces a pending resume.
            resumeOnGain = false
            handler.removeCallbacks(abandonLater)
            registerNoisy()
            if (request == null) requestFocus()
        } else {
            unregisterNoisy()
            if (request != null && !resumeOnGain) {
                Log.i(TAG, "focus: paused, keeping focus for ${ABANDON_DELAY_MS / 1000} s")
                handler.removeCallbacks(abandonLater)
                handler.postDelayed(abandonLater, ABANDON_DELAY_MS)
            }
        }
    }

    private fun requestFocus() {
        val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build(),
            )
            .setOnAudioFocusChangeListener(focusListener, handler)
            .setWillPauseWhenDucked(false)
            .build()
        if (audioManager.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            request = focus
            Log.i(TAG, "focus: granted")
        } else {
            Log.i(TAG, "focus: denied, pausing")
            send { it.pause() }
        }
    }

    private fun onFocusChange(change: Int) {
        when (change) {
            AudioManager.AUDIOFOCUS_GAIN -> {
                if (resumeOnGain) {
                    resumeOnGain = false
                    Log.i(TAG, "focus: gained, resuming")
                    send { it.play() }
                } else {
                    Log.i(TAG, "focus: gained")
                }
            }
            AudioManager.AUDIOFOCUS_LOSS -> {
                Log.i(TAG, "focus: lost, pausing and releasing focus")
                send { it.pause() }
                abandon("focus lost")
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                if (playing) {
                    Log.i(TAG, "focus: transient loss, pausing until gained")
                    resumeOnGain = true
                    send { it.pause() }
                } else {
                    Log.i(TAG, "focus: transient loss while not playing")
                }
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK ->
                Log.i(TAG, "focus: transient loss, system may duck")
            else -> Log.i(TAG, "focus: change $change ignored")
        }
    }

    /** Releases focus, if held, and cancels a pending release. */
    private fun abandon(reason: String) {
        handler.removeCallbacks(abandonLater)
        resumeOnGain = false
        val held = request ?: return
        request = null
        val ok = audioManager.abandonAudioFocusRequest(held) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        Log.i(TAG, "focus: abandoned ($reason, ok=$ok)")
    }

    private fun registerNoisy() {
        if (noisyRegistered) return
        // Broadcast from the system; it is an exported receiver for the platform's protected action.
        ContextCompat.registerReceiver(
            appContext,
            noisyReceiver,
            IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
            ContextCompat.RECEIVER_EXPORTED,
        )
        noisyRegistered = true
    }

    private fun unregisterNoisy() {
        if (!noisyRegistered) return
        appContext.unregisterReceiver(noisyReceiver)
        noisyRegistered = false
    }
}
