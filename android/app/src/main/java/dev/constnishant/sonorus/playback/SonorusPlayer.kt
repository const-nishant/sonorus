package dev.constnishant.sonorus.playback

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.Player
import androidx.media3.common.SimpleBasePlayer
import androidx.media3.common.SimpleBasePlayer.MediaItemData
import androidx.media3.common.SimpleBasePlayer.PositionSupplier
import androidx.media3.common.SimpleBasePlayer.State
import androidx.media3.common.util.UnstableApi
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import dev.constnishant.sonorus.core.PlaybackState
import dev.constnishant.sonorus.core.QueueItem
import dev.constnishant.sonorus.core.RepeatMode
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.PlaybackUi
import dev.constnishant.sonorus.data.SonorusRepository
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import kotlin.math.abs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val TAG = "Sonorus"

/** Covers kept in memory. Each is at most about [MAX_COVER_PX] pixels on its longest side. */
private const val COVER_CACHE_SIZE = 8

/** Covers at or below this size are passed on as they are; larger ones are downscaled. */
private const val LARGE_COVER_BYTES = 300L * 1024

/** Longest side of a downscaled cover, in pixels. */
private const val MAX_COVER_PX = 512

/**
 * Media3 player backed by the Rust core. The session, notification and lock screen read their
 * state from here; their commands are sent to the core through [repository].
 *
 * The core's playback state is mirrored from [SonorusRepository.events]. [getState] runs on the
 * main thread, so it never reads files or calls the core: track metadata and cover bytes that are
 * not known yet are loaded on [Dispatchers.IO] and the state is invalidated when they arrive.
 */
/** A position tick further than this from the extrapolated position is published (seek, stall). */
private const val POSITION_DRIFT_MS = 1_000L

@UnstableApi
class SonorusPlayer(
    private val repository: SonorusRepository,
    context: Context,
) : SimpleBasePlayer(Looper.getMainLooper()) {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** Latest core playback state. Written and read on the main thread. */
    private var ui = PlaybackUi()

    /** Position and [SystemClock.elapsedRealtime] of the last state published to controllers. */
    private var publishedPositionMs = 0L
    private var publishedAtMs = 0L

    /** Track info by track id. A null value means the lookup failed. Main thread only. */
    private val tracks = HashMap<Long, TrackInfo?>()

    /** Track ids with a lookup in flight. Main thread only. */
    private val tracksLoading = HashSet<Long>()

    /**
     * Cover bytes by cover path, least recently used first. A null value means the cover could not
     * be read. Main thread only.
     */
    private val covers = object : LinkedHashMap<String, ByteArray?>(COVER_CACHE_SIZE, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, ByteArray?>?): Boolean =
            size > COVER_CACHE_SIZE
    }

    /** Cover paths with a load in flight. Main thread only. */
    private val coversLoading = HashSet<String>()

    init {
        scope.launch {
            repository.events.playback.collect { next ->
                val previous = ui
                ui = next
                val now = SystemClock.elapsedRealtime()
                // Position ticks arrive every 250 ms. While playing, controllers extrapolate from
                // the last published position, so a tick that stays on that line (no seek, no
                // stall) needs no state rebuild; rebuilding the whole queue 4x/s is wasted work.
                if (next.state == PlaybackState.PLAYING &&
                    next.copy(positionMs = 0) == previous.copy(positionMs = 0)
                ) {
                    val expected = publishedPositionMs + (now - publishedAtMs)
                    if (abs(next.positionMs - expected) < POSITION_DRIFT_MS) return@collect
                }
                publishedPositionMs = next.positionMs
                publishedAtMs = now
                invalidateState()
            }
        }
    }

    override fun getState(): State {
        val current = ui
        val queue = current.queue
        val track = current.track
        val playing = current.state.wantsPlay()

        requestMissingTracks(queue)
        track?.coverPath?.let { requestCover(it) }

        val playlist = queue.map { mediaItemData(it, current) }
        val index = queue.indexOfFirst { it.entryId == current.entryId }.takeIf { it >= 0 } ?: 0

        val position = if (current.state == PlaybackState.PLAYING) {
            PositionSupplier.getExtrapolating(current.positionMs, 1f)
        } else {
            PositionSupplier.getConstant(current.positionMs)
        }

        return State.Builder()
            .setAvailableCommands(availableCommands(loaded = track != null, hasQueue = queue.isNotEmpty()))
            .setPlayWhenReady(playing, Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST)
            .setPlaybackState(playbackState(current))
            .setPlaylist(playlist)
            .setCurrentMediaItemIndex(index)
            .setContentPositionMs(position)
            .setShuffleModeEnabled(current.shuffle)
            .setRepeatMode(
                when (current.repeat) {
                    RepeatMode.OFF -> Player.REPEAT_MODE_OFF
                    RepeatMode.ALL -> Player.REPEAT_MODE_ALL
                    RepeatMode.ONE -> Player.REPEAT_MODE_ONE
                },
            )
            .build()
    }

    override fun handleSetPlayWhenReady(playWhenReady: Boolean): ListenableFuture<*> {
        repository.command { if (playWhenReady) it.play() else it.pause() }
        return Futures.immediateVoidFuture()
    }

    override fun handleStop(): ListenableFuture<*> {
        // The queue is kept, so this only pauses; the core's stop would clear the current track.
        repository.command { it.pause() }
        return Futures.immediateVoidFuture()
    }

    override fun handleSeek(mediaItemIndex: Int, positionMs: Long, seekCommand: Int): ListenableFuture<*> {
        when (seekCommand) {
            Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM ->
                repository.command { it.next() }
            // The core decides between restarting the track and going to the previous entry.
            Player.COMMAND_SEEK_TO_PREVIOUS, Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM ->
                repository.command { it.previous() }
            Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM -> {
                val target = positionMs.coerceAtLeast(0L).toULong()
                repository.command { it.seek(target) }
            }
            Player.COMMAND_SEEK_TO_MEDIA_ITEM -> {
                // Jumps start the entry from its beginning; the position is not applied.
                val entryId = ui.queue.getOrNull(mediaItemIndex)?.entryId
                if (entryId != null) repository.command { it.jumpTo(entryId) }
            }
            else -> Log.w(TAG, "Unsupported seek command $seekCommand")
        }
        return Futures.immediateVoidFuture()
    }

    override fun handleSetShuffleModeEnabled(shuffleModeEnabled: Boolean): ListenableFuture<*> {
        repository.command { it.setShuffle(shuffleModeEnabled) }
        return Futures.immediateVoidFuture()
    }

    override fun handleSetRepeatMode(repeatMode: Int): ListenableFuture<*> {
        val mode = when (repeatMode) {
            Player.REPEAT_MODE_ALL -> RepeatMode.ALL
            Player.REPEAT_MODE_ONE -> RepeatMode.ONE
            else -> RepeatMode.OFF
        }
        repository.command { it.setRepeat(mode) }
        return Futures.immediateVoidFuture()
    }

    override fun handleRelease(): ListenableFuture<*> {
        scope.cancel()
        return Futures.immediateVoidFuture()
    }

    private fun availableCommands(loaded: Boolean, hasQueue: Boolean): Player.Commands {
        val builder = Player.Commands.Builder().addAll(
            Player.COMMAND_PLAY_PAUSE,
            Player.COMMAND_STOP,
            Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM,
            Player.COMMAND_GET_CURRENT_MEDIA_ITEM,
            Player.COMMAND_GET_TIMELINE,
            Player.COMMAND_GET_METADATA,
            Player.COMMAND_SET_SHUFFLE_MODE,
            Player.COMMAND_SET_REPEAT_MODE,
        )
        if (loaded) {
            builder.addAll(
                Player.COMMAND_SEEK_TO_NEXT,
                Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM,
                Player.COMMAND_SEEK_TO_PREVIOUS,
                Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM,
            )
        }
        if (hasQueue) builder.add(Player.COMMAND_SEEK_TO_MEDIA_ITEM)
        return builder.build()
    }

    private fun playbackState(current: PlaybackUi): Int {
        if (current.track == null || current.queue.isEmpty()) return Player.STATE_IDLE
        return when (current.state) {
            PlaybackState.IDLE -> Player.STATE_IDLE
            PlaybackState.LOADING, PlaybackState.BUFFERING -> Player.STATE_BUFFERING
            PlaybackState.PLAYING, PlaybackState.PAUSED -> Player.STATE_READY
        }
    }

    private fun mediaItemData(item: QueueItem, current: PlaybackUi): MediaItemData {
        val track = if (current.track?.id == item.trackId) current.track else tracks[item.trackId]
        val durationMs = (if (item.entryId == current.entryId) current.durationMs else null)
            ?: track?.durationMs?.toLong()
        val metadata = MediaMetadata.Builder()
            .setTitle(track?.title)
            .setArtist(track?.artist)
            .setAlbumTitle(track?.album)
        val cover = track?.coverPath?.let { covers[it] }
        if (cover != null) metadata.setArtworkData(cover, MediaMetadata.PICTURE_TYPE_FRONT_COVER)
        val mediaItem = MediaItem.Builder()
            .setMediaId(item.entryId.toString())
            .setMediaMetadata(metadata.build())
            .build()
        return MediaItemData.Builder(item.entryId)
            .setMediaItem(mediaItem)
            .setDurationUs(durationMs?.let { it * 1000L } ?: C.TIME_UNSET)
            .setIsSeekable(true)
            .build()
    }

    /** Looks up the tracks of [queue] that are not known yet, in one batch on IO. */
    private fun requestMissingTracks(queue: List<QueueItem>) {
        val known = ui.track?.id
        val missing = queue.map { it.trackId }.distinct()
            .filter { it != known && !tracks.containsKey(it) && it !in tracksLoading }
        if (missing.isEmpty()) return
        tracksLoading += missing
        scope.launch {
            val loaded = repository.call { core ->
                missing.associateWith { id ->
                    try {
                        core.track(id)
                    } catch (e: CoreException) {
                        null
                    }
                }
            }
            tracksLoading.removeAll(missing.toSet())
            tracks.putAll(loaded)
            invalidateState()
        }
    }

    /** Loads the cover at [path] on IO, unless it is cached or already loading. */
    private fun requestCover(path: String) {
        if (covers.containsKey(path) || path in coversLoading) return
        coversLoading += path
        scope.launch {
            val bytes = withContext(Dispatchers.IO) { readCover(path) }
            coversLoading.remove(path)
            covers[path] = bytes
            invalidateState()
        }
    }

    private companion object {
        /** Reads the cover at [path] as bytes for the session. Runs on IO. Returns null on failure. */
        fun readCover(path: String): ByteArray? {
            val file = File(path)
            if (!file.isFile) return null
            return try {
                if (file.length() <= LARGE_COVER_BYTES) file.readBytes() else downscale(file)
            } catch (e: IOException) {
                Log.w(TAG, "Could not read cover $path", e)
                null
            }
        }

        /** Decodes [file] at a reduced sample size and re-encodes it as JPEG. */
        fun downscale(file: File): ByteArray? {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeFile(file.path, bounds)
            val largest = maxOf(bounds.outWidth, bounds.outHeight)
            if (largest <= 0) return null
            var sample = 1
            while (largest / sample > MAX_COVER_PX) sample *= 2
            val options = BitmapFactory.Options().apply { inSampleSize = sample }
            val bitmap = BitmapFactory.decodeFile(file.path, options) ?: return null
            return try {
                ByteArrayOutputStream().use { out ->
                    bitmap.compress(Bitmap.CompressFormat.JPEG, 90, out)
                    out.toByteArray()
                }
            } finally {
                bitmap.recycle()
            }
        }
    }
}

/** True while the core is playing or about to play: the player reports play-when-ready, and the focus guard holds focus. */
internal fun PlaybackState.wantsPlay(): Boolean = when (this) {
    PlaybackState.PLAYING, PlaybackState.LOADING, PlaybackState.BUFFERING -> true
    PlaybackState.IDLE, PlaybackState.PAUSED -> false
}
