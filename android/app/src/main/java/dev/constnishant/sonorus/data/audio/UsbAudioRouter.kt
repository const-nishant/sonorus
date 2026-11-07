package dev.constnishant.sonorus.data.audio

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioMixerAttributes
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import dev.constnishant.sonorus.core.OutputRouter
import dev.constnishant.sonorus.core.OutputSampleFormat
import dev.constnishant.sonorus.core.RoutePlanInfo
import dev.constnishant.sonorus.core.RouteRequestInfo
import dev.constnishant.sonorus.core.SignalPathInfo
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

private const val TAG = "Sonorus"

/** A USB audio output (DAC or USB headset) that bit-perfect audio can be sent to. */
data class UsbDac(val id: Int, val name: String)

/**
 * Output router for the Rust core. For each stream open it decides whether the stream can go to
 * a connected USB DAC bit-perfect, by setting a preferred mixer attribute on the DAC, or falls
 * back to the default output. It also tracks the connected DAC ([dac]) and the last open result
 * ([signalPath]).
 *
 * [plan] and [opened] are called by the core on background threads, possibly two at once. Shared
 * state is guarded by [lock]. [plan] never throws: any failure falls back to the default output.
 *
 * @param bitPerfectEnabled reads the user's bit-perfect setting. Called on the core's thread.
 * @param onDacDetached called on the main thread when the tracked DAC is removed.
 */
class UsbAudioRouter(
    context: Context,
    private val bitPerfectEnabled: () -> Boolean,
    private val onDacDetached: () -> Unit,
) : OutputRouter {
    private val audioManager = context.getSystemService(AudioManager::class.java)
    private val lock = Any()

    private val _dac = MutableStateFlow<UsbDac?>(null)

    /** The connected USB DAC, or null when none is connected. */
    val dac: StateFlow<UsbDac?> = _dac.asStateFlow()

    private val _signalPath = MutableStateFlow<SignalPathInfo?>(null)

    /** The result of the last stream open, or null before the first one. */
    val signalPath: StateFlow<SignalPathInfo?> = _signalPath.asStateFlow()

    /** The mixer attributes this router set on the DAC, or null. Guarded by [lock]. */
    private var applied: AppliedMixer? = null

    /**
     * Until this [SystemClock.elapsedRealtime] time, plans ask the engine to hold a reopened stream
     * stopped: the DAC has just gone and the app is pausing, so a reopen on the phone speaker must
     * not start playing. Guarded by [lock].
     */
    private var holdUntilMs = 0L

    private val deviceCallback = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>) {
            refresh()
        }

        override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>) {
            refresh()
        }
    }

    init {
        audioManager.registerAudioDeviceCallback(deviceCallback, Handler(Looper.getMainLooper()))
        refresh()
    }

    /**
     * Re-reads the connected USB output. When the tracked DAC is gone, forgets the mixer attributes
     * set for it, clears [dac] and calls [onDacDetached].
     */
    fun refresh() {
        val removed: UsbDac? = synchronized(lock) {
            val previous = _dac.value
            val current = findUsbOutput()
            _dac.value = current
            if (previous != null && previous.id != current?.id) {
                // The device is gone, so there is nothing to clear on it.
                applied = null
                startHoldLocked()
                previous
            } else {
                null
            }
        }
        if (removed != null) {
            Log.i(TAG, "USB DAC ${removed.name} removed")
            onDacDetached()
        }
    }

    /** Stops listening for device changes. */
    fun release() {
        audioManager.unregisterAudioDeviceCallback(deviceCallback)
    }

    override fun plan(request: RouteRequestInfo): RoutePlanInfo {
        val fallback = RoutePlanInfo(
            deviceId = null,
            sampleFormat = request.requested,
            exclusive = request.exclusive,
            bitPerfect = false,
            hold = false,
        )
        return try {
            synchronized(lock) {
                val plan = choosePlan(request, fallback)
                if (SystemClock.elapsedRealtime() < holdUntilMs) {
                    Log.i(TAG, "plan: hold (USB DAC just removed, pausing)")
                    plan.copy(hold = true)
                } else {
                    plan
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "Route planning failed, using the default output", e)
            fallback
        }
    }

    override fun opened(path: SignalPathInfo) {
        try {
            _signalPath.value = path
            val stream = path.stream
            Log.i(
                TAG,
                "opened: mode=${path.mode} " +
                    "stream=${stream?.sampleRate?.toString() ?: "none"} Hz " +
                    "${stream?.sampleFormat ?: "-"} " +
                    "${if (stream?.exclusive == true) "exclusive" else "shared"}",
            )
        } catch (e: Exception) {
            Log.w(TAG, "Could not record the signal path", e)
        }
    }

    /** Must be called with [lock] held. */
    private fun choosePlan(request: RouteRequestInfo, fallback: RoutePlanInfo): RoutePlanInfo {
        val enabled = bitPerfectEnabled()
        val dac = _dac.value
        if (!enabled || dac == null) {
            clearAppliedLocked()
            Log.i(TAG, "plan: mixer (bit-perfect ${if (enabled) "on" else "off"}, DAC ${dac?.name ?: "none"})")
            return fallback
        }
        val device = findDevice(dac.id)
        if (device == null) {
            // Gone before the device callback ran: the app is about to pause.
            startHoldLocked()
            clearAppliedLocked()
            Log.i(TAG, "plan: mixer (DAC ${dac.name} is no longer listed)")
            return fallback
        }

        val rate = request.sampleRate.toInt()
        val channels = request.channels.toInt()
        val candidates = audioManager.getSupportedMixerAttributes(device).filter { attrs ->
            attrs.mixerBehavior == AudioMixerAttributes.MIXER_BEHAVIOR_BIT_PERFECT &&
                attrs.format.sampleRate == rate &&
                attrs.format.channelCount == channels
        }
        val pick = preferredEncodings(request.sourceBits).firstNotNullOfOrNull { (encoding, format) ->
            candidates.firstOrNull { it.format.encoding == encoding }?.let { it to format }
        }
        if (pick == null) {
            clearAppliedLocked()
            Log.i(TAG, "plan: mixer (${dac.name} has no bit-perfect ${rate} Hz, ${channels} ch)")
            return fallback
        }
        val (attrs, sampleFormat) = pick

        val key = AppliedMixer(dac.id, attrs.format.encoding, rate, channels)
        if (applied != key) {
            val ok = audioManager.setPreferredMixerAttributes(MEDIA_ATTRIBUTES, device, attrs)
            if (!ok) {
                clearAppliedLocked()
                Log.w(TAG, "plan: ${dac.name} refused the bit-perfect format, using the mixer")
                return fallback
            }
            applied = key
        }
        Log.i(TAG, "plan: bit-perfect to ${dac.name} at ${rate} Hz, ${channels} ch, ${sampleFormat}")
        return RoutePlanInfo(
            deviceId = dac.id,
            sampleFormat = sampleFormat,
            exclusive = false,
            bitPerfect = true,
            hold = false,
        )
    }

    /** Starts the hold window after the DAC is removed. Must be called with [lock] held. */
    private fun startHoldLocked() {
        holdUntilMs = SystemClock.elapsedRealtime() + HOLD_MS
    }

    /** Clears the mixer attributes this router set, if any. Must be called with [lock] held. */
    private fun clearAppliedLocked() {
        val key = applied ?: return
        applied = null
        val device = findDevice(key.deviceId) ?: return
        val cleared = audioManager.clearPreferredMixerAttributes(MEDIA_ATTRIBUTES, device)
        Log.i(TAG, "cleared the bit-perfect format on DAC id ${key.deviceId} (ok=$cleared)")
    }

    private fun findDevice(id: Int): AudioDeviceInfo? =
        audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS).firstOrNull { it.id == id }

    private fun findUsbOutput(): UsbDac? =
        audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            .firstOrNull { device ->
                device.isSink &&
                    (device.type == AudioDeviceInfo.TYPE_USB_DEVICE ||
                        device.type == AudioDeviceInfo.TYPE_USB_HEADSET)
            }
            ?.let { device ->
                val name = device.productName.toString().trim().ifEmpty { "USB audio" }
                UsbDac(device.id, name)
            }

    /** The mixer attributes last set on a DAC. Two opens with the same key need no new call. */
    private data class AppliedMixer(val deviceId: Int, val encoding: Int, val sampleRate: Int, val channels: Int)

    private companion object {
        /** How long after a DAC removal reopens stay held; the pause lands well within it. */
        const val HOLD_MS = 3_000L

        /** The engine's AAudio stream uses usage MEDIA, so the preference must match it. */
        val MEDIA_ATTRIBUTES: AudioAttributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_MEDIA)
            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
            .build()

        /**
         * Lossless PCM encodings to try, best first, for a source of [sourceBits] bits. Each is
         * paired with the output format the core opens for it.
         */
        fun preferredEncodings(sourceBits: UInt?): List<Pair<Int, OutputSampleFormat>> = when {
            sourceBits != null && sourceBits <= 16u -> listOf(
                AudioFormat.ENCODING_PCM_16BIT to OutputSampleFormat.INT16,
                AudioFormat.ENCODING_PCM_24BIT_PACKED to OutputSampleFormat.INT24_PACKED,
                AudioFormat.ENCODING_PCM_32BIT to OutputSampleFormat.INT32,
            )
            sourceBits != null && sourceBits <= 24u -> listOf(
                AudioFormat.ENCODING_PCM_24BIT_PACKED to OutputSampleFormat.INT24_PACKED,
                AudioFormat.ENCODING_PCM_32BIT to OutputSampleFormat.INT32,
            )
            else -> listOf(AudioFormat.ENCODING_PCM_32BIT to OutputSampleFormat.INT32)
        }
    }
}
