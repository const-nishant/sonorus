package dev.constnishant.sonorus

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import dev.constnishant.sonorus.ui.SonorusRoot
import dev.constnishant.sonorus.ui.theme.SonorusTheme

/** Entry activity. Hosts the Sonorus navigation shell. */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            SonorusTheme {
                SonorusRoot()
            }
        }
    }
}
