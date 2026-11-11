package dev.constnishant.sonorus.ui

import androidx.annotation.StringRes
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material.icons.rounded.LibraryMusic
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavBackStackEntry
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.SmartPlaylistKind
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.player.MiniPlayer
import dev.constnishant.sonorus.ui.player.NowPlayingScreen
import dev.constnishant.sonorus.ui.player.showsPause
import dev.constnishant.sonorus.ui.screens.AlbumScreen
import dev.constnishant.sonorus.ui.screens.ArtistScreen
import dev.constnishant.sonorus.ui.screens.HomeScreen
import dev.constnishant.sonorus.ui.screens.LibraryScreen
import dev.constnishant.sonorus.ui.screens.PlaylistScreen
import dev.constnishant.sonorus.ui.screens.SmartPlaylistScreen
import dev.constnishant.sonorus.ui.screens.StatsScreen
import dev.constnishant.sonorus.ui.screens.SearchScreen
import dev.constnishant.sonorus.ui.settings.SettingsScreen
import dev.constnishant.sonorus.ui.uploads.UploadsScreen

private data class TopLevelTab(val route: String, @StringRes val labelRes: Int, val icon: ImageVector)

private val TopLevelTabs = listOf(
    TopLevelTab(Routes.HOME, R.string.nav_home, Icons.Rounded.Home),
    TopLevelTab(Routes.LIBRARY, R.string.nav_library, Icons.Rounded.LibraryMusic),
    TopLevelTab(Routes.SEARCH, R.string.nav_search, Icons.Rounded.Search),
)

/** Timing of screen-to-screen transitions. */
private fun <T> navMotion() = tween<T>(durationMillis = 250, easing = FastOutSlowInEasing)

/** Timing of Now Playing rising and falling. */
private fun <T> sheetMotion() = tween<T>(durationMillis = 350, easing = FastOutSlowInEasing)

/** Navigation arguments for the detail routes: a single non-null library id. */
private val IdArgs = listOf(navArgument(Routes.ARG_ID) { type = NavType.LongType })

private fun NavBackStackEntry.idArg(): Long =
    checkNotNull(arguments?.getLong(Routes.ARG_ID)) { "missing ${Routes.ARG_ID}" }

/** Navigation arguments for the smart playlist route: the [SmartPlaylistKind] enum name. */
private val KindArgs = listOf(navArgument(Routes.ARG_KIND) { type = NavType.StringType })

/** The smart playlist kind named by the route argument. */
private fun NavBackStackEntry.kindArg(): SmartPlaylistKind {
    val name = checkNotNull(arguments?.getString(Routes.ARG_KIND)) { "missing ${Routes.ARG_KIND}" }
    return SmartPlaylistKind.valueOf(name)
}

/**
 * The app shell: bottom navigation, mini player, snackbar host and the navigation graph. The
 * bottom bar and the mini player are hidden on the Now Playing and Settings screens.
 */
@Composable
fun SonorusRoot() {
    val repository = LocalContext.current.repository
    val navController = rememberNavController()
    val playback by repository.events.playback.collectAsStateWithLifecycle()
    // The last loaded playback, so the mini player keeps its content while it slides out.
    var shownPlayback by remember { mutableStateOf(playback) }
    LaunchedEffect(playback) {
        if (playback.track != null) shownPlayback = playback
    }
    val backStackEntry by navController.currentBackStackEntryAsState()
    val destination = backStackEntry?.destination
    val showChrome = destination?.route != Routes.NOW_PLAYING && destination?.route != Routes.SETTINGS
    val snackbarHostState = remember { SnackbarHostState() }
    // The tab the user is in. Detail screens are not nested under a tab in the graph, so the tab
    // is remembered from the last top-level destination instead of derived from the current one.
    var currentTab by rememberSaveable { mutableStateOf(Routes.HOME) }
    LaunchedEffect(destination?.route) {
        val route = destination?.route
        if (TopLevelTabs.any { it.route == route } && route != null) currentTab = route
    }

    LaunchedEffect(repository) {
        repository.events.errors.collect { snackbarHostState.showSnackbar(it) }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
        // Screens handle the status bar themselves; this Scaffold only reserves the bottom bar.
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            if (showChrome) {
                Column {
                    AnimatedVisibility(
                        visible = playback.track != null,
                        enter = slideInVertically { it } + fadeIn(),
                        exit = slideOutVertically { it } + fadeOut(),
                    ) {
                        MiniPlayer(
                            playback = shownPlayback,
                            onOpen = { navController.navigate(Routes.NOW_PLAYING) },
                            onPlayPause = {
                                val pause = playback.state.showsPause()
                                repository.command { if (pause) it.pause() else it.play() }
                            },
                            onNext = { repository.command { it.next() } },
                            onPrevious = { repository.command { it.previous() } },
                        )
                    }
                    NavigationBar {
                        TopLevelTabs.forEach { tab ->
                            NavigationBarItem(
                                selected = currentTab == tab.route,
                                onClick = {
                                    if (currentTab == tab.route) {
                                        // Re-selecting the current tab returns to its top screen.
                                        navController.popBackStack(tab.route, inclusive = false)
                                    } else {
                                        navController.navigate(tab.route) {
                                            popUpTo(Routes.HOME) { saveState = true }
                                            launchSingleTop = true
                                            restoreState = true
                                        }
                                    }
                                },
                                icon = { Icon(tab.icon, contentDescription = null) },
                                label = { Text(stringResource(tab.labelRes)) },
                            )
                        }
                    }
                }
            }
        },
    ) { innerPadding ->
        NavHost(
            navController = navController,
            startDestination = Routes.HOME,
            modifier = Modifier.padding(innerPadding).consumeWindowInsets(innerPadding),
            // Shared-axis style: the new screen slides in a little from the side while fading in.
            enterTransition = { slideInHorizontally(navMotion()) { it / 10 } + fadeIn(navMotion()) },
            exitTransition = { fadeOut(navMotion()) },
            popEnterTransition = { fadeIn(navMotion()) },
            popExitTransition = { slideOutHorizontally(navMotion()) { it / 10 } + fadeOut(navMotion()) },
        ) {
            composable(Routes.HOME) {
                HomeScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onOpenSettings = { navController.navigate(Routes.SETTINGS) },
                    onOpenStats = { navController.navigate(Routes.STATS) },
                )
            }
            composable(Routes.LIBRARY) {
                LibraryScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                    onPlaylist = { navController.navigate(Routes.playlist(it)) },
                    onSmartPlaylist = { navController.navigate(Routes.smartPlaylist(it)) },
                )
            }
            composable(Routes.SEARCH) {
                SearchScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                )
            }
            composable(Routes.ALBUM, arguments = IdArgs) { entry ->
                AlbumScreen(
                    albumId = entry.idArg(),
                    onArtist = { navController.navigate(Routes.artist(it)) },
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.ARTIST, arguments = IdArgs) { entry ->
                ArtistScreen(
                    artistId = entry.idArg(),
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.PLAYLIST, arguments = IdArgs) { entry ->
                PlaylistScreen(
                    playlistId = entry.idArg(),
                    onBack = { navController.popBackStack() },
                )
            }
            composable(
                Routes.NOW_PLAYING,
                // Now Playing rises from the mini player and drops back down when closed.
                enterTransition = { slideInVertically(sheetMotion()) { it } },
                exitTransition = { fadeOut(navMotion()) },
                popEnterTransition = { fadeIn(navMotion()) },
                popExitTransition = { slideOutVertically(sheetMotion()) { it } },
            ) {
                NowPlayingScreen(
                    onBack = { navController.popBackStack() },
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                )
            }
            composable(Routes.SMART, arguments = KindArgs) { entry ->
                SmartPlaylistScreen(
                    kind = entry.kindArg(),
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.STATS) {
                StatsScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.SETTINGS) {
                SettingsScreen(
                    onBack = { navController.popBackStack() },
                    onUploads = { navController.navigate(Routes.UPLOADS) },
                )
            }
            composable(Routes.UPLOADS) {
                UploadsScreen(onBack = { navController.popBackStack() })
            }
        }
    }
}
