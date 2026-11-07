package dev.constnishant.sonorus

import android.app.Application
import dev.constnishant.sonorus.core.initLogging
import dev.constnishant.sonorus.data.SonorusRepository

/** Application entry point. Owns the [SonorusRepository] shared by all screens. */
class SonorusApp : Application() {
    /** The app-wide repository. Created in [onCreate]; use the `Context.repository` accessor. */
    lateinit var repository: SonorusRepository

    override fun onCreate() {
        super.onCreate()
        initLogging()
        repository = SonorusRepository(this)
    }
}
