package dev.constnishant.sonorus.ui.player

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.core.OutputPathMode
import dev.constnishant.sonorus.core.SignalPathInfo
import dev.constnishant.sonorus.data.audio.UsbDac
import dev.constnishant.sonorus.ui.components.formatKhz
import dev.constnishant.sonorus.ui.components.label

/**
 * Chip showing the signal path mode of the last open: "Bit-perfect" in the HI-RES accent, or
 * "Exclusive" / "Mixer" in the muted color. [onClick] opens the details.
 */
@Composable
fun SignalPathChip(path: SignalPathInfo, onClick: () -> Unit) {
    val text = modeLabel(path.mode)
    val tint = if (path.mode == OutputPathMode.BIT_PERFECT) {
        MaterialTheme.colorScheme.tertiary
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(
        onClick = onClick,
        shape = RoundedCornerShape(50),
        color = Color.Transparent,
        contentColor = tint,
        border = BorderStroke(1.dp, tint),
    ) {
        Text(
            text = text,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
            style = MaterialTheme.typography.labelMedium,
        )
    }
}

/**
 * "Signal path" details: source, output stream, device, mode and any error. [dac] is the connected
 * USB DAC, if any, used to name the device.
 */
@Composable
fun SignalPathDialog(path: SignalPathInfo, dac: UsbDac?, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text("Close")
            }
        },
        title = { Text("Signal path") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                SignalRow(label = "Source", value = sourceText(path))
                SignalRow(label = "Output", value = outputText(path))
                SignalRow(label = "Device", value = deviceText(path, dac))
                SignalRow(
                    label = "Mode",
                    value = modeLabel(path.mode),
                    note = modeNote(path.mode),
                )
                path.error?.let { message ->
                    Text(
                        text = message,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
    )
}

/** One label and value line in the dialog, with an optional explanation under the value. */
@Composable
private fun SignalRow(label: String, value: String, note: String? = null) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.Top,
    ) {
        Text(
            text = label,
            modifier = Modifier.width(64.dp),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Start,
        )
        Column(Modifier.weight(1f)) {
            Text(text = value, style = MaterialTheme.typography.bodyMedium)
            note?.let {
                Text(
                    text = it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

private fun modeLabel(mode: OutputPathMode): String = when (mode) {
    OutputPathMode.BIT_PERFECT -> "Bit-perfect"
    OutputPathMode.EXCLUSIVE -> "Exclusive"
    OutputPathMode.MIXER -> "Mixer"
}

private fun modeNote(mode: OutputPathMode): String = when (mode) {
    OutputPathMode.BIT_PERFECT -> "Samples reach the DAC unchanged"
    OutputPathMode.EXCLUSIVE -> "Direct stream, the device format may still differ"
    OutputPathMode.MIXER -> "Android mixes and may resample to the device rate"
}

/** "24-bit / 96 kHz", or "Unknown bit depth / 44.1 kHz". */
private fun sourceText(path: SignalPathInfo): String {
    val bits = path.sourceBits?.let { "$it-bit" } ?: "Unknown bit depth"
    return "$bits / ${formatKhz(path.sourceRate)} kHz"
}

/** "96 kHz · 24-bit · Exclusive", or "Not open" when no stream is open. */
private fun outputText(path: SignalPathInfo): String {
    val stream = path.stream ?: return "Not open"
    val share = if (stream.exclusive) "Exclusive" else "Shared"
    return "${formatKhz(stream.sampleRate)} kHz · ${stream.sampleFormat.label()} · $share"
}

/**
 * "USB DAC name (id 42)", or "Phone speaker/default (id 7)". The id is left out when unknown.
 *
 * A default-route stream follows Android's media routing, which moves it to a USB DAC plugged in
 * after the open without a reopen, so the stream's device id can be stale. When the plan used the
 * default route and a DAC is connected, the DAC is named.
 */
private fun deviceText(path: SignalPathInfo, dac: UsbDac?): String {
    if (path.plan.deviceId == null && dac != null) {
        return "${dac.name} (id ${dac.id})"
    }
    val id = path.stream?.deviceId ?: path.plan.deviceId
    val name = dac?.takeIf { id != null && it.id == id }?.name ?: "Phone speaker/default"
    return if (id != null) "$name (id $id)" else name
}
