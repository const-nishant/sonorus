package dev.constnishant.sonorus.data

import android.content.Context
import dev.constnishant.sonorus.SonorusApp

/** The app-wide [SonorusRepository]. Requires the app to be the process's `Application`. */
val Context.repository: SonorusRepository
    get() = (applicationContext as SonorusApp).repository
