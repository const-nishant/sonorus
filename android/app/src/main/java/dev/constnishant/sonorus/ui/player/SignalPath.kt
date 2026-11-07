package dev.constnishant.sonorus.ui.player

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.width
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.OutputPathMode
import dev.constnishant.sonorus.core.SignalPathInfo
import dev.constnishant.sonorus.data.audio.UsbDac
import dev.constnishant.sonorus.ui.components.SonorusChip
import dev.constnishant.sonorus.ui.components.formatKhz
import dev.constnishant.sonorus.ui.components.label

/**
 * Chip showing the signal path mode of the last open: "Bit-perfect" in the HI-RES accent, or
 * "Exclusive" / "Mixer" in the muted color. [onClick] opens the details.
 */
@Composable
fun SignalPathChip(path: SignalPathInfo, onClick: () -> Unit) {
    SonorusChip(
        text = modeLabel(path.mode),
        accent = path.mode == OutputPathMode.BIT_PERFECT,
        onClick = onClick,
    )
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
                Text(stringResource(R.string.action_close))
            }
        },
        title = { Text(stringResource(R.string.signal_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                SignalRow(label = stringResource(R.string.signal_source), value = sourceText(path))
                SignalRow(label = stringResource(R.string.signal_output), value = outputText(path))
                SignalRow(label = stringResource(R.string.signal_device), value = deviceText(path, dac))
                SignalRow(
                    label = stringResource(R.string.signal_mode),
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

@Composable
private fun modeLabel(mode: OutputPathMode): String = stringResource(
    when (mode) {
        OutputPathMode.BIT_PERFECT -> R.string.signal_mode_bit_perfect
        OutputPathMode.EXCLUSIVE -> R.string.signal_mode_exclusive
        OutputPathMode.MIXER -> R.string.signal_mode_mixer
    },
)

@Composable
private fun modeNote(mode: OutputPathMode): String = stringResource(
    when (mode) {
        OutputPathMode.BIT_PERFECT -> R.string.signal_note_bit_perfect
        OutputPathMode.EXCLUSIVE -> R.string.signal_note_exclusive
        OutputPathMode.MIXER -> R.string.signal_note_mixer
    },
)

/** "24-bit / 96 kHz", or "Unknown bit depth / 44.1 kHz". */
@Composable
private fun sourceText(path: SignalPathInfo): String {
    val bits = path.sourceBits?.let { stringResource(R.string.signal_sample_bits, it) }
        ?: stringResource(R.string.signal_unknown_depth)
    return stringResource(R.string.signal_source_line, bits, stringResource(R.string.khz_unit, formatKhz(path.sourceRate)))
}

/** "96 kHz · 24-bit · Exclusive", or "Not open" when no stream is open. */
@Composable
private fun outputText(path: SignalPathInfo): String {
    val stream = path.stream ?: return stringResource(R.string.signal_not_open)
    val share = if (stream.exclusive) {
        stringResource(R.string.signal_mode_exclusive)
    } else {
        stringResource(R.string.signal_mode_shared)
    }
    val khz = stringResource(R.string.khz_unit, formatKhz(stream.sampleRate))
    return khz + " · " + stream.sampleFormat.label(LocalContext.current) + " · " + share
}

/**
 * "USB DAC name (id 42)", or "Phone speaker/default (id 7)". The id is left out when unknown.
 *
 * A default-route stream follows Android's media routing, which moves it to a USB DAC plugged in
 * after the open without a reopen, so the stream's device id can be stale. When the plan used the
 * default route and a DAC is connected, the DAC is named.
 */
@Composable
private fun deviceText(path: SignalPathInfo, dac: UsbDac?): String {
    if (path.plan.deviceId == null && dac != null) {
        return stringResource(R.string.signal_device_with_id, dac.name, dac.id)
    }
    val id = path.stream?.deviceId ?: path.plan.deviceId
    val name = dac?.takeIf { id != null && it.id == id }?.name
        ?: stringResource(R.string.signal_device_phone)
    return if (id != null) stringResource(R.string.signal_device_with_id, name, id) else name
}
