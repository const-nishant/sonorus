package dev.constnishant.sonorus.upload

import android.app.ForegroundServiceStartNotAllowedException
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import androidx.core.net.toUri
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import dev.constnishant.sonorus.MainActivity
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.UploadJobInfo
import dev.constnishant.sonorus.core.UploadState
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.data.userMessage
import java.io.IOException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch

private const val TAG = "Sonorus"
private const val CHANNEL_ID = "uploads"

/** The ongoing notification and the end-of-run notification share this id, so the latter replaces the former. */
private const val NOTIFICATION_ID = 1
private const val NOTIFY_INTERVAL_MS = 500L
private const val EXTRA_RETRY_JOB_ID = "retry_job_id"
private const val ACTION_CANCEL = "dev.constnishant.sonorus.upload.CANCEL"

/**
 * Uploads queued music to the bucket, one job at a time, in a foreground service of type dataSync.
 *
 * A single worker loop reads the upload queue and runs the oldest queued (or interrupted) job,
 * then the next, until nothing is left. Failed jobs are not retried by the loop; the user taps
 * Retry, which starts the service with [start]'s `retryJobId`. When the loop finds nothing to do,
 * the library is rescanned if anything was uploaded, a summary notification is shown and the
 * service stops.
 *
 * Start it only from the foreground: Android 14 refuses to start dataSync services from the
 * background.
 */
class UploadService : Service() {
    private val repository by lazy { applicationContext.repository }
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Guards the start-related fields below and the foreground notification. */
    private val lock = Any()

    /** Failed jobs the user asked to run again. Guarded by [lock]. */
    private val retryIds = mutableSetOf<Long>()

    /** True while the worker loop runs. Guarded by [lock]. */
    private var running = false

    /** True when a start arrived since the worker last read the queue. Guarded by [lock]. */
    private var kicked = false

    /** The id of the latest start, for `stopSelf(startId)`. Guarded by [lock]. */
    private var lastStartId = 0

    /** Set on Android 15's dataSync time limit: the worker takes no new job. */
    @Volatile private var stopRequested = false

    /** Jobs the worker has run in this service run. Only the worker touches it. */
    private val attempted = mutableSetOf<Long>()

    /** The job being run, for the notification. */
    @Volatile private var current: UploadJobInfo? = null

    /** 1-based number of the current job in this run, for the notification. */
    @Volatile private var position = 0

    /** Jobs this run will run, the current one included, for the notification. */
    @Volatile private var total = 0

    /** Last notification post time, guarded by [lock]. */
    private var lastPostMs = 0L

    /** Jobs that ended done in this run. Only the worker touches it. */
    private var doneCount = 0

    /** Jobs that ended failed in this run. Only the worker touches it. */
    private var failedCount = 0

    override fun onCreate() {
        super.onCreate()
        val channel = NotificationChannel(CHANNEL_ID, "Uploads", NotificationManager.IMPORTANCE_LOW)
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        try {
            startForeground(
                NOTIFICATION_ID,
                ongoingNotification(),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } catch (e: ForegroundServiceStartNotAllowedException) {
            // Started from the background. The queue waits for the next start from the app.
            Log.w(TAG, "upload service refused the foreground start: $e")
            stopSelf(startId)
            return START_NOT_STICKY
        }

        if (intent?.action == ACTION_CANCEL) {
            current?.let { repository.cancelUpload(it.id) }
            val idle = synchronized(lock) { !running }
            if (idle) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf(startId)
            }
            return START_NOT_STICKY
        }

        val retryId = intent?.takeIf { it.hasExtra(EXTRA_RETRY_JOB_ID) }
            ?.getLongExtra(EXTRA_RETRY_JOB_ID, 0L)
        val launch = synchronized(lock) {
            lastStartId = startId
            if (retryId != null) retryIds += retryId
            kicked = true
            if (running || stopRequested) {
                false
            } else {
                running = true
                true
            }
        }
        if (launch) {
            scope.launch { work() }
        } else if (stopRequested) {
            stopSelf(startId)
        }
        return START_NOT_STICKY
    }

    /** Android 15: the dataSync time limit is reached. Stop now; the queue resumes on the next start. */
    @RequiresApi(35)
    override fun onTimeout(startId: Int, fgsType: Int) {
        stopRequested = true
        stopSelf()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    /** The worker loop: runs jobs until the queue has nothing left, then finishes. */
    private suspend fun work() {
        val watch = scope.launch {
            repository.events.uploadProgress.collect { postProgress(force = false) }
        }
        try {
            while (true) {
                synchronized(lock) { kicked = false }
                val jobs = try {
                    repository.uploads()
                } catch (e: CoreException) {
                    e.userMessage("Could not read the upload queue.")
                    emptyList()
                }
                val next = if (stopRequested) null else pick(jobs)
                if (next != null) {
                    runJob(next, jobs)
                } else if (finish()) {
                    return
                }
            }
        } finally {
            watch.cancel()
        }
    }

    /**
     * The job to run next: a failed job the user asked to retry, else the oldest queued or
     * interrupted job that this run has not tried yet. Null when there is none.
     */
    private fun pick(jobs: List<UploadJobInfo>): UploadJobInfo? = synchronized(lock) {
        val retry = jobs.firstOrNull { it.state == UploadState.FAILED && it.id in retryIds }
        if (retry != null) {
            retryIds.remove(retry.id)
            return@synchronized retry
        }
        // No pending retry matches a failed job any more: the ids are stale.
        retryIds.clear()
        jobs
            .filter { (it.state == UploadState.QUEUED || it.state == UploadState.UPLOADING) && it.id !in attempted }
            .minByOrNull { it.id }
    }

    /** Runs one job and records how it ended. */
    private suspend fun runJob(job: UploadJobInfo, jobs: List<UploadJobInfo>) {
        attempted += job.id
        position += 1
        total = position + jobs.count {
            it.id != job.id &&
                it.id !in attempted &&
                (it.state == UploadState.QUEUED || it.state == UploadState.UPLOADING)
        }
        current = job
        postProgress(force = true)

        val fd = openSource(job)
        if (fd == null) {
            // The source is gone (deleted, or its permission was lost). Cancel the job; the user
            // can add the file again.
            Log.w(TAG, "upload job ${job.id}: the source is no longer readable, cancelling")
            try {
                repository.cancelUploadNow(job.id)
            } catch (e: CoreException) {
                e.userMessage("Could not cancel the upload.")
            }
            failedCount += 1
        } else {
            try {
                when (repository.runUpload(job.id, fd).state) {
                    UploadState.DONE -> doneCount += 1
                    UploadState.FAILED -> failedCount += 1
                    else -> Unit
                }
            } catch (e: CoreException.Busy) {
                // Another run holds this job (the service was restarted while it ran). That run
                // reports the outcome.
                Log.w(TAG, "upload job ${job.id} is already running")
            } catch (e: CoreException) {
                e.userMessage("Upload failed.")
                failedCount += 1
            }
        }
        current = null
        postProgress(force = true)
    }

    /**
     * Opens the source of [job] and returns its detached descriptor, which the core then owns.
     * Null when the source cannot be opened.
     */
    private suspend fun openSource(job: UploadJobInfo): Int? {
        val uri = job.sourceUri.toUri()
        return try {
            contentResolver.openFileDescriptor(uri, "r")?.detachFd()
        } catch (e: IOException) {
            null
        } catch (e: SecurityException) {
            null
        }
    }

    /**
     * Ends the run if nothing arrived since the queue was read. Returns false when a start came in
     * meanwhile, so the caller reads the queue again.
     */
    private fun finish(): Boolean = synchronized(lock) {
        if (kicked) {
            false
        } else {
            running = false
            current = null
            if (doneCount > 0) repository.startScan()
            val summaryPosted = postSummary()
            stopForeground(if (summaryPosted) STOP_FOREGROUND_DETACH else STOP_FOREGROUND_REMOVE)
            stopSelf(lastStartId)
            true
        }
    }

    /**
     * Shows the end-of-run notification when anything was uploaded or failed. Returns true when it
     * was posted, so the foreground notification is kept rather than removed.
     */
    private fun postSummary(): Boolean {
        if (doneCount + failedCount == 0) return false
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_upload_done)
            .setContentTitle("Upload finished: $doneCount uploaded, $failedCount failed")
            .setContentIntent(openAppIntent())
            .setAutoCancel(true)
            .build()
        notify(notification)
        return true
    }

    /**
     * Refreshes the ongoing notification. Posts at most twice a second unless [force] is set.
     * Does nothing once the run has finished.
     */
    private fun postProgress(force: Boolean) {
        synchronized(lock) {
            if (!running) return
            val now = SystemClock.elapsedRealtime()
            if (!force && now - lastPostMs < NOTIFY_INTERVAL_MS) return
            lastPostMs = now
            notify(ongoingNotification())
        }
    }

    private fun notify(notification: Notification) {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification)
    }

    private fun ongoingNotification(): Notification {
        val job = current
        val builder = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle("Uploading music")
            .setContentText(
                if (job == null) "Preparing uploads" else "$position of $total · ${job.displayName}",
            )
            .setContentIntent(openAppIntent())
            .addAction(0, "Cancel", cancelIntent())
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
        val fraction = job?.let(::progressFraction)
        if (fraction != null) {
            builder.setProgress(100, (fraction * 100).toInt(), false)
        } else {
            builder.setProgress(0, 0, true)
        }
        return builder.build()
    }

    /** Share of [job] stored so far, from the latest progress event, else from the job; null when unknown. */
    private fun progressFraction(job: UploadJobInfo): Double? {
        val progress = repository.events.uploadProgress.value[job.id]
        if (progress != null) {
            val (done, size) = progress
            return if (size > 0UL) done.toDouble() / size.toDouble() else null
        }
        return if (job.size > 0UL) job.bytesDone.toDouble() / job.size.toDouble() else null
    }

    private fun openAppIntent(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE,
    )

    private fun cancelIntent(): PendingIntent = PendingIntent.getService(
        this,
        1,
        Intent(this, UploadService::class.java).setAction(ACTION_CANCEL),
        PendingIntent.FLAG_IMMUTABLE,
    )

    companion object {
        /**
         * Starts the upload worker, or hands it new work if it is running. Call this only from the
         * foreground. [retryJobId] asks for a failed job to be run again. A start the system refuses
         * is logged; the queue then waits for the next start from the app.
         */
        fun start(context: Context, retryJobId: Long? = null) {
            val intent = Intent(context, UploadService::class.java)
            retryJobId?.let { intent.putExtra(EXTRA_RETRY_JOB_ID, it) }
            try {
                ContextCompat.startForegroundService(context, intent)
            } catch (e: ForegroundServiceStartNotAllowedException) {
                Log.w(TAG, "upload service not started from the background: $e")
            }
        }
    }
}
