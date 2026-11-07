package dev.constnishant.sonorus.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.theme.Dimens

/** Back arrow and page title at the top of every settings page. */
@Composable
internal fun SettingsTopBar(title: String, onBack: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 64.dp)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        BackButton(onClick = onBack)
        Spacer(Modifier.width(4.dp))
        Text(text = title, style = MaterialTheme.typography.titleLarge)
    }
}

/** A small accent label above a group of rows inside a page. */
@Composable
internal fun SettingsGroupLabel(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = Dimens.ScreenPadding, end = Dimens.ScreenPadding, top = 24.dp, bottom = 4.dp),
    )
}

/** Quiet explanatory text between rows. */
@Composable
internal fun SettingsNote(text: String, color: Color = MaterialTheme.colorScheme.onSurfaceVariant) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = color,
        modifier = Modifier.padding(horizontal = Dimens.ScreenPadding, vertical = 8.dp),
    )
}

/** The result of the last action on a page: the accent color on success, the error color on failure. */
@Composable
internal fun SettingsStatus(message: String?, failed: Boolean) {
    message ?: return
    SettingsNote(
        text = message,
        color = if (failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
    )
}

/**
 * One settings row: an optional leading [icon], a [title] and a one-line [summary]. With [onClick]
 * the whole row is a button; [trailing] sits at the end (a switch, a spinner).
 */
@Composable
internal fun SettingsRow(
    title: String,
    modifier: Modifier = Modifier,
    summary: String? = null,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    summaryLines: Int = 1,
    titleColor: Color = MaterialTheme.colorScheme.onSurface,
    onClick: (() -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    val clickModifier = if (onClick != null) {
        Modifier.clickable(enabled = enabled, role = Role.Button, onClick = onClick)
    } else {
        Modifier
    }
    Row(
        modifier = modifier
            .fillMaxWidth()
            .then(clickModifier)
            .heightIn(min = 64.dp)
            .padding(horizontal = Dimens.ScreenPadding, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Dimens.ScreenPadding),
    ) {
        val alpha = if (enabled) 1f else 0.38f
        if (icon != null) {
            Box(Modifier.size(24.dp), contentAlignment = Alignment.Center) {
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = alpha),
                )
            }
        }
        Column(Modifier.weight(1f)) {
            Text(
                text = title,
                style = MaterialTheme.typography.bodyLarge,
                color = titleColor.copy(alpha = alpha),
            )
            if (!summary.isNullOrEmpty()) {
                Text(
                    text = summary,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = alpha),
                    maxLines = summaryLines,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        trailing?.invoke()
    }
}

/** A row whose whole surface toggles [checked]. */
@Composable
internal fun SettingsSwitch(
    title: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    summary: String? = null,
) {
    SettingsRow(
        title = title,
        summary = summary,
        summaryLines = 3,
        modifier = Modifier.toggleable(value = checked, role = Role.Switch, onValueChange = onCheckedChange),
        trailing = { Switch(checked = checked, onCheckedChange = null) },
    )
}

/**
 * A row showing the label of the option equal to [selected]. Tapping it opens a dialog with one radio
 * button per option.
 */
@Composable
internal fun <T> SettingsChoice(
    title: String,
    options: List<Pair<T, String>>,
    selected: T,
    onSelect: (T) -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    SettingsRow(
        title = title,
        summary = options.firstOrNull { it.first == selected }?.second,
        onClick = { open = true },
    )
    if (open) {
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text(title) },
            text = {
                Column(Modifier.selectableGroup()) {
                    options.forEach { (value, label) ->
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(min = 48.dp)
                                .selectable(
                                    selected = value == selected,
                                    role = Role.RadioButton,
                                    onClick = {
                                        open = false
                                        onSelect(value)
                                    },
                                ),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = value == selected, onClick = null)
                            Spacer(Modifier.width(Dimens.ScreenPadding))
                            Text(label, style = MaterialTheme.typography.bodyLarge)
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { open = false }) { Text(stringResource(R.string.action_cancel)) }
            },
        )
    }
}

/** A small spinner for the trailing slot of a row whose action is running. */
@Composable
internal fun RowSpinner() {
    CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
}
