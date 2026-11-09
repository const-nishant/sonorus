package dev.constnishant.sonorus.ui.player

import android.os.SystemClock
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.SleepTimerRequest
import dev.constnishant.sonorus.data.SleepTimerUi
import dev.constnishant.sonorus.ui.components.SonorusChip
import dev.constnishant.sonorus.ui.components.formatDuration
import dev.constnishant.sonorus.ui.theme.Dimens
import kotlinx.coroutines.delay

/** How often the countdown chip redraws, in milliseconds. */
private const val COUNTDOWN_TICK_MS = 1_000L

/**
 * The sleep timer picker: a bottom sheet with 15, 30 and 45 minutes, one hour and "End of song".
 * While a timer is [active] it also offers "Turn off timer". Tapping a row calls [onSelect] with
 * the request; the caller closes the sheet.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SleepTimerSheet(
    active: Boolean,
    onSelect: (SleepTimerRequest) -> Unit,
    onDismiss: () -> Unit,
) {
    val options = listOf(
        SleepTimerRequest.Minutes(15u) to pluralStringResource(R.plurals.sleep_timer_minutes, 15, 15),
        SleepTimerRequest.Minutes(30u) to pluralStringResource(R.plurals.sleep_timer_minutes, 30, 30),
        SleepTimerRequest.Minutes(45u) to pluralStringResource(R.plurals.sleep_timer_minutes, 45, 45),
        SleepTimerRequest.Minutes(60u) to stringResource(R.string.sleep_timer_one_hour),
        SleepTimerRequest.EndOfTrack to stringResource(R.string.sleep_timer_end_of_song),
    )

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(
            text = stringResource(R.string.sleep_timer_title),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(horizontal = Dimens.ScreenPadding, vertical = 8.dp),
        )
        options.forEach { (request, label) ->
            SheetRow(label = label, onClick = { onSelect(request) })
        }
        if (active) {
            SheetRow(
                label = stringResource(R.string.sleep_timer_off),
                onClick = { onSelect(SleepTimerRequest.Off) },
            )
        }
        Spacer(Modifier.height(Dimens.SectionSpacing))
    }
}

/** One tappable line of the sleep timer sheet, at least 48 dp tall. */
@Composable
private fun SheetRow(label: String, onClick: () -> Unit) {
    Text(
        text = label,
        style = MaterialTheme.typography.bodyLarge,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClick = onClick)
            .heightIn(min = 48.dp)
            .padding(horizontal = Dimens.ScreenPadding, vertical = 12.dp),
    )
}

/**
 * The countdown chip under the title: "Sleep in 12:34" while a timed sleep runs (m:ss, or h:mm:ss
 * from one hour), "Sleep after this song" for an end-of-track sleep. The countdown redraws once a
 * second until its deadline, then holds at 0:00 until the core reports the timer fired. [onClick]
 * opens the picker. Call it only while [timer] is active.
 */
@Composable
fun SleepChip(timer: SleepTimerUi, onClick: () -> Unit) {
    val deadline = timer.deadlineElapsedMs
    var now by remember { mutableLongStateOf(SystemClock.elapsedRealtime()) }
    LaunchedEffect(deadline) {
        now = SystemClock.elapsedRealtime()
        while (deadline != null && now < deadline) {
            delay(COUNTDOWN_TICK_MS)
            now = SystemClock.elapsedRealtime()
        }
    }
    val text = if (deadline != null) {
        stringResource(R.string.sleep_chip_in, formatDuration((deadline - now).coerceAtLeast(0L)))
    } else {
        stringResource(R.string.sleep_chip_end_of_song)
    }
    SonorusChip(text = text, onClick = onClick)
}
