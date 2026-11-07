package dev.constnishant.sonorus.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.util.Log

private const val TAG = "Sonorus"

/**
 * Watches the default network and reports whether the device is connected and whether that
 * connection is metered (mobile data). [onChanged] is called on a system thread, only when the
 * (connected, metered) pair differs from the last one reported; the first state is always
 * reported.
 *
 * Connected means the default network has internet access and passed validation. Metered means
 * the network lacks NET_CAPABILITY_NOT_METERED.
 */
class NetworkMonitor(
    context: Context,
    private val onChanged: (connected: Boolean, metered: Boolean) -> Unit,
) {
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val lock = Any()

    /** The last pair reported, or null before the first report. Guarded by [lock]. */
    private var last: Pair<Boolean, Boolean>? = null

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            report(capabilities)
        }

        override fun onLost(network: Network) {
            // With a default-network callback, the lost network may already be replaced by a
            // new default that reported first. Report the active network if there is one.
            val active = connectivity.activeNetwork
            report(active?.let { connectivity.getNetworkCapabilities(it) })
        }
    }

    /** Registers the default-network callback. Requires ACCESS_NETWORK_STATE. */
    fun start() {
        connectivity.registerDefaultNetworkCallback(callback)
    }

    private fun report(capabilities: NetworkCapabilities?) {
        val connected = capabilities != null &&
            capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
        val metered = capabilities != null &&
            !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
        val pair = connected to metered
        synchronized(lock) {
            if (pair == last) return
            last = pair
        }
        Log.i(TAG, "network: connected=$connected metered=$metered")
        onChanged(connected, metered)
    }
}
